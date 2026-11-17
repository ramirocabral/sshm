//! `~/.ssh/config` importer: one host per concrete alias, resolved with OpenSSH's first-match-wins rules.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::util::{clean_segment, port_opt, wildcard_match};
use super::{ImportReport, ImportedHost};
use crate::paths;
use crate::store::hosts::{Host, SecretSource};

const FOLDER: &str = "ssh_config";
/// Same limit as OpenSSH's READCONF_MAX_DEPTH.
const MAX_DEPTH: usize = 16;

pub(super) fn import(path: &Path) -> Result<ImportReport> {
    let base = if path.starts_with("/etc/ssh") {
        PathBuf::from("/etc/ssh")
    } else {
        paths::home().join(".ssh")
    };
    parse(path, &base)
}

struct Block {
    patterns: Vec<String>,
    is_match: bool,
    options: Vec<(String, String)>,
}

struct Parser<'a> {
    base: &'a Path,
    blocks: Vec<Block>,
    aliases: Vec<String>,
    stack: Vec<PathBuf>,
    warnings: Vec<String>,
}

/// `base` is where relative `Include` paths resolve (~/.ssh for user configs).
pub(super) fn parse(path: &Path, base: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let global = Block {
        patterns: vec!["*".into()],
        is_match: false,
        options: Vec::new(),
    };
    let mut p = Parser {
        base,
        blocks: vec![global],
        aliases: Vec::new(),
        stack: vec![path.to_path_buf()],
        warnings: Vec::new(),
    };
    p.feed(&text, path);

    let mut report = ImportReport {
        hosts: Vec::new(),
        warnings: std::mem::take(&mut p.warnings),
    };
    for alias in &p.aliases {
        let mut opts: HashMap<&str, &str> = HashMap::new();
        for b in p
            .blocks
            .iter()
            .filter(|b| !b.is_match && host_matches(alias, &b.patterns))
        {
            for (k, v) in &b.options {
                opts.entry(k.as_str()).or_insert(v.as_str());
            }
        }
        let port = opts.get("port").and_then(|v| match v.parse::<u16>() {
            Ok(p) => port_opt(p),
            Err(_) => {
                report
                    .warnings
                    .push(format!("ssh_config: '{alias}' has an invalid port '{v}'"));
                None
            }
        });
        let not_none = |key: &str| {
            opts.get(key)
                .filter(|v| !v.eq_ignore_ascii_case("none"))
                .map(|v| v.to_string())
        };
        let host = Host {
            name: clean_segment(alias),
            folder: FOLDER.into(),
            host: alias.clone(),
            port,
            user: opts.get("user").map(|v| v.to_string()),
            identity_file: not_none("identityfile"),
            jump: not_none("proxyjump"),
            password: SecretSource::None,
            root_password: SecretSource::None,
            ..Host::default()
        };
        report.hosts.push(ImportedHost::new(host));
    }
    Ok(report)
}

impl Parser<'_> {
    fn feed(&mut self, text: &str, file: &Path) {
        for (n, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (keyword, rest) = split_keyword(line);
            let Some(args) = split_args(rest) else {
                self.warnings
                    .push(format!("ssh_config: {}:{}: unterminated quote", file.display(), n + 1));
                continue;
            };
            match keyword.to_ascii_lowercase().as_str() {
                "host" => {
                    for a in &args {
                        if !a.contains(['*', '?', '!']) && !self.aliases.contains(a) {
                            self.aliases.push(a.clone());
                        }
                    }
                    self.blocks.push(Block {
                        patterns: args,
                        is_match: false,
                        options: Vec::new(),
                    });
                }
                "match" => self.blocks.push(Block {
                    patterns: Vec::new(),
                    is_match: true,
                    options: Vec::new(),
                }),
                "include" => {
                    for arg in &args {
                        self.include(arg, file);
                    }
                }
                key => {
                    if let (Some(first), Some(block)) = (args.into_iter().next(), self.blocks.last_mut()) {
                        block.options.push((key.to_string(), first));
                    }
                }
            }
        }
    }

    fn include(&mut self, arg: &str, from: &Path) {
        if self.stack.len() > MAX_DEPTH {
            self.warnings.push(format!(
                "ssh_config: too many nested Include directives in {}",
                from.display()
            ));
            return;
        }
        let mut files = if let Some(rest) = arg.strip_prefix("~/") {
            self.expand(&paths::home().join(rest))
        } else if Path::new(arg).is_absolute() {
            self.expand(Path::new(arg))
        } else {
            let found = self.expand(&self.base.join(arg));
            match (found.is_empty(), from.parent()) {
                (true, Some(dir)) => self.expand(&dir.join(arg)),
                _ => found,
            }
        };
        if files.is_empty() {
            self.warnings
                .push(format!("ssh_config: Include {arg}: no matching files"));
        }
        for f in files.drain(..) {
            if self.stack.contains(&f) {
                self.warnings
                    .push(format!("ssh_config: recursive Include of {}, ignored", f.display()));
                continue;
            }
            match std::fs::read_to_string(&f) {
                Ok(text) => {
                    self.stack.push(f.clone());
                    self.feed(&text, &f);
                    self.stack.pop();
                }
                Err(e) => self
                    .warnings
                    .push(format!("ssh_config: could not read {}: {e}", f.display())),
            }
        }
    }

    /// Globs with `*`/`?` only in the file name, like `config.d/*.conf`.
    fn expand(&mut self, path: &Path) -> Vec<PathBuf> {
        let (Some(dir), Some(pattern)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            return Vec::new();
        };
        if dir.to_string_lossy().contains(['*', '?']) {
            self.warnings.push(format!(
                "ssh_config: Include {}: wildcards are only supported in the file name",
                path.display()
            ));
            return Vec::new();
        }
        if !pattern.contains(['*', '?']) {
            return if path.is_file() {
                vec![path.to_path_buf()]
            } else {
                Vec::new()
            };
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                (pattern.starts_with('.') || !name.starts_with('.')) && wildcard_match(pattern, &name)
            })
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        out.sort();
        out
    }
}

/// Splits `Keyword value`, `Keyword=value` and `Keyword = value`.
fn split_keyword(line: &str) -> (&str, &str) {
    let end = line.find(|c: char| c.is_whitespace() || c == '=').unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    (keyword, rest.strip_prefix('=').map(str::trim_start).unwrap_or(rest))
}

/// OpenSSH argv_split: quotes, a few backslash escapes, and `#` starting a word begins a comment.
fn split_args(s: &str) -> Option<Vec<String>> {
    let mut args = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        match chars.peek() {
            None | Some('#') => break,
            _ => {}
        }
        let mut arg = String::new();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            match (c, quote) {
                ('\\', q) => match chars.peek().copied() {
                    Some(n @ ('\'' | '"' | '\\')) => {
                        arg.push(n);
                        chars.next();
                    }
                    Some(' ') if q.is_none() => {
                        arg.push(' ');
                        chars.next();
                    }
                    _ => arg.push('\\'),
                },
                (c, None) if c.is_whitespace() => break,
                ('\'' | '"', None) => quote = Some(c),
                (c, Some(q)) if c == q => quote = None,
                (c, _) => arg.push(c),
            }
        }
        if quote.is_some() {
            return None;
        }
        args.push(arg);
    }
    Some(args)
}

fn host_matches(host: &str, patterns: &[String]) -> bool {
    let host = host.to_lowercase();
    let mut matched = false;
    for p in patterns {
        let p = p.to_lowercase();
        if let Some(neg) = p.strip_prefix('!') {
            if wildcard_match(neg, &host) {
                return false;
            }
        } else if wildcard_match(&p, &host) {
            matched = true;
        }
    }
    matched
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    #[test]
    fn tokenizer() {
        assert_eq!(split_keyword("Port=2222"), ("Port", "2222"));
        assert_eq!(split_keyword("Port = 2222"), ("Port", "2222"));
        assert_eq!(split_keyword("HostName\tx"), ("HostName", "x"));
        assert_eq!(
            split_args(r#"a "b c" 'd e' f\ g # comment"#).unwrap(),
            ["a", "b c", "d e", "f g"]
        );
        assert_eq!(split_args(r#"x"y z"w"#).unwrap(), ["xy zw"]);
        assert!(split_args("\"open").is_none());
        assert!(host_matches("Web1", &["web*".into(), "!web2".into()]));
        assert!(!host_matches("web2", &["web*".into(), "!web2".into()]));
    }

    #[test]
    fn parses_fixture() {
        let dir = fixture("ssh_config");
        let r = parse(&dir.join("config"), &dir).unwrap();
        let get = |n: &str| {
            &r.hosts
                .iter()
                .find(|h| h.host.name == n)
                .unwrap_or_else(|| panic!("missing {n}"))
                .host
        };
        let names: Vec<_> = r.hosts.iter().map(|h| h.host.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "work-gw", "work-app", "home-nas", "bastion", "web1", "web2", "db", "quoted", "eq", "bad-port"
            ]
        );
        assert!(
            r.hosts
                .iter()
                .all(|h| h.host.folder == "ssh_config" && h.host.password == SecretSource::None)
        );

        let web1 = get("web1");
        assert_eq!(web1.host, "web1");
        assert_eq!(web1.user.as_deref(), Some("deploy"));
        assert_eq!(web1.port, Some(2201));
        assert_eq!(web1.jump.as_deref(), Some("bastion"));
        assert_eq!(web1.identity_file.as_deref(), Some("~/.ssh/id_web"));
        assert_eq!(get("web2").port, None);
        assert_eq!(get("web2").user.as_deref(), Some("deploy"));

        assert_eq!(get("bastion").user.as_deref(), Some("admin"));
        assert_eq!(get("bastion").jump, None);
        assert_eq!(get("bastion").identity_file.as_deref(), Some("~/.ssh/id_ed25519"));
        assert_eq!(get("db").user.as_deref(), Some("postgres"));
        assert_eq!(get("db").identity_file, None);
        assert_eq!(get("quoted").identity_file.as_deref(), Some("~/.ssh/my key"));
        assert_eq!(get("eq").port, Some(2022));
        assert_eq!(get("eq").user.as_deref(), Some("root"));
        assert_eq!(get("work-app").jump.as_deref(), Some("work-gw"));
        assert_eq!(get("work-app").user.as_deref(), Some("ramiro"));
        assert_eq!(get("home-nas").port, Some(2200));

        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings.iter().any(|w| w.contains("missing-include")));
        assert!(r.warnings.iter().any(|w| w.contains("invalid port")));
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(parse(Path::new("/nonexistent/sshm/config"), Path::new("/nonexistent")).is_err());
    }
}
