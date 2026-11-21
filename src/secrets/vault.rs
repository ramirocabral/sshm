use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::paths::{self, Fingerprint};
use crate::store::hosts::merge3;

/// scrypt work factor for the vault file (2^17 ≈ 0.3 s); keeps saves snappy.
#[cfg(not(test))]
const WORK_FACTOR: u8 = 17;
#[cfg(test)]
const WORK_FACTOR: u8 = 10;

/// How long `exists()` trusts its last look at the disk.
const EXISTS_TTL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Zeroize)]
#[serde(default)]
pub struct VaultEntry {
    pub password: Option<String>,
    pub root_password: Option<String>,
    pub passphrase: Option<String>,
}

impl VaultEntry {
    pub fn is_empty(&self) -> bool {
        self.password.is_none() && self.root_password.is_none() && self.passphrase.is_none()
    }

    pub fn get(&self, kind: SecretKind) -> Option<&String> {
        match kind {
            SecretKind::Password => self.password.as_ref(),
            SecretKind::RootPassword => self.root_password.as_ref(),
            SecretKind::Passphrase => self.passphrase.as_ref(),
        }
    }

    pub fn set(&mut self, kind: SecretKind, value: Option<String>) {
        let slot = match kind {
            SecretKind::Password => &mut self.password,
            SecretKind::RootPassword => &mut self.root_password,
            SecretKind::Passphrase => &mut self.passphrase,
        };
        if let Some(old) = slot.as_mut() {
            old.zeroize();
        }
        *slot = value.filter(|v| !v.is_empty());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    RootPassword,
    Passphrase,
}

impl SecretKind {
    pub const ALL: [SecretKind; 3] = [SecretKind::Password, SecretKind::RootPassword, SecretKind::Passphrase];
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct VaultData {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: BTreeMap<String, VaultEntry>,
}

impl Drop for VaultData {
    fn drop(&mut self) {
        for e in self.entries.values_mut() {
            e.zeroize();
        }
    }
}

/// Merges per host and per secret: what this instance changed since `base` wins, the rest
/// comes from `theirs` (the file another instance wrote).
fn merge_entries(
    base: &BTreeMap<String, VaultEntry>,
    ours: &BTreeMap<String, VaultEntry>,
    theirs: &BTreeMap<String, VaultEntry>,
) -> BTreeMap<String, VaultEntry> {
    let ids: BTreeSet<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
    let mut out = BTreeMap::new();
    for id in ids {
        let (b, o, t) = (base.get(id), ours.get(id), theirs.get(id));
        let mut entry = VaultEntry::default();
        for kind in SecretKind::ALL {
            let (fb, fo, ft) = (b.and_then(|e| e.get(kind)), o.and_then(|e| e.get(kind)), t.and_then(|e| e.get(kind)));
            entry.set(kind, merge3(fb, fo, ft));
        }
        if !entry.is_empty() {
            out.insert(id.clone(), entry);
        }
    }
    out
}

fn read_encrypted(path: &Path, passphrase: &SecretString) -> Result<VaultData> {
    let ciphertext = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut plain = decrypt_with(&ciphertext, passphrase)?;
    let data = serde_json::from_slice(&plain).context("the vault file is corrupted");
    plain.zeroize();
    data
}

fn write_encrypted(path: &Path, data: &VaultData, passphrase: &SecretString) -> Result<()> {
    let mut plain = serde_json::to_vec(data)?;
    let out = encrypt_with(&plain, passphrase);
    plain.zeroize();
    paths::write_private(path, &out?)
}

/// What this instance knows about the file on disk; shared with its save jobs.
#[derive(Default)]
struct DiskState {
    /// The entries as this instance last read or wrote them: the base of merges.
    base: VaultData,
    fingerprint: Option<Fingerprint>,
    /// The passphrase the file was last read or written with.
    passphrase: Option<SecretString>,
    /// Sequence number of the newest save job written.
    written: u64,
    /// Locked since: jobs finishing now must not bring secrets back into memory.
    locked: bool,
}

/// Secrets keyed by host id, stored as an age file encrypted with a passphrase (scrypt).
/// The file can be opened with the stock `age -d` tool.
pub struct Vault {
    path: PathBuf,
    passphrase: Option<SecretString>,
    data: VaultData,
    disk: Arc<Mutex<DiskState>>,
    next_seq: u64,
    exists: Cell<Option<(bool, Instant)>>,
}

/// A decrypted (or newly created) vault file, ready for `Vault::install`.
pub struct Opened {
    passphrase: SecretString,
    data: VaultData,
    fingerprint: Option<Fingerprint>,
}

/// A snapshot of the vault to write. `run` does the scrypt and the write on any thread; jobs
/// may run late, out of order or concurrently: an older job never overwrites a newer one.
pub struct SaveJob {
    path: PathBuf,
    passphrase: SecretString,
    data: VaultData,
    seq: u64,
    disk: Arc<Mutex<DiskState>>,
}

/// The outcome of a `SaveJob`, for `Vault::apply_saved`.
#[derive(Default)]
pub struct Saved {
    /// Entries another instance had written: (host id, value in the job's snapshot, value now on disk).
    adopted: Vec<(String, Option<VaultEntry>, Option<VaultEntry>)>,
}

impl Drop for Saved {
    fn drop(&mut self) {
        for (_, a, b) in &mut self.adopted {
            a.zeroize();
            b.zeroize();
        }
    }
}

fn lock_state(disk: &Mutex<DiskState>) -> MutexGuard<'_, DiskState> {
    disk.lock().unwrap_or_else(|e| e.into_inner())
}

impl Vault {
    pub fn default_path() -> PathBuf {
        paths::data_dir().join("vault.age")
    }

    pub fn new(path: PathBuf) -> Self {
        Vault {
            path,
            passphrase: None,
            data: VaultData::default(),
            disk: Arc::new(Mutex::new(DiskState::default())),
            next_seq: 0,
            exists: Cell::new(None),
        }
    }

    /// Whether the vault file exists; cached for a second, as the UI asks on every frame.
    pub fn exists(&self) -> bool {
        if let Some((exists, at)) = self.exists.get()
            && at.elapsed() < EXISTS_TTL
        {
            return exists;
        }
        let exists = self.path.exists();
        self.exists.set(Some((exists, Instant::now())));
        exists
    }

    fn mark_exists(&self) {
        self.exists.set(Some((true, Instant::now())));
    }

    pub fn is_unlocked(&self) -> bool {
        self.passphrase.is_some()
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Decrypts the vault at `path`, or creates an empty one there when there is none.
    /// Blocking (scrypt): run it off the UI thread and pass the result to `install`.
    pub fn open(path: &Path, passphrase: &str) -> Result<Opened> {
        if passphrase.is_empty() {
            bail!("the master password cannot be empty");
        }
        let secret = SecretString::from(passphrase.to_string());
        let _lock = paths::lock_for(path)?;
        let Some(fingerprint) = paths::fingerprint(path) else {
            let data = VaultData { version: 1, entries: BTreeMap::new() };
            write_encrypted(path, &data, &secret)?;
            return Ok(Opened { passphrase: secret, data, fingerprint: paths::fingerprint(path) });
        };
        let data = read_encrypted(path, &secret)?;
        Ok(Opened { passphrase: secret, data, fingerprint: Some(fingerprint) })
    }

    /// Makes the result of `open` the unlocked state.
    pub fn install(&mut self, opened: Opened) {
        {
            let mut disk = lock_state(&self.disk);
            disk.base = opened.data.clone();
            disk.fingerprint = opened.fingerprint;
            disk.passphrase = Some(opened.passphrase.clone());
            disk.locked = false;
        }
        self.data = opened.data;
        self.passphrase = Some(opened.passphrase);
        self.mark_exists();
    }

    /// Creates the vault when it does not exist yet, otherwise decrypts it. Blocking.
    pub fn unlock(&mut self, passphrase: &str) -> Result<()> {
        let opened = Self::open(&self.path, passphrase)?;
        self.install(opened);
        Ok(())
    }

    pub fn lock(&mut self) {
        self.passphrase = None;
        self.data = VaultData::default();
        let mut disk = lock_state(&self.disk);
        disk.base = VaultData::default();
        disk.passphrase = None;
        disk.locked = true;
    }

    /// Changes the master password; it takes effect with the next save.
    pub fn set_passphrase(&mut self, new: &str) -> Result<()> {
        if !self.is_unlocked() {
            bail!("the vault is locked");
        }
        if new.is_empty() {
            bail!("the master password cannot be empty");
        }
        self.passphrase = Some(SecretString::from(new.to_string()));
        Ok(())
    }

    /// A detached copy of the unlocked entries, for work done off the UI thread (e.g. an export).
    pub fn snapshot(&self) -> Vault {
        Vault {
            path: self.path.clone(),
            passphrase: self.passphrase.clone(),
            data: self.data.clone(),
            disk: Arc::new(Mutex::new(DiskState::default())),
            next_seq: 0,
            exists: Cell::new(None),
        }
    }

    /// Snapshots the vault for writing off the UI thread (`SaveJob::run`, then `apply_saved`).
    pub fn save_job(&mut self) -> Result<SaveJob> {
        let passphrase = self.passphrase.clone().ok_or_else(|| anyhow!("the vault is locked"))?;
        self.next_seq += 1;
        Ok(SaveJob { path: self.path.clone(), passphrase, data: self.data.clone(), seq: self.next_seq, disk: self.disk.clone() })
    }

    /// Takes in what another instance had saved, unless this instance changed it since the job.
    pub fn apply_saved(&mut self, saved: Saved) {
        self.mark_exists();
        if !self.is_unlocked() {
            return;
        }
        for (id, snapshot, now) in &saved.adopted {
            if self.data.entries.get(id) != snapshot.as_ref() {
                continue;
            }
            match now {
                Some(e) => {
                    self.data.entries.insert(id.clone(), e.clone());
                }
                None => self.remove(id),
            }
        }
    }

    /// Encrypts and writes the vault, merging what another instance saved meanwhile. Blocking.
    pub fn save(&mut self) -> Result<()> {
        let saved = self.save_job()?.run()?;
        self.apply_saved(saved);
        Ok(())
    }

    pub fn entry(&self, host_id: &str) -> Option<&VaultEntry> {
        self.data.entries.get(host_id)
    }

    pub fn get(&self, host_id: &str, kind: SecretKind) -> Option<String> {
        self.entry(host_id).and_then(|e| e.get(kind).cloned())
    }

    pub fn set(&mut self, host_id: &str, kind: SecretKind, value: Option<String>) {
        let entry = self.data.entries.entry(host_id.to_string()).or_default();
        entry.set(kind, value);
        if entry.is_empty() {
            self.data.entries.remove(host_id);
        }
    }

    pub fn remove(&mut self, host_id: &str) {
        if let Some(mut e) = self.data.entries.remove(host_id) {
            e.zeroize();
        }
    }
}

impl SaveJob {
    pub fn run(self) -> Result<Saved> {
        let _lock = paths::lock_for(&self.path)?;
        let (base, known, disk_pass) = {
            let disk = lock_state(&self.disk);
            if self.seq <= disk.written {
                return Ok(Saved::default());
            }
            (disk.base.clone(), disk.fingerprint, disk.passphrase.clone())
        };
        let mut merged = self.data.clone();
        let current = paths::fingerprint(&self.path);
        if current.is_some() && current != known {
            let pass = disk_pass.as_ref().unwrap_or(&self.passphrase);
            let theirs = read_encrypted(&self.path, pass).map_err(|e| {
                if e.to_string() == "wrong master password" {
                    anyhow!("the vault was saved by another sshm with a different master password; not saved (lock and unlock it again)")
                } else {
                    e.context("the vault was changed by another sshm and cannot be read; not saved")
                }
            })?;
            merged.entries = merge_entries(&base.entries, &self.data.entries, &theirs.entries);
        }
        write_encrypted(&self.path, &merged, &self.passphrase)?;

        let ids: BTreeSet<&String> = self.data.entries.keys().chain(merged.entries.keys()).collect();
        let adopted = ids
            .into_iter()
            .filter(|id| self.data.entries.get(*id) != merged.entries.get(*id))
            .map(|id| (id.clone(), self.data.entries.get(id).cloned(), merged.entries.get(id).cloned()))
            .collect();
        let mut disk = lock_state(&self.disk);
        disk.fingerprint = paths::fingerprint(&self.path);
        disk.written = self.seq;
        if !disk.locked {
            disk.base = merged;
            disk.passphrase = Some(self.passphrase.clone());
        }
        Ok(Saved { adopted })
    }
}

pub fn encrypt_with(plain: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let mut recipient = age::scrypt::Recipient::new(passphrase.clone());
    recipient.set_work_factor(WORK_FACTOR);
    age::encrypt(&recipient, plain).map_err(|e| anyhow!("encryption failed: {e}"))
}

pub fn decrypt_with(ciphertext: &[u8], passphrase: &SecretString) -> Result<Vec<u8>> {
    let mut identity = age::scrypt::Identity::new(passphrase.clone());
    identity.set_max_work_factor(22);
    age::decrypt(&identity, ciphertext).map_err(|e| match e {
        age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys => {
            anyhow!("wrong master password")
        }
        other => anyhow!("decryption failed: {other}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path() -> PathBuf {
        std::env::temp_dir().join(format!("sshm-vault-{}", crate::store::hosts::new_id())).join("vault.age")
    }

    fn pw(v: &Vault, host: &str) -> Option<String> {
        v.get(host, SecretKind::Password)
    }

    fn reopen(path: &Path, pass: &str) -> Vault {
        let mut v = Vault::new(path.to_path_buf());
        v.unlock(pass).unwrap();
        v
    }

    #[test]
    fn create_save_reopen() {
        let path = tmp_path();
        let mut v = Vault::new(path.clone());
        assert!(!v.exists());
        v.unlock("master").unwrap();
        assert!(v.exists());
        v.set("h1", SecretKind::Password, Some("p4ss".into()));
        v.set("h1", SecretKind::RootPassword, Some("r00t".into()));
        v.save().unwrap();

        let mut again = Vault::new(path.clone());
        assert!(again.unlock("other").is_err());
        again.unlock("master").unwrap();
        assert_eq!(again.get("h1", SecretKind::Password).as_deref(), Some("p4ss"));
        assert_eq!(again.get("h1", SecretKind::RootPassword).as_deref(), Some("r00t"));

        again.set("h1", SecretKind::Password, None);
        again.set("h1", SecretKind::RootPassword, None);
        assert!(again.entry("h1").is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn open_and_save_off_thread() {
        let path = tmp_path();
        let opened = {
            let path = path.clone();
            std::thread::spawn(move || Vault::open(&path, "m")).join().unwrap().unwrap()
        };
        let mut v = Vault::new(path.clone());
        v.install(opened);
        v.set("h", SecretKind::Passphrase, Some("key-pass".into()));
        let job = v.save_job().unwrap();
        let saved = std::thread::spawn(move || job.run()).join().unwrap().unwrap();
        v.apply_saved(saved);
        assert_eq!(reopen(&path, "m").get("h", SecretKind::Passphrase).as_deref(), Some("key-pass"));
        assert!(Vault::open(&path, "").is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn two_instances_merge_instead_of_overwriting() {
        let path = tmp_path();
        let mut seed = Vault::new(path.clone());
        seed.unlock("m").unwrap();
        seed.set("gone", SecretKind::Password, Some("old".into()));
        seed.set("shared", SecretKind::Password, Some("p".into()));
        seed.save().unwrap();

        let mut a = reopen(&path, "m");
        let mut b = reopen(&path, "m");
        a.set("from-a", SecretKind::Password, Some("a".into()));
        a.remove("gone");
        a.set("shared", SecretKind::RootPassword, Some("root-a".into()));
        a.save().unwrap();

        b.set("from-b", SecretKind::Password, Some("b".into()));
        b.set("shared", SecretKind::Password, Some("p2".into()));
        b.save().unwrap();
        // b adopted a's work while saving.
        assert_eq!(pw(&b, "from-a").as_deref(), Some("a"));
        assert!(b.entry("gone").is_none());

        let disk = reopen(&path, "m");
        assert_eq!(pw(&disk, "from-a").as_deref(), Some("a"));
        assert_eq!(pw(&disk, "from-b").as_deref(), Some("b"));
        assert!(disk.entry("gone").is_none());
        assert_eq!(pw(&disk, "shared").as_deref(), Some("p2"));
        assert_eq!(disk.get("shared", SecretKind::RootPassword).as_deref(), Some("root-a"));

        // b deletes what a wrote; a saving something unrelated keeps the deletion.
        b.remove("from-a");
        b.save().unwrap();
        a.set("later", SecretKind::Password, Some("l".into()));
        a.save().unwrap();
        let disk = reopen(&path, "m");
        assert!(disk.entry("from-a").is_none());
        assert_eq!(pw(&disk, "later").as_deref(), Some("l"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn refuses_to_overwrite_a_vault_with_another_master_password() {
        let path = tmp_path();
        let mut a = reopen(&path, "m");
        let mut b = reopen(&path, "m");
        a.set("h", SecretKind::Password, Some("a".into()));
        a.set_passphrase("new").unwrap();
        a.save().unwrap();
        b.set("x", SecretKind::Password, Some("b".into()));
        let err = b.save().unwrap_err();
        assert!(err.to_string().contains("different master password"), "{err}");
        assert_eq!(pw(&reopen(&path, "new"), "h").as_deref(), Some("a"));
        // The instance that changed it keeps saving fine.
        a.set("h2", SecretKind::Password, Some("2".into()));
        a.save().unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn stale_and_reordered_jobs() {
        let path = tmp_path();
        let mut v = reopen(&path, "m");
        let mut other = reopen(&path, "m");
        v.set("h", SecretKind::Password, Some("1".into()));
        let first = v.save_job().unwrap();
        v.set("h", SecretKind::Password, Some("2".into()));
        v.set("k", SecretKind::Password, Some("k".into()));
        let second = v.save_job().unwrap();

        // Another instance writes between the snapshots and the runs.
        other.set("o", SecretKind::Password, Some("o".into()));
        other.save().unwrap();

        v.apply_saved(second.run().unwrap());
        let skipped = first.run().unwrap();
        assert!(skipped.adopted.is_empty());
        v.apply_saved(skipped);

        let disk = reopen(&path, "m");
        assert_eq!(pw(&disk, "h").as_deref(), Some("2"));
        assert_eq!(pw(&disk, "k").as_deref(), Some("k"));
        assert_eq!(pw(&disk, "o").as_deref(), Some("o"));
        assert_eq!(pw(&v, "o").as_deref(), Some("o"));

        // Locking before a job finishes keeps the secrets out of memory.
        v.set("z", SecretKind::Password, Some("z".into()));
        let job = v.save_job().unwrap();
        v.lock();
        v.apply_saved(job.run().unwrap());
        assert!(v.entry("o").is_none());
        assert_eq!(pw(&reopen(&path, "m"), "z").as_deref(), Some("z"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
