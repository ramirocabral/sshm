//! Ásbrú Connection Manager / PAC Manager (`asbru.yml`, `pac.yml`).
//!
//! Secrets are `Crypt::CBC->encrypt_hex` with Blowfish and a fixed passphrase (asbru-cm lib/PACUtils.pm):
//! hex("Salted__" + salt + ciphertext), key and IV from OpenSSL's EVP_BytesToKey (MD5, one round) with a 56-byte
//! key, because Crypt::Blowfish reports keysize 0 and Crypt::CBC falls back to max_keysize.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use blowfish::Blowfish;
use blowfish::cipher::KeyInit;
use md5::{Digest, Md5};
use serde_yaml_ng::Value;

use super::util::{SkipCounter, cbc_decrypt, clean_folder, clean_segment, hex_decode, non_empty, pkcs7_unpad, port_opt};
use super::{ImportReport, ImportedHost};
use crate::shell;
use crate::store::hosts::{Host, SecretSource};

const APP: &str = "Ásbrú";
const PASSPHRASE: &[u8] = b"PAC Manager (David Torrejon Vaquerizas, david.tv@gmail.com)";
const ROOT: &str = "__PAC__ROOT__";
const SHELL: &str = "__PAC_SHELL__";

pub(super) fn import(path: &Path) -> Result<ImportReport> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text)
}

/// OpenSSL EVP_BytesToKey with MD5 and a single iteration.
fn bytes_to_key(pass: &[u8], salt: &[u8], key_len: usize, iv_len: usize) -> (Vec<u8>, Vec<u8>) {
    let mut out = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    while out.len() < key_len + iv_len {
        let mut h = Md5::new();
        h.update(&prev);
        h.update(pass);
        h.update(salt);
        prev = h.finalize().to_vec();
        out.extend_from_slice(&prev);
    }
    let iv = out[key_len..key_len + iv_len].to_vec();
    out.truncate(key_len);
    (out, iv)
}

/// `None` when the value is not a valid ciphertext.
pub(super) fn decrypt(hex: &str) -> Option<String> {
    let raw = hex_decode(hex)?;
    let rest = raw.strip_prefix(b"Salted__")?;
    if rest.len() < 8 {
        return None;
    }
    let (salt, ct) = rest.split_at(8);
    let (key, iv) = bytes_to_key(PASSPHRASE, salt, 56, 8);
    let cipher: Blowfish = Blowfish::new_from_slice(&key).ok()?;
    String::from_utf8(pkcs7_unpad(cbc_decrypt(&cipher, &iv, ct)?, 8)?).ok()
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => u8::from(*b).to_string(),
        _ => String::new(),
    }
}

fn truthy(v: &Value, key: &str) -> bool {
    !matches!(text(v, key).as_str(), "" | "0" | "false")
}

fn jump_from(v: &Value) -> Option<String> {
    let ip = text(v, "jump ip");
    if ip.is_empty() {
        return None;
    }
    let user = non_empty(&text(v, "jump user")).map(|u| format!("{u}@")).unwrap_or_default();
    let port = text(v, "jump port").parse::<u16>().ok().and_then(port_opt).map(|p| format!(":{p}")).unwrap_or_default();
    Some(format!("{user}{ip}{port}"))
}

fn folder_of(parent: &str, groups: &HashMap<String, (String, String)>) -> String {
    let mut parts = Vec::new();
    let mut seen = HashSet::new();
    let mut cur = parent;
    while let Some((name, up)) = groups.get(cur) {
        if !seen.insert(cur) {
            break;
        }
        parts.push(clean_segment(name));
        cur = up;
    }
    parts.reverse();
    clean_folder(&parts.join("/"))
}

pub(super) fn parse(text_yaml: &str) -> Result<ImportReport> {
    let doc: Value = serde_yaml_ng::from_str(text_yaml).context("not a valid YAML file")?;
    let envs = doc
        .get("environments")
        .and_then(Value::as_mapping)
        .ok_or_else(|| anyhow!("not an Ásbrú/PAC configuration (no 'environments' key)"))?;
    let defaults = doc.get("defaults").cloned().unwrap_or(Value::Null);
    let global_jump = (text(&defaults, "proxy") == "Jump").then(|| jump_from(&defaults)).flatten();

    let groups: HashMap<String, (String, String)> = envs
        .iter()
        .filter(|(_, v)| truthy(v, "_is_group"))
        .filter_map(|(k, v)| Some((k.as_str().filter(|k| *k != ROOT)?.to_string(), (text(v, "name"), text(v, "parent")))))
        .collect();

    let mut report = ImportReport::default();
    let mut skipped = SkipCounter::default();
    let (mut failed, mut proxied) = (0usize, 0usize);
    let mut secret = |v: &Value, key: &str| -> Option<String> {
        let enc = text(v, key);
        if enc.is_empty() {
            return None;
        }
        let plain = decrypt(&enc);
        failed += usize::from(plain.is_none());
        plain.filter(|p| !p.is_empty())
    };

    for (k, v) in envs {
        let Some(uuid) = k.as_str() else { continue };
        if uuid == ROOT || uuid == SHELL || truthy(v, "_is_group") || !v.is_mapping() {
            continue;
        }
        let addr = text(v, "ip");
        let name = [text(v, "name"), text(v, "title"), addr.clone()].into_iter().find(|s| !s.is_empty()).unwrap_or_default();
        let method = text(v, "method");
        if !method.eq_ignore_ascii_case("SSH") {
            skipped.add(if method.is_empty() { "unknown" } else { &method }, &name);
            continue;
        }
        if addr.is_empty() {
            report.warnings.push(format!("{APP}: '{name}' has no IP/host, skipped"));
            continue;
        }

        let key_auth = text(v, "auth type") == "publickey";
        // "use proxy": 0 = global settings, 1 = own SOCKS/HTTP proxy, 2 = none, 3 = own jump server.
        let jump = match text(v, "use proxy").as_str() {
            "3" => jump_from(v),
            "" | "0" => global_jump.clone(),
            "1" => {
                proxied += 1;
                None
            }
            _ => None,
        };
        let ssh_args = match shell::split(&text(v, "options")) {
            Some(args) => args,
            None => {
                report.warnings.push(format!("{APP}: '{name}' has ssh options with an unterminated quote; ignored"));
                Vec::new()
            }
        };
        let description = text(v, "description");
        let host = Host {
            name: clean_segment(&name),
            folder: folder_of(&text(v, "parent"), &groups),
            host: addr,
            port: text(v, "port").parse::<u16>().ok().and_then(port_opt),
            user: non_empty(&text(v, "user")),
            identity_file: if key_auth { non_empty(&text(v, "public key")) } else { None },
            jump,
            notes: if description == format!("Connection with '{name}'") { String::new() } else { description },
            ssh_args,
            password: if text(v, "auth type") == "userpass" { SecretSource::Vault } else { SecretSource::None },
            root_password: SecretSource::None,
            ..Host::default()
        };
        let mut item = ImportedHost::new(host);
        item.password = secret(v, "pass");
        if key_auth {
            item.passphrase = secret(v, "passphrase");
        }
        report.hosts.push(item);
    }

    skipped.warnings(APP, "SSH", &mut report.warnings);
    if failed > 0 {
        report.warnings.push(format!("{APP}: could not decrypt {failed} password(s)"));
    }
    if proxied > 0 {
        report.warnings.push(format!("{APP}: {proxied} connection(s) use a SOCKS/HTTP proxy, which was not imported"));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::fixture;
    use super::*;

    /// Ciphertext of the empty string: found verbatim as the default `passphrase` / keepass `password` in several
    /// unrelated public asbru.yml/pac.yml files (salt "12345678"). It must decrypt to one block of PKCS#5 padding.
    #[test]
    fn published_empty_vector() {
        assert_eq!(decrypt("53616c7465645f5f3132333435363738a9fd581c1ddfc1c2").as_deref(), Some(""));
        assert_eq!(decrypt("53616c7465645f5f3132333435363738a9fd581c1ddfc1c3"), None);
        assert_eq!(decrypt("not hex"), None);
        assert_eq!(decrypt("53616c7465645f5f"), None);
    }

    #[test]
    fn parses_fixture() {
        let r = import(&fixture("asbru/asbru.yml")).unwrap();
        let paths: Vec<_> = r.hosts.iter().map(|h| h.host.path()).collect();
        assert_eq!(paths, ["Clientes/ACME/web-01", "Clientes/ACME/Base de datos/db-01", "Lab/nas", "router"]);

        let web = &r.hosts[0];
        assert_eq!((web.host.host.as_str(), web.host.port, web.host.user.as_deref()), ("10.20.0.11", Some(2222), Some("deploy")));
        assert_eq!(web.password.as_deref(), Some("Contraseña-ñ#1"));
        assert_eq!(web.host.password, SecretSource::Vault);
        assert_eq!(web.host.ssh_args, ["-C", "-o", "ServerAliveInterval=20"]);
        assert_eq!(web.host.notes, "");

        let db = &r.hosts[1];
        assert_eq!(db.host.identity_file.as_deref(), Some("/home/ramiro/.ssh/acme_db"));
        assert_eq!(db.passphrase.as_deref(), Some("frase de la clave"));
        assert_eq!(db.password, None);
        assert_eq!(db.host.password, SecretSource::None);
        assert_eq!(db.host.jump.as_deref(), Some("jumper@bastion.acme.com:2200"));
        assert_eq!(db.host.notes, "Base principal");

        assert_eq!(r.hosts[2].host.jump.as_deref(), Some("ops@gw.lab"));
        assert_eq!(r.hosts[2].password.as_deref(), Some("nas-pass"));
        assert_eq!(r.hosts[3].host.jump, None);

        let w = r.warnings.join("\n");
        assert!(w.contains("skipped 1 RDP (xfreerdp) session(s) ('Escritorio')"), "{w}");
        assert!(w.contains("could not decrypt 1 password(s)"), "{w}");
        assert!(w.contains("1 connection(s) use a SOCKS/HTTP proxy"), "{w}");
    }

    #[test]
    fn rejects_other_yaml() {
        assert!(parse("foo: [1, 2]").is_err());
        assert!(parse(": : :").is_err());
    }
}
