//! Remmina profiles (`*.remmina`, a single file or a directory of them).
//!
//! Saved passwords are 3DES-CBC with zero padding (Remmina src/remmina_crypt.c): the base64 `secret` in
//! remmina.pref decodes to 32 bytes, the first 24 are the key and the last 8 the IV. A password of "." means it
//! lives in the system keyring (libsecret).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use des::TdesEde3;
use des::cipher::KeyInit;

use super::util::{
    IniSection, SkipCounter, cbc_decrypt, clean_folder, clean_segment, decode_text, find_section, non_empty, parse_ini,
    port_opt, printable_secret,
};
use super::{ImportReport, ImportedHost};
use crate::paths;
use crate::store::hosts::{Host, SecretSource};

const APP: &str = "Remmina";

pub(super) struct Secret {
    key: [u8; 24],
    iv: [u8; 8],
}

impl Secret {
    pub fn from_base64(b64: &str) -> Option<Self> {
        let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        Some(Secret {
            key: raw.get(..24)?.try_into().ok()?,
            iv: raw.get(24..32)?.try_into().ok()?,
        })
    }

    pub fn decrypt(&self, b64: &str) -> Option<String> {
        let data = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
        let cipher = TdesEde3::new_from_slice(&self.key).ok()?;
        let mut plain = cbc_decrypt(&cipher, &self.iv, &data)?;
        if let Some(nul) = plain.iter().position(|&b| b == 0) {
            plain.truncate(nul);
        }
        printable_secret(plain)
    }
}

fn default_pref() -> PathBuf {
    paths::config_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| paths::home().join(".config"))
        .join("remmina/remmina.pref")
}

fn load_secret(path: &Path) -> Result<Secret, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("could not read {}: {e}", path.display()))?;
    let sections = parse_ini(&decode_text(&bytes));
    let value = find_section(&sections, "remmina_pref")
        .and_then(|s| s.get("secret"))
        .ok_or_else(|| format!("{} has no 'secret' entry", path.display()))?;
    Secret::from_base64(value).ok_or_else(|| format!("invalid 'secret' in {}", path.display()))
}

/// GKeyFile escapes.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(o) => out.push(o),
            None => out.push('\\'),
        }
    }
    out
}

/// `host`, `host:port` or `[v6]:port`; more than one ':' without brackets is a bare IPv6 address.
fn split_server(server: &str) -> (String, Option<u16>) {
    if let Some(rest) = server.strip_prefix('[')
        && let Some((host, after)) = rest.split_once(']')
    {
        return (host.to_string(), after.strip_prefix(':').and_then(|p| p.parse().ok()));
    }
    match server.split_once(':') {
        Some((host, port)) if !port.contains(':') => (host.to_string(), port.parse().ok()),
        _ => (server.to_string(), None),
    }
}

fn profile_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "remmina") && p.is_file())
        .collect();
    files.sort();
    if files.is_empty() {
        bail!("no .remmina files in {}", path.display());
    }
    Ok(files)
}

#[derive(Default)]
struct Counters {
    keyring: usize,
    failed: usize,
    no_key: usize,
}

pub(super) fn import(path: &Path, pref: Option<&Path>) -> Result<ImportReport> {
    let files = profile_files(path)?;
    let pref_path = pref.map(Path::to_path_buf).unwrap_or_else(default_pref);
    let secret = load_secret(&pref_path);

    let mut report = ImportReport::default();
    let mut skipped = SkipCounter::default();
    let mut counters = Counters::default();
    for file in &files {
        let fname = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        let sections = parse_ini(&decode_text(&bytes));
        let Some(section) = find_section(&sections, "remmina") else {
            report
                .warnings
                .push(format!("{APP}: {fname} is not a Remmina profile, skipped"));
            continue;
        };
        let get = |k: &str| section.get(k).map(unescape).unwrap_or_default();
        let name = non_empty(&get("name")).unwrap_or_else(|| fname.trim_end_matches(".remmina").to_string());
        let protocol = get("protocol");
        if !protocol.eq_ignore_ascii_case("SSH") {
            skipped.add(if protocol.is_empty() { "unknown" } else { &protocol }, &name);
            continue;
        }
        let (addr, port) = split_server(get("server").trim());
        if addr.is_empty() {
            report.warnings.push(format!("{APP}: '{name}' has no server, skipped"));
            continue;
        }
        let password = read_secret(section, "password", &secret, &mut counters);
        let passphrase = read_secret(section, "ssh_passphrase", &secret, &mut counters);
        let ssh_auth: u8 = get("ssh_auth").trim().parse().unwrap_or(0);
        let proxy = get("ssh_proxycommand");
        let host = Host {
            name: clean_segment(&name),
            folder: clean_folder(&get("group")),
            host: addr,
            port: port.and_then(port_opt),
            user: non_empty(&get("username")).or_else(|| non_empty(&get("ssh_username"))),
            identity_file: non_empty(&get("ssh_privatekey")),
            ssh_args: if proxy.trim().is_empty() {
                Vec::new()
            } else {
                vec!["-o".into(), format!("ProxyCommand={}", proxy.trim())]
            },
            // ssh_auth: 0 = password, 5 = keyboard-interactive; the rest use keys, the agent or Kerberos.
            password: if matches!(ssh_auth, 0 | 5) {
                SecretSource::Vault
            } else {
                SecretSource::None
            },
            root_password: SecretSource::None,
            ..Host::default()
        };
        let mut item = ImportedHost::new(host);
        item.password = password;
        item.passphrase = passphrase;
        report.hosts.push(item);
    }

    skipped.warnings(APP, "SSH", &mut report.warnings);
    if counters.keyring > 0 {
        report.warnings.push(format!(
            "{APP}: {} password(s) are stored in the system keyring (libsecret) and cannot be read from the files",
            counters.keyring
        ));
    }
    if counters.no_key > 0 {
        let reason = secret.err().unwrap_or_default();
        report
            .warnings
            .push(format!("{APP}: {} password(s) not imported: {reason}", counters.no_key));
    }
    if counters.failed > 0 {
        report.warnings.push(format!(
            "{APP}: could not decrypt {} password(s) (remmina.pref from another installation?)",
            counters.failed
        ));
    }
    Ok(report)
}

fn read_secret(
    section: &IniSection,
    key: &str,
    secret: &Result<Secret, String>,
    counters: &mut Counters,
) -> Option<String> {
    let value = section.get(key)?.trim();
    match value {
        "" => None,
        "." => {
            counters.keyring += 1;
            None
        }
        enc => {
            let Ok(s) = secret else {
                counters.no_key += 1;
                return None;
            };
            let plain = s.decrypt(enc);
            counters.failed += usize::from(plain.is_none());
            plain.filter(|p| !p.is_empty())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::super::{ImportOptions, Source};
    use super::*;

    /// Published in StackOverflow question 59405976 ("create remmina connection file with saved password").
    #[test]
    fn published_vector() {
        let s = Secret::from_base64("9c+QkAz49dY5PIuphZjJWFQwq72tvcwD4/QXFtxIkNE=").unwrap();
        assert_eq!(s.decrypt("Rk/hk6y5CmE=").as_deref(), Some("333333"));
        assert_eq!(s.decrypt("Rk/hk6y5CmE"), None);
        assert!(Secret::from_base64("c2hvcnQ=").is_none());
    }

    #[test]
    fn servers() {
        assert_eq!(split_server("host"), ("host".into(), None));
        assert_eq!(split_server("host:2222"), ("host".into(), Some(2222)));
        assert_eq!(split_server("[fe80::1]:2200"), ("fe80::1".into(), Some(2200)));
        assert_eq!(split_server("fe80::1"), ("fe80::1".into(), None));
    }

    #[test]
    fn directory_of_profiles() {
        let opts = ImportOptions {
            passphrase: None,
            remmina_pref: Some(fixture("remmina/remmina.pref")),
        };
        let r = super::super::import(Source::Remmina, &fixture("remmina/profiles"), &opts).unwrap();
        let paths: Vec<_> = r.hosts.iter().map(|h| h.host.path()).collect();
        assert_eq!(
            paths,
            [
                "Clientes/ACME/web-01",
                "Lab/nas",
                "Clientes/ACME/db con espacios",
                "legacy"
            ]
        );

        let web = &r.hosts[0];
        assert_eq!(
            (web.host.host.as_str(), web.host.port, web.host.user.as_deref()),
            ("10.20.0.11", Some(2222), Some("deploy"))
        );
        assert_eq!(web.password.as_deref(), Some("Remmina-Pass#1"));
        assert_eq!(web.host.password, SecretSource::Vault);

        let nas = &r.hosts[1];
        assert_eq!(nas.host.identity_file.as_deref(), Some("/home/ramiro/.ssh/nas"));
        assert_eq!(nas.passphrase.as_deref(), Some("frase nas"));
        assert_eq!(nas.host.password, SecretSource::None);
        assert_eq!(nas.host.ssh_args, ["-o", "ProxyCommand=ssh -W %h:%p gw.lab"]);

        assert_eq!(r.hosts[2].host.host, "fe80::10");
        assert_eq!(r.hosts[2].password, None);
        assert_eq!(r.hosts[3].host.user.as_deref(), Some("viejo"));

        let w = r.warnings.join("\n");
        assert!(w.contains("skipped 1 RDP session(s) ('Escritorio')"), "{w}");
        assert!(w.contains("1 password(s) are stored in the system keyring"), "{w}");
        assert!(w.contains("local commands"), "{w}");
        assert!(!w.contains("decrypt"), "{w}");
    }

    #[test]
    fn single_file_and_missing_pref() {
        let file = fixture("remmina/profiles/acme-web.remmina");
        let r = import(&file, Some(&fixture("remmina/missing.pref"))).unwrap();
        assert_eq!(r.hosts.len(), 1);
        assert_eq!(r.hosts[0].password, None);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("1 password(s) not imported: could not read")),
            "{:?}",
            r.warnings
        );

        let r = import(&file, Some(&fixture("remmina/other.pref"))).unwrap();
        assert_eq!(r.hosts[0].password, None);
        assert!(
            r.warnings.iter().any(|w| w.contains("could not decrypt 1")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn empty_directory_is_an_error() {
        assert!(import(&fixture("ssh_config/conf.d"), None).is_err());
    }
}
