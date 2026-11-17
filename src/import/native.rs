//! sshm's own formats: JSON (lossless), CSV (spreadsheet-friendly) and bundle (JSON encrypted with age).

use std::collections::HashMap;

use age::secrecy::SecretString;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::util::{clean_folder, clean_segment, non_empty};
use super::{ImportReport, ImportedHost};
use crate::secrets::Vault;
use crate::secrets::vault::{decrypt_with, encrypt_with};
use crate::store::hosts::Host;

const VERSION: u32 = 1;

pub(super) const CSV_HEADER: [&str; 11] = [
    "name",
    "folder",
    "host",
    "port",
    "user",
    "identity_file",
    "jump",
    "tags",
    "password",
    "root_password",
    "notes",
];

#[derive(Serialize, Deserialize)]
struct NativeFile {
    version: u32,
    hosts: Vec<NativeHost>,
}

#[derive(Serialize, Deserialize)]
struct NativeHost {
    #[serde(flatten)]
    host: Host,
    #[serde(default, skip_serializing_if = "Secrets::is_empty")]
    secrets: Secrets,
}

#[derive(Default, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct Secrets {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    root_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    passphrase: Option<String>,
}

impl Secrets {
    fn of(host: &Host, vault: Option<&Vault>) -> Self {
        let Some(e) = vault.and_then(|v| v.entry(&host.id)) else {
            return Secrets::default();
        };
        Secrets {
            password: e.password.clone(),
            root_password: e.root_password.clone(),
            passphrase: e.passphrase.clone(),
        }
    }

    fn is_empty(&self) -> bool {
        self.password.is_none() && self.root_password.is_none() && self.passphrase.is_none()
    }
}

fn to_json(hosts: &[Host], vault: Option<&Vault>) -> Result<Vec<u8>> {
    let file = NativeFile {
        version: VERSION,
        hosts: hosts
            .iter()
            .map(|h| NativeHost {
                host: h.clone(),
                secrets: Secrets::of(h, vault),
            })
            .collect(),
    };
    Ok(serde_json::to_vec_pretty(&file)?)
}

pub(super) fn export_json(hosts: &[Host], vault: Option<&Vault>) -> Result<Vec<u8>> {
    to_json(hosts, vault)
}

pub(super) fn export_bundle(hosts: &[Host], vault: Option<&Vault>, passphrase: Option<&str>) -> Result<Vec<u8>> {
    let Some(pass) = passphrase.filter(|p| !p.is_empty()) else {
        bail!("a bundle needs a passphrase")
    };
    let mut plain = to_json(hosts, vault)?;
    let out = encrypt_with(&plain, &SecretString::from(pass.to_string()));
    plain.zeroize();
    out
}

pub(super) fn export_csv(hosts: &[Host], vault: Option<&Vault>) -> Result<Vec<u8>> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record(CSV_HEADER)?;
    for h in hosts {
        let s = Secrets::of(h, vault);
        let port = h.port.map(|p| p.to_string()).unwrap_or_default();
        let opt = |o: &Option<String>| o.clone().unwrap_or_default();
        w.write_record([
            h.name.clone(),
            h.folder.clone(),
            h.host.clone(),
            port,
            opt(&h.user),
            opt(&h.identity_file),
            opt(&h.jump),
            h.tags.join(";"),
            opt(&s.password),
            opt(&s.root_password),
            h.notes.clone(),
        ])?;
    }
    w.into_inner().context("could not generate the CSV")
}

pub(super) fn import_json(bytes: &[u8]) -> Result<ImportReport> {
    let file: NativeFile = serde_json::from_slice(bytes).context("not an sshm JSON export")?;
    if file.version != VERSION {
        bail!(
            "unsupported export version {} (this sshm reads version {VERSION})",
            file.version
        );
    }
    let mut report = ImportReport::default();
    for (i, mut nh) in file.hosts.into_iter().enumerate() {
        let mut host = nh.host;
        host.name = clean_segment(&host.name);
        host.folder = clean_folder(&host.folder);
        host.host = host.host.trim().to_string();
        if host.host.is_empty() {
            report.warnings.push(format!("host #{}: no address, skipped", i + 1));
            continue;
        }
        if host.name.is_empty() {
            host.name = clean_segment(&host.host);
        }
        let mut item = ImportedHost::new(host);
        item.password = nh.secrets.password.take();
        item.root_password = nh.secrets.root_password.take();
        item.passphrase = nh.secrets.passphrase.take();
        report.hosts.push(item);
    }
    Ok(report)
}

pub(super) fn import_bundle(bytes: &[u8], passphrase: Option<&str>) -> Result<ImportReport> {
    let Some(pass) = passphrase else {
        bail!("the bundle is encrypted; its passphrase is required")
    };
    let mut plain =
        decrypt_with(bytes, &SecretString::from(pass.to_string())).context("could not decrypt the bundle")?;
    let report = import_json(&plain);
    plain.zeroize();
    report
}

pub(super) fn import_csv(bytes: &[u8]) -> Result<ImportReport> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut rdr = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(bytes);
    let headers = rdr.headers().context("the CSV has no header row")?.clone();
    let cols: HashMap<String, usize> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.to_ascii_lowercase(), i))
        .collect();
    if !cols.contains_key("host") {
        bail!(
            "the CSV has no 'host' column (expected header: {})",
            CSV_HEADER.join(",")
        );
    }

    let mut report = ImportReport::default();
    for (i, record) in rdr.records().enumerate() {
        let line = i + 2;
        let record = match record {
            Ok(r) => r,
            Err(e) => {
                report.warnings.push(format!("row {line}: {e}"));
                continue;
            }
        };
        let get = |col: &str| cols.get(col).and_then(|&i| record.get(i)).unwrap_or("").to_string();
        let addr = get("host");
        if addr.is_empty() {
            report.warnings.push(format!("row {line}: no host, skipped"));
            continue;
        }
        let port = match get("port") {
            p if p.is_empty() => None,
            p => match p.parse::<u16>() {
                Ok(p) => Some(p),
                Err(_) => {
                    report
                        .warnings
                        .push(format!("row {line}: invalid port '{p}', using the default"));
                    None
                }
            },
        };
        let name = non_empty(&get("name")).unwrap_or_else(|| addr.clone());
        let host = Host {
            name: clean_segment(&name),
            folder: clean_folder(&get("folder")),
            host: addr,
            port,
            user: non_empty(&get("user")),
            identity_file: non_empty(&get("identity_file")),
            jump: non_empty(&get("jump")),
            tags: get("tags").split(';').filter_map(non_empty).collect(),
            notes: get("notes"),
            ..Host::default()
        };
        let mut item = ImportedHost::new(host);
        item.password = non_empty(&get("password"));
        item.root_password = non_empty(&get("root_password"));
        report.hosts.push(item);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{TempDir, fixture};
    use super::super::{Conflict, ExportFormat, ImportOptions, Source, export, import, merge};
    use super::*;
    use crate::secrets::SecretKind;
    use crate::store::hosts::{HostStore, SecretSource};

    fn sample(tmp: &TempDir) -> (HostStore, Vault) {
        let mut store = tmp.store();
        let mut vault = tmp.vault();
        let web = Host {
            name: "web".into(),
            folder: "clientes/acme".into(),
            host: "10.0.0.1".into(),
            port: Some(2222),
            user: Some("deploy".into()),
            identity_file: Some("~/.ssh/acme".into()),
            jump: Some("bastion".into()),
            tags: vec!["prod".into(), "web".into()],
            notes: "note, with \"quotes\"\nand a newline".into(),
            auto_sudo: true,
            on_connect: Some("cd /srv".into()),
            ssh_args: vec!["-o".into(), "ServerAliveInterval=30".into()],
            ..Host::default()
        };
        vault.set(&web.id, SecretKind::Password, Some("p4ss,;\"x".into()));
        vault.set(&web.id, SecretKind::RootPassword, Some("r00t".into()));
        vault.set(&web.id, SecretKind::Passphrase, Some("frase".into()));
        let op = Host {
            name: "db".into(),
            host: "db.local".into(),
            password: SecretSource::OnePassword {
                reference: "op://Infra/db/password".into(),
            },
            root_password: SecretSource::None,
            ..Host::default()
        };
        store.upsert(web);
        store.upsert(op);
        (store, vault)
    }

    fn import_bytes(source: Source, bytes: &[u8], passphrase: Option<&str>) -> Result<ImportReport> {
        let tmp = TempDir::new();
        let path = tmp.0.join("export");
        std::fs::write(&path, bytes).unwrap();
        import(
            source,
            &path,
            &ImportOptions {
                passphrase: passphrase.map(String::from),
                remmina_pref: None,
            },
        )
    }

    /// Imports into a fresh store + vault and compares hosts and secrets with the original.
    fn assert_roundtrip(report: ImportReport, original: &HostStore, vault: &Vault, full_fidelity: bool) {
        let tmp = TempDir::new();
        let mut store = tmp.store();
        let mut new_vault = tmp.vault();
        let summary = merge(&mut store, Some(&mut new_vault), report, Conflict::Skip).unwrap();
        assert_eq!(summary.added, original.hosts.len());
        for orig in &original.hosts {
            let got = store.find_by_path(&orig.path()).unwrap();
            if full_fidelity {
                assert_eq!(got, orig);
            } else {
                assert_eq!(
                    (
                        &got.host,
                        got.port,
                        &got.user,
                        &got.identity_file,
                        &got.jump,
                        &got.tags,
                        &got.notes
                    ),
                    (
                        &orig.host,
                        orig.port,
                        &orig.user,
                        &orig.identity_file,
                        &orig.jump,
                        &orig.tags,
                        &orig.notes
                    )
                );
            }
            for kind in [SecretKind::Password, SecretKind::RootPassword] {
                assert_eq!(
                    new_vault.get(&got.id, kind),
                    vault.get(&orig.id, kind),
                    "{kind:?} of {}",
                    orig.path()
                );
            }
        }
    }

    #[test]
    fn json_roundtrip_with_secrets() {
        let tmp = TempDir::new();
        let (store, vault) = sample(&tmp);
        let bytes = export(&store, Some(&vault), ExportFormat::Json, true, None).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("\"version\": 1") && text.contains("p4ss"));
        let report = import_bytes(Source::Json, &bytes, None).unwrap();
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(
            report
                .hosts
                .iter()
                .find(|h| h.host.name == "web")
                .unwrap()
                .passphrase
                .as_deref(),
            Some("frase")
        );
        assert_roundtrip(report, &store, &vault, true);
    }

    #[test]
    fn json_without_secrets() {
        let tmp = TempDir::new();
        let (store, vault) = sample(&tmp);
        let bytes = export(&store, Some(&vault), ExportFormat::Json, false, None).unwrap();
        assert!(!String::from_utf8(bytes.clone()).unwrap().contains("secrets"));
        let report = import_bytes(Source::Json, &bytes, None).unwrap();
        assert!(report.hosts.iter().all(|h| !h.has_secrets()));
        let locked = Vault::new(tmp.0.join("otra.age"));
        assert!(export(&store, Some(&locked), ExportFormat::Json, true, None).is_err());
        assert!(export(&store, None, ExportFormat::Csv, true, None).is_err());
    }

    #[test]
    fn csv_roundtrip_with_secrets() {
        let tmp = TempDir::new();
        let (store, vault) = sample(&tmp);
        let bytes = export(&store, Some(&vault), ExportFormat::Csv, true, None).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.starts_with(&CSV_HEADER.join(",")));
        assert!(text.contains("prod;web"));
        assert_roundtrip(import_bytes(Source::Csv, &bytes, None).unwrap(), &store, &vault, false);
    }

    #[test]
    fn bundle_roundtrip() {
        let tmp = TempDir::new();
        let (store, vault) = sample(&tmp);
        assert!(export(&store, Some(&vault), ExportFormat::Bundle, false, None).is_err());
        let bytes = export(&store, Some(&vault), ExportFormat::Bundle, false, Some("bundle-pass")).unwrap();
        assert!(bytes.starts_with(b"age-encryption.org/v1"));
        assert!(import_bytes(Source::Bundle, &bytes, Some("wrong")).is_err());
        assert!(import_bytes(Source::Bundle, &bytes, None).is_err());
        let report = import_bytes(Source::Bundle, &bytes, Some("bundle-pass")).unwrap();
        assert_roundtrip(report, &store, &vault, true);
    }

    #[test]
    fn rejects_other_json() {
        assert!(import_json(br#"{"hosts": []}"#).is_err());
        assert!(import_json(br#"{"version": 9, "hosts": []}"#).is_err());
        assert!(import_json(b"[1,2").is_err());
    }

    #[test]
    fn csv_fixture() {
        let r = import(Source::Csv, &fixture("native/hosts.csv"), &ImportOptions::default()).unwrap();
        let names: Vec<_> = r.hosts.iter().map(|h| h.host.path()).collect();
        assert_eq!(
            names,
            ["Clientes/ACME/web-01", "Clientes/ACME/db-01", "10.1.1.1", "lab/router"]
        );
        let web = &r.hosts[0];
        assert_eq!(web.host.port, Some(2222));
        assert_eq!(web.host.tags, ["prod", "web"]);
        assert_eq!(web.password.as_deref(), Some("s3cr3t,\"ok\""));
        assert_eq!(web.root_password.as_deref(), Some("t00r"));
        assert_eq!(r.hosts[1].host.notes, "réplica\nprincipal");
        assert_eq!(r.hosts[2].host.port, None);
        assert_eq!(r.hosts[3].host.jump.as_deref(), Some("admin@bastion:2200"));
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("invalid port"));
        assert!(r.warnings[1].contains("no host"));
    }

    #[test]
    fn json_fixture_with_defaults() {
        let r = import(Source::Json, &fixture("native/hosts.json"), &ImportOptions::default()).unwrap();
        assert_eq!(r.hosts.len(), 2);
        let a = &r.hosts[0].host;
        assert_eq!((a.path().as_str(), a.port, a.id.len()), ("infra/proxy", Some(22), 16));
        assert_eq!(a.password, SecretSource::Bitwarden { item: "proxy".into() });
        assert_eq!(r.hosts[1].host.name, "10.9.9.9");
        assert_eq!(r.hosts[1].password.as_deref(), Some("hunter2"));
        assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
        assert!(r.warnings[1].contains("local commands"));
    }
}
