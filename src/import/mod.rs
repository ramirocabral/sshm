//! Importing hosts from other tools, and sshm's own JSON/CSV/bundle export format.

mod asbru;
mod mobaxterm;
mod native;
mod remmina;
mod ssh_config;
mod util;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use zeroize::Zeroize;

use crate::secrets::{SecretKind, Vault};
use crate::store::hosts::{Host, HostStore, SecretSource, new_id};

pub struct ImportedHost {
    pub host: Host,
    pub password: Option<String>,
    pub root_password: Option<String>,
    pub passphrase: Option<String>,
}

impl ImportedHost {
    pub fn new(host: Host) -> Self {
        ImportedHost { host, password: None, root_password: None, passphrase: None }
    }

    pub fn has_secrets(&self) -> bool {
        [&self.password, &self.root_password, &self.passphrase].iter().any(|s| s.as_deref().is_some_and(|v| !v.is_empty()))
    }
}

impl Drop for ImportedHost {
    fn drop(&mut self) {
        self.password.zeroize();
        self.root_password.zeroize();
        self.passphrase.zeroize();
    }
}

#[derive(Default)]
pub struct ImportReport {
    pub hosts: Vec<ImportedHost>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Source {
    SshConfig,
    Json,
    Csv,
    Bundle,
    Mobaxterm,
    Asbru,
    Remmina,
}

#[derive(Default)]
pub struct ImportOptions {
    /// Bundle passphrase, or the MobaXterm master password.
    pub passphrase: Option<String>,
    /// remmina.pref holding the key for Remmina passwords; defaults to ~/.config/remmina/remmina.pref.
    pub remmina_pref: Option<PathBuf>,
}

pub fn import(source: Source, path: &Path, opts: &ImportOptions) -> Result<ImportReport> {
    let pass = opts.passphrase.as_deref().filter(|p| !p.is_empty());
    let mut report = match source {
        Source::SshConfig => ssh_config::import(path)?,
        Source::Json => native::import_json(&read(path)?)?,
        Source::Csv => native::import_csv(&read(path)?)?,
        Source::Bundle => native::import_bundle(&read(path)?, pass)?,
        Source::Mobaxterm => mobaxterm::import(path, pass)?,
        Source::Asbru => asbru::import(path)?,
        Source::Remmina => remmina::import(path, opts.remmina_pref.as_deref())?,
    };
    warn_local_commands(&mut report);
    Ok(report)
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("reading {}", path.display()))
}

/// Imported files may come from someone else: flag hosts that would run local commands.
fn warn_local_commands(report: &mut ImportReport) {
    for item in &report.hosts {
        let h = &item.host;
        let mut what = Vec::new();
        if [&h.password, &h.root_password].iter().any(|s| matches!(s, SecretSource::Command { .. })) {
            what.push("cmd:");
        }
        for opt in ["ProxyCommand", "LocalCommand"] {
            if h.ssh_args.iter().any(|a| a.to_ascii_lowercase().contains(&opt.to_ascii_lowercase())) {
                what.push(opt);
            }
        }
        if !what.is_empty() {
            report.warnings.push(format!("'{}' runs local commands ({}); review it before connecting", h.path(), what.join(", ")));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Conflict {
    Skip,
    Overwrite,
    Rename,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MergeSummary {
    pub added: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Added under a new name ("name (2)") because the path was taken.
    pub renamed: usize,
    /// Secrets written to the vault.
    pub secrets: usize,
    /// Secrets discarded because no unlocked vault was given.
    pub secrets_dropped: usize,
}

/// Conflicts are detected by Host::path() (folder/name). Secrets go to the vault only if `vault` is Some and unlocked;
/// imported hosts with a secret get password/root_password = SecretSource::Vault, hosts without one keep their source.
/// Overwrite keeps the existing host id. Nothing is saved: the caller saves the store and the vault.
pub fn merge(store: &mut HostStore, vault: Option<&mut Vault>, report: ImportReport, conflict: Conflict) -> Result<MergeSummary> {
    let mut vault = vault.filter(|v| v.is_unlocked());
    let mut summary = MergeSummary::default();
    let mut by_path: HashMap<String, usize> = store.hosts.iter().enumerate().map(|(i, h)| (h.path(), i)).collect();
    let mut ids: HashSet<String> = store.hosts.iter().map(|h| h.id.clone()).collect();

    for mut item in report.hosts {
        let mut host = std::mem::take(&mut item.host);
        match by_path.get(&host.path()).copied() {
            Some(_) if conflict == Conflict::Skip => {
                summary.skipped += 1;
                continue;
            }
            Some(idx) if conflict == Conflict::Overwrite => {
                host.id = store.hosts[idx].id.clone();
                store_secrets(&mut item, &mut host, vault.as_deref_mut(), &mut summary);
                store.hosts[idx] = host;
                summary.updated += 1;
                continue;
            }
            Some(_) => {
                let base = host.name.clone();
                for n in 2.. {
                    host.name = format!("{base} ({n})");
                    if !by_path.contains_key(&host.path()) {
                        break;
                    }
                }
                summary.renamed += 1;
            }
            None => summary.added += 1,
        }
        if host.id.is_empty() || ids.contains(&host.id) {
            host.id = new_id();
        }
        store_secrets(&mut item, &mut host, vault.as_deref_mut(), &mut summary);
        ids.insert(host.id.clone());
        by_path.insert(host.path(), store.hosts.len());
        store.hosts.push(host);
    }
    store.sort();
    Ok(summary)
}

fn store_secrets(item: &mut ImportedHost, host: &mut Host, mut vault: Option<&mut Vault>, summary: &mut MergeSummary) {
    let secrets = [
        (SecretKind::Password, item.password.take()),
        (SecretKind::RootPassword, item.root_password.take()),
        (SecretKind::Passphrase, item.passphrase.take()),
    ];
    for (kind, value) in secrets {
        let Some(mut value) = value.filter(|v| !v.is_empty()) else {
            continue;
        };
        let Some(v) = vault.as_deref_mut() else {
            value.zeroize();
            summary.secrets_dropped += 1;
            continue;
        };
        v.set(&host.id, kind, Some(value));
        summary.secrets += 1;
        match kind {
            SecretKind::Password => host.password = SecretSource::Vault,
            SecretKind::RootPassword => host.root_password = SecretSource::Vault,
            SecretKind::Passphrase => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum ExportFormat {
    Json,
    Csv,
    Bundle,
}

/// Bundle = JSON encrypted with age scrypt via secrets::vault::encrypt_with. Every format
/// includes the secrets only when with_secrets is set (and then the vault must be unlocked).
pub fn export(
    store: &HostStore,
    vault: Option<&Vault>,
    format: ExportFormat,
    with_secrets: bool,
    passphrase: Option<&str>,
) -> Result<Vec<u8>> {
    let unlocked = vault.filter(|v| v.is_unlocked());
    if with_secrets && unlocked.is_none() {
        bail!("the vault is locked; unlock it to export secrets");
    }
    let secrets = unlocked.filter(|_| with_secrets);
    match format {
        ExportFormat::Json => native::export_json(&store.hosts, secrets),
        ExportFormat::Csv => native::export_csv(&store.hosts, secrets),
        ExportFormat::Bundle => native::export_bundle(&store.hosts, secrets, passphrase),
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::PathBuf;

    use crate::secrets::Vault;
    use crate::store::hosts::{HostStore, new_id};

    pub fn fixture(rel: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(rel)
    }

    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("sshm-import-{}", new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }

        pub fn store(&self) -> HostStore {
            HostStore::load(self.0.join("hosts.toml")).unwrap()
        }

        pub fn vault(&self) -> Vault {
            let mut v = Vault::new(self.0.join("vault.age"));
            v.unlock("master").unwrap();
            v
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::TempDir;
    use super::*;

    fn imported(folder: &str, name: &str, host: &str, password: Option<&str>) -> ImportedHost {
        let mut item = ImportedHost::new(Host {
            name: name.into(),
            folder: folder.into(),
            host: host.into(),
            password: SecretSource::None,
            ..Host::default()
        });
        item.password = password.map(String::from);
        item
    }

    fn seeded() -> (TempDir, HostStore, Vault, String) {
        let tmp = TempDir::new();
        let mut store = tmp.store();
        let mut vault = tmp.vault();
        let existing =
            Host { name: "web".into(), folder: "prod".into(), host: "10.0.0.1".into(), user: Some("old".into()), ..Host::default() };
        let id = existing.id.clone();
        vault.set(&id, SecretKind::Password, Some("old-pass".into()));
        store.upsert(existing);
        (tmp, store, vault, id)
    }

    fn report() -> ImportReport {
        let mut web = imported("prod", "web", "10.0.0.2", Some("new-pass"));
        web.root_password = Some("root-pass".into());
        ImportReport { hosts: vec![web, imported("prod", "db", "10.0.0.3", None)], warnings: vec![] }
    }

    #[test]
    fn merge_skip() {
        let (_tmp, mut store, mut vault, id) = seeded();
        let s = merge(&mut store, Some(&mut vault), report(), Conflict::Skip).unwrap();
        assert_eq!((s.added, s.updated, s.skipped, s.renamed, s.secrets), (1, 0, 1, 0, 0));
        assert_eq!(store.get(&id).unwrap().host, "10.0.0.1");
        assert_eq!(vault.get(&id, SecretKind::Password).as_deref(), Some("old-pass"));
        let db = store.find_by_path("prod/db").unwrap();
        assert_eq!(db.password, SecretSource::None);
    }

    #[test]
    fn merge_overwrite_keeps_id() {
        let (_tmp, mut store, mut vault, id) = seeded();
        let s = merge(&mut store, Some(&mut vault), report(), Conflict::Overwrite).unwrap();
        assert_eq!((s.added, s.updated, s.skipped, s.renamed, s.secrets), (1, 1, 0, 0, 2));
        assert_eq!(store.hosts.len(), 2);
        let web = store.find_by_path("prod/web").unwrap();
        assert_eq!(web.id, id);
        assert_eq!(web.host, "10.0.0.2");
        assert_eq!(web.user, None);
        assert_eq!(web.password, SecretSource::Vault);
        assert_eq!(web.root_password, SecretSource::Vault);
        assert_eq!(vault.get(&id, SecretKind::Password).as_deref(), Some("new-pass"));
        assert_eq!(vault.get(&id, SecretKind::RootPassword).as_deref(), Some("root-pass"));
    }

    #[test]
    fn merge_rename() {
        let (_tmp, mut store, mut vault, id) = seeded();
        let mut r = report();
        r.hosts.push(imported("prod", "web", "10.0.0.9", None));
        let s = merge(&mut store, Some(&mut vault), r, Conflict::Rename).unwrap();
        assert_eq!((s.added, s.updated, s.skipped, s.renamed, s.secrets), (1, 0, 0, 2, 2));
        assert_eq!(store.get(&id).unwrap().host, "10.0.0.1");
        let renamed = store.find_by_path("prod/web (2)").unwrap();
        assert_eq!(renamed.host, "10.0.0.2");
        assert_ne!(renamed.id, id);
        assert_eq!(vault.get(&renamed.id, SecretKind::Password).as_deref(), Some("new-pass"));
        assert_eq!(store.find_by_path("prod/web (3)").unwrap().host, "10.0.0.9");
        assert_eq!(store.hosts.len(), 4);
    }

    #[test]
    fn merge_without_vault_drops_secrets() {
        let (_tmp, mut store, mut vault, _) = seeded();
        vault.lock();
        let s = merge(&mut store, Some(&mut vault), report(), Conflict::Rename).unwrap();
        assert_eq!((s.secrets, s.secrets_dropped), (0, 2));
        assert_eq!(store.find_by_path("prod/web (2)").unwrap().password, SecretSource::None);
        let s = merge(&mut store, None, report(), Conflict::Skip).unwrap();
        assert_eq!((s.skipped, s.secrets_dropped), (2, 0));
    }

    #[test]
    fn merge_regenerates_colliding_ids() {
        let (_tmp, mut store, mut vault, id) = seeded();
        let mut item = imported("otra", "web", "10.0.0.5", Some("x"));
        item.host.id = id.clone();
        let r = ImportReport { hosts: vec![item], warnings: vec![] };
        merge(&mut store, Some(&mut vault), r, Conflict::Skip).unwrap();
        let new = store.find_by_path("otra/web").unwrap();
        assert_ne!(new.id, id);
        assert_eq!(vault.get(&id, SecretKind::Password).as_deref(), Some("old-pass"));
        assert_eq!(store.get(&id).unwrap().host, "10.0.0.1");
    }

    #[test]
    fn warns_about_local_commands() {
        let mut item = imported("", "x", "x", None);
        item.host.password = SecretSource::Command { command: "pass show x".into() };
        item.host.ssh_args = vec!["-o".into(), "proxycommand=nc %h %p".into()];
        let mut r = ImportReport { hosts: vec![item], warnings: vec![] };
        warn_local_commands(&mut r);
        assert_eq!(r.warnings.len(), 1);
        assert!(r.warnings[0].contains("cmd:, ProxyCommand"), "{:?}", r.warnings);
    }
}
