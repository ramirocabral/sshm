//! MobaXterm bookmarks (`.mxtsessions` exports and `MobaXterm.ini`).
//!
//! Session line layout (reverse-engineered, see https://gist.github.com/Ruzgfpegk/ab597838e4abbe8de30d7224afd062ea):
//! `Name=<logout>#<icon>#<type>%<host>%<port>%<user>%…#<terminal>#<start mode>#<comment>#<colour>`.
//! Passwords protected by a master password are AES-256-CFB8 with key = SHA-512(master)[..32] and
//! IV = AES(key, 0^16), as in https://github.com/HyperSine/how-does-MobaXterm-encrypt-password.

use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use sha2::{Digest, Sha512};

use super::util::{
    IniSection, SkipCounter, aes256_cfb8_decrypt, aes256_encrypt_block, clean_folder, clean_segment, decode_text,
    find_section, non_empty, parse_ini, port_opt, printable_secret,
};
use super::{ImportReport, ImportedHost};
use crate::store::hosts::{Host, SecretSource};

const SSH: u32 = 0;
const APP: &str = "MobaXterm";

/// Session types in the order of MobaXterm's "New session" buttons.
const KINDS: [&str; 15] = [
    "SSH", "Telnet", "Rsh", "XDMCP", "RDP", "VNC", "FTP", "SFTP", "Serial", "File", "Shell", "Browser", "Mosh",
    "AWS S3", "WSL",
];

pub(super) fn import(path: &Path, master: Option<&str>) -> Result<ImportReport> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&decode_text(&bytes), master)
}

struct Session {
    kind: u32,
    host: String,
    port: u16,
    user: String,
    command: String,
    jump: Option<String>,
    key: String,
    comment: String,
}

fn unescape(s: &str) -> String {
    s.replace("__PTVIRG__", ";")
        .replace("__DBLQUO__", "\"")
        .replace("__PIPE__", "|")
        .replace("__DIEZE__", "#")
        .replace("__EQUAL__", "=")
        .replace("__DBLDOT__", ":")
}

fn parse_session(value: &str) -> Option<Session> {
    let parts: Vec<&str> = value.split('#').collect();
    let fields: Vec<&str> = parts.get(2)?.split('%').collect();
    let field = |i: usize| fields.get(i).copied().unwrap_or("");
    let kind = field(0).trim().parse().ok()?;
    Some(Session {
        kind,
        host: unescape(field(1)).trim().to_string(),
        port: field(2).trim().parse().unwrap_or(22),
        user: unescape(field(3)).trim().to_string(),
        command: unescape(field(7)).trim().to_string(),
        jump: jump(field(8), field(9), field(10)),
        key: unescape(field(14)).trim().to_string(),
        comment: parts.get(5).map(|c| unescape(c).trim().to_string()).unwrap_or_default(),
    })
}

/// Gateways are parallel `__PIPE__`-separated lists of hosts, ports and users.
fn jump(hosts: &str, ports: &str, users: &str) -> Option<String> {
    let ports: Vec<&str> = ports.split("__PIPE__").collect();
    let users: Vec<&str> = users.split("__PIPE__").collect();
    let hops: Vec<String> = hosts
        .split("__PIPE__")
        .enumerate()
        .filter_map(|(i, h)| {
            let h = unescape(h).trim().to_string();
            if h.is_empty() {
                return None;
            }
            let user = users
                .get(i)
                .map(|u| unescape(u).trim().to_string())
                .filter(|u| !u.is_empty());
            let port = ports
                .get(i)
                .and_then(|p| p.trim().parse::<u16>().ok())
                .and_then(port_opt);
            Some(format!(
                "{}{h}{}",
                user.map(|u| format!("{u}@")).unwrap_or_default(),
                port.map(|p| format!(":{p}")).unwrap_or_default()
            ))
        })
        .collect();
    (!hops.is_empty()).then(|| hops.join(","))
}

fn master_key(master: &str) -> [u8; 32] {
    let digest = Sha512::digest(master.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest[..32]);
    key
}

pub(super) fn decrypt(key: &[u8; 32], b64: &str) -> Option<String> {
    let data = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let iv = aes256_encrypt_block(key, [0; 16])?;
    printable_secret(aes256_cfb8_decrypt(key, &iv, &data)?)
}

fn lookup<'a>(section: Option<&'a IniSection>, key: &str) -> Option<&'a str> {
    section?
        .entries
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

pub(super) fn parse(text: &str, master: Option<&str>) -> Result<ImportReport> {
    let sections = parse_ini(text);
    let passwords = find_section(&sections, "Passwords");
    let credentials = find_section(&sections, "Credentials");
    let key = master.map(master_key);

    let mut report = ImportReport::default();
    let mut skipped = SkipCounter::default();
    let (mut found, mut failed, mut windows_keys) = (false, 0usize, 0usize);
    let mut decrypt_opt = |enc: &str| -> Option<String> {
        let k = key.as_ref()?;
        let plain = decrypt(k, enc);
        failed += usize::from(plain.is_none());
        plain.filter(|p| !p.is_empty())
    };

    for section in &sections {
        let lname = section.name.to_ascii_lowercase();
        if lname != "bookmarks" && !lname.starts_with("bookmarks_") {
            continue;
        }
        found = true;
        let folder = clean_folder(&section.get("SubRep").unwrap_or("").replace('\\', "/"));
        for (name, value) in &section.entries {
            if name.eq_ignore_ascii_case("SubRep") || name.eq_ignore_ascii_case("ImgNum") {
                continue;
            }
            let Some(s) = parse_session(value) else {
                report.warnings.push(format!("{APP}: could not parse session '{name}'"));
                continue;
            };
            if s.kind != SSH {
                skipped.add(KINDS.get(s.kind as usize).copied().unwrap_or("unknown"), name);
                continue;
            }
            if s.host.is_empty() {
                report
                    .warnings
                    .push(format!("{APP}: session '{name}' has no host, skipped"));
                continue;
            }

            let mut user = s.user.clone();
            let mut password = None;
            if let Some(cred) = user
                .strip_prefix('[')
                .and_then(|u| u.strip_suffix(']'))
                .map(str::to_string)
            {
                match lookup(credentials, &cred).and_then(|v| v.split_once(':')) {
                    Some((u, enc)) => {
                        user = u.to_string();
                        password = decrypt_opt(enc);
                    }
                    None => {
                        report.warnings.push(format!(
                            "{APP}: '{name}' uses credential '{cred}', which is not in the file"
                        ));
                        user.clear();
                    }
                }
            }
            if password.is_none() && !user.is_empty() {
                let enc = lookup(passwords, &format!("ssh{}:{user}@{}", s.port, s.host))
                    .or_else(|| lookup(passwords, &format!("{user}@{}", s.host)));
                password = enc.and_then(&mut decrypt_opt);
            }

            let identity_file = non_empty(&s.key.replace("_CurrentDrive_", "C"));
            if identity_file
                .as_deref()
                .is_some_and(|k| k.contains('\\') || k.contains(':') || k.contains("_ProfileDir_"))
            {
                windows_keys += 1;
            }
            let host = Host {
                name: clean_segment(name),
                folder: folder.clone(),
                host: s.host,
                port: port_opt(s.port),
                user: non_empty(&user),
                jump: s.jump,
                notes: s.comment,
                on_connect: non_empty(&s.command),
                password: if identity_file.is_some() {
                    SecretSource::None
                } else {
                    SecretSource::Vault
                },
                identity_file,
                ..Host::default()
            };
            let mut item = ImportedHost::new(host);
            item.password = password;
            report.hosts.push(item);
        }
    }
    if !found {
        bail!("no MobaXterm sessions in the file (no [Bookmarks] sections)");
    }

    skipped.warnings(APP, "SSH", &mut report.warnings);
    let stored = passwords.is_some_and(|s| !s.entries.is_empty()) || credentials.is_some_and(|s| !s.entries.is_empty());
    if stored && key.is_none() {
        report.warnings.push(format!(
            "{APP}: the file has saved passwords; give the master password to import them"
        ));
    }
    if failed > 0 {
        report.warnings.push(format!(
            "{APP}: could not decrypt {failed} password(s) (wrong master password, passwords saved without a master \
             password, or the format of a newer MobaXterm version?)"
        ));
    }
    if windows_keys > 0 {
        report.warnings.push(format!(
            "{APP}: {windows_keys} session(s) use a private key with a Windows path; fix identity_file"
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    /// Published by HyperSine (README of how-does-MobaXterm-encrypt-password), master password "12345678".
    #[test]
    fn published_vectors() {
        let key = master_key("12345678");
        assert_eq!(
            decrypt(&key, "1du11XKQBOxud/FWh4ouWA==").as_deref(),
            Some("Lw3+cZ2s.w@U@f]U")
        );
        assert_eq!(decrypt(&key, "0XROpGmLAYVx").as_deref(), Some("HyperSine"));
        assert_eq!(decrypt(&master_key("other"), "1du11XKQBOxud/FWh4ouWA=="), None);
        assert_eq!(decrypt(&key, "not base64!"), None);
    }

    #[test]
    fn session_line() {
        let s = parse_session("#109#0%10.0.0.1%2222%root%%-1%-1%uptime%gw1__PIPE__gw2%22__PIPE__2200%ops__PIPE__%0%0%0%_CurrentDrive_:\\k.ppk%%-1#MobaFont%10#0# Nota __DIEZE__1 #-1").unwrap();
        assert_eq!(
            (s.kind, s.host.as_str(), s.port, s.user.as_str()),
            (0, "10.0.0.1", 2222, "root")
        );
        assert_eq!(s.command, "uptime");
        assert_eq!(s.jump.as_deref(), Some("ops@gw1,gw2:2200"));
        assert_eq!(s.key, "_CurrentDrive_:\\k.ppk");
        assert_eq!(s.comment, "Nota #1");
        assert!(parse_session("no format").is_none());
        assert!(parse_session("#109#x%host").is_none());
    }

    #[test]
    fn ini_with_master_password() {
        let path = fixture("mobaxterm/MobaXterm.ini");
        let r = import(&path, Some("Maestra-2024")).unwrap();
        let paths: Vec<_> = r.hosts.iter().map(|h| h.host.path()).collect();
        assert_eq!(
            paths,
            [
                "router",
                "Clientes/ACME/web-01",
                "Clientes/ACME/db-01",
                "Clientes/ACME/Producción/app",
                "Lab/legacy"
            ]
        );

        let web = &r.hosts[1];
        assert_eq!(
            (web.host.host.as_str(), web.host.port, web.host.user.as_deref()),
            ("10.20.0.11", None, Some("deploy"))
        );
        assert_eq!(web.password.as_deref(), Some("Web#Pass;2024"));
        assert_eq!(web.host.password, SecretSource::Vault);
        assert_eq!(web.host.notes, "servidor web principal");

        let db = &r.hosts[2];
        assert_eq!(db.host.port, Some(2222));
        assert_eq!(db.password.as_deref(), Some("db-pass"));
        assert_eq!(db.host.jump.as_deref(), Some("jump@bastion.acme.com"));
        assert_eq!(db.host.on_connect.as_deref(), Some("sudo -i"));

        let app = &r.hosts[3];
        assert_eq!(app.host.user.as_deref(), Some("svc"));
        assert_eq!(app.password.as_deref(), Some("cred-pass"));

        let legacy = &r.hosts[4];
        assert_eq!(
            legacy.host.identity_file.as_deref(),
            Some("C:\\Users\\ramiro\\.ssh\\legacy.ppk")
        );
        assert_eq!(legacy.host.password, SecretSource::None);
        assert_eq!(legacy.password, None);

        let w = r.warnings.join("\n");
        assert!(w.contains("skipped 2 RDP session(s)"), "{w}");
        assert!(w.contains("skipped 1 VNC session(s)"), "{w}");
        assert!(w.contains("Windows path"), "{w}");
        assert!(!w.contains("decrypt"), "{w}");
    }

    #[test]
    fn ini_without_or_wrong_master_password() {
        let path = fixture("mobaxterm/MobaXterm.ini");
        let r = import(&path, None).unwrap();
        assert_eq!(r.hosts.len(), 5);
        assert!(r.hosts.iter().all(|h| h.password.is_none()));
        assert!(r.warnings.iter().any(|w| w.contains("give the master password")));

        let r = import(&path, Some("wrong")).unwrap();
        assert!(r.hosts.iter().all(|h| h.password.is_none()));
        assert!(
            r.warnings.iter().any(|w| w.contains("could not decrypt 3 password(s)")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn mxtsessions_in_cp1252() {
        let r = import(&fixture("mobaxterm/sessions.mxtsessions"), None).unwrap();
        let paths: Vec<_> = r.hosts.iter().map(|h| h.host.path()).collect();
        assert_eq!(paths, ["Café/€ server", "Café/Sub carpeta/nas"]);
        assert_eq!(r.hosts[1].host.user.as_deref(), Some("admin"));
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn not_a_mobaxterm_file() {
        assert!(parse("[remmina]\nname=x\n", None).is_err());
    }
}
