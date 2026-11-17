use std::collections::BTreeMap;
use std::path::PathBuf;

use age::secrecy::SecretString;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use crate::paths;

/// scrypt work factor for the vault file (2^17 ≈ 0.3 s); keeps saves snappy.
const WORK_FACTOR: u8 = 17;

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

#[derive(Default, Serialize, Deserialize)]
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

/// Secrets keyed by host id, stored as an age file encrypted with a passphrase (scrypt).
/// The file can be opened with the stock `age -d` tool.
pub struct Vault {
    path: PathBuf,
    passphrase: Option<SecretString>,
    data: VaultData,
}

impl Vault {
    pub fn default_path() -> PathBuf {
        paths::data_dir().join("vault.age")
    }

    pub fn new(path: PathBuf) -> Self {
        Vault { path, passphrase: None, data: VaultData::default() }
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    pub fn is_unlocked(&self) -> bool {
        self.passphrase.is_some()
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// Creates the vault when it does not exist yet, otherwise decrypts it.
    pub fn unlock(&mut self, passphrase: &str) -> Result<()> {
        if passphrase.is_empty() {
            bail!("the master password cannot be empty");
        }
        let secret = SecretString::from(passphrase.to_string());
        if !self.exists() {
            self.passphrase = Some(secret);
            self.data = VaultData { version: 1, entries: BTreeMap::new() };
            return self.save();
        }
        let ciphertext = std::fs::read(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        let data = decrypt_with(&ciphertext, &secret)?;
        self.data = serde_json::from_slice(&data).context("the vault file is corrupted")?;
        self.passphrase = Some(secret);
        Ok(())
    }

    pub fn lock(&mut self) {
        self.passphrase = None;
        self.data = VaultData::default();
    }

    pub fn change_passphrase(&mut self, new: &str) -> Result<()> {
        if !self.is_unlocked() {
            bail!("the vault is locked");
        }
        if new.is_empty() {
            bail!("the master password cannot be empty");
        }
        self.passphrase = Some(SecretString::from(new.to_string()));
        self.save()
    }

    pub fn save(&self) -> Result<()> {
        let pass = self.passphrase.as_ref().ok_or_else(|| anyhow!("the vault is locked"))?;
        let mut plain = serde_json::to_vec(&self.data)?;
        let out = encrypt_with(&plain, pass);
        plain.zeroize();
        paths::write_private(&self.path, &out?)
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

    #[test]
    fn create_save_reopen() {
        let path = tmp_path();
        let mut v = Vault::new(path.clone());
        v.unlock("master").unwrap();
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
}
