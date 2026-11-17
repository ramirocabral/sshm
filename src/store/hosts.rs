use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

/// Where a secret comes from. Serialized as a short string so hosts.toml stays hand-editable:
/// `none`, `vault`, `bw:<item>`, `rbw:<item>`, `op://vault/item/field`, `cmd:<shell command>`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SecretSource {
    None,
    #[default]
    Vault,
    Bitwarden { item: String },
    Rbw { item: String },
    OnePassword { reference: String },
    Command { command: String },
}

impl SecretSource {
    pub fn is_none(&self) -> bool {
        matches!(self, SecretSource::None)
    }

    pub fn label(&self) -> &'static str {
        match self {
            SecretSource::None => "none",
            SecretSource::Vault => "vault",
            SecretSource::Bitwarden { .. } => "bitwarden (bw)",
            SecretSource::Rbw { .. } => "bitwarden (rbw)",
            SecretSource::OnePassword { .. } => "1password",
            SecretSource::Command { .. } => "command",
        }
    }
}

impl fmt::Display for SecretSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SecretSource::None => write!(f, "none"),
            SecretSource::Vault => write!(f, "vault"),
            SecretSource::Bitwarden { item } => write!(f, "bw:{item}"),
            SecretSource::Rbw { item } => write!(f, "rbw:{item}"),
            SecretSource::OnePassword { reference } => write!(f, "{reference}"),
            SecretSource::Command { command } => write!(f, "cmd:{command}"),
        }
    }
}

impl FromStr for SecretSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        Ok(match s {
            "" | "none" => SecretSource::None,
            "vault" => SecretSource::Vault,
            _ if s.starts_with("op://") => SecretSource::OnePassword { reference: s.to_string() },
            _ => {
                if let Some(item) = s.strip_prefix("bw:") {
                    SecretSource::Bitwarden { item: item.trim().to_string() }
                } else if let Some(item) = s.strip_prefix("rbw:") {
                    SecretSource::Rbw { item: item.trim().to_string() }
                } else if let Some(command) = s.strip_prefix("cmd:") {
                    SecretSource::Command { command: command.trim().to_string() }
                } else {
                    return Err(format!(
                        "invalid secret source: {s:?} (use none, vault, bw:<item>, rbw:<item>, op://… or cmd:<command>)"
                    ));
                }
            }
        })
    }
}

impl TryFrom<String> for SecretSource {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<SecretSource> for String {
    fn from(s: SecretSource) -> String {
        s.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Host {
    pub id: String,
    pub name: String,
    /// Folder path separated by '/', empty for the root.
    pub folder: String,
    /// Hostname, IP or an alias from ~/.ssh/config.
    pub host: String,
    pub port: Option<u16>,
    pub user: Option<String>,
    pub identity_file: Option<String>,
    pub jump: Option<String>,
    pub tags: Vec<String>,
    pub notes: String,
    pub password: SecretSource,
    pub root_password: SecretSource,
    /// Fill `[sudo] password for` prompts automatically.
    pub auto_sudo: bool,
    /// Text typed into the session once it is connected.
    pub on_connect: Option<String>,
    /// Extra arguments passed to ssh, e.g. ["-o", "ServerAliveInterval=30"].
    pub ssh_args: Vec<String>,
}

impl Default for Host {
    fn default() -> Self {
        Host {
            id: new_id(),
            name: String::new(),
            folder: String::new(),
            host: String::new(),
            port: None,
            user: None,
            identity_file: None,
            jump: None,
            tags: Vec::new(),
            notes: String::new(),
            password: SecretSource::Vault,
            root_password: SecretSource::Vault,
            auto_sudo: false,
            on_connect: None,
            ssh_args: Vec::new(),
        }
    }
}

impl Host {
    pub fn display_target(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push_str(&format!(":{p}"));
        }
        s
    }

    pub fn path(&self) -> String {
        if self.folder.is_empty() { self.name.clone() } else { format!("{}/{}", self.folder, self.name) }
    }
}

pub fn new_id() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    (0..16).map(|_| format!("{:x}", rng.random_range(0..16u8))).collect()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct HostsFile {
    #[serde(default, rename = "host")]
    hosts: Vec<Host>,
}

#[derive(Debug, Default)]
pub struct HostStore {
    pub hosts: Vec<Host>,
    path: PathBuf,
}

impl HostStore {
    pub fn default_path() -> PathBuf {
        paths::data_dir().join("hosts.toml")
    }

    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path())
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(HostStore { hosts: Vec::new(), path });
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let file: HostsFile = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let mut store = HostStore { hosts: file.hosts, path };
        store.sort();
        Ok(store)
    }

    pub fn save(&self) -> Result<()> {
        let file = HostsFile { hosts: self.hosts.clone() };
        let text = toml::to_string_pretty(&file)?;
        paths::write_private(&self.path, text.as_bytes())
    }

    pub fn sort(&mut self) {
        self.hosts.sort_by(|a, b| {
            a.folder.to_lowercase().cmp(&b.folder.to_lowercase()).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.id == id)
    }

    #[cfg(test)]
    pub fn find_by_path(&self, path: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.path() == path)
    }

    /// Hosts in `folder` or any of its subfolders.
    pub fn hosts_under<'a>(&'a self, folder: &'a str) -> impl Iterator<Item = &'a Host> + 'a {
        self.hosts
            .iter()
            .filter(move |h| h.folder == folder || h.folder.strip_prefix(folder).is_some_and(|rest| rest.starts_with('/')))
    }

    pub fn upsert(&mut self, host: Host) {
        if let Some(existing) = self.get_mut(&host.id) {
            *existing = host;
        } else {
            self.hosts.push(host);
        }
        self.sort();
    }

    pub fn remove(&mut self, id: &str) -> Option<Host> {
        let idx = self.hosts.iter().position(|h| h.id == id)?;
        Some(self.hosts.remove(idx))
    }

    pub fn folders(&self) -> Vec<String> {
        let mut set = std::collections::BTreeSet::new();
        for h in &self.hosts {
            let mut acc = String::new();
            for part in h.folder.split('/').filter(|p| !p.is_empty()) {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                set.insert(acc.clone());
            }
        }
        set.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_source_roundtrip() {
        for s in ["none", "vault", "bw:srv-01", "rbw:srv 02", "op://Infra/srv/password", "cmd:pass show ssh/x"] {
            let parsed: SecretSource = s.parse().unwrap();
            assert_eq!(parsed.to_string(), s);
        }
        assert!("whatever".parse::<SecretSource>().is_err());
    }

    #[test]
    fn hosts_toml_roundtrip() {
        let dir = std::env::temp_dir().join(format!("sshm-test-{}", new_id()));
        let path = dir.join("hosts.toml");
        let mut store = HostStore::load(path.clone()).unwrap();
        store.upsert(Host {
            name: "web".into(),
            folder: "clientes/acme".into(),
            host: "10.0.0.1".into(),
            port: Some(2222),
            user: Some("root".into()),
            password: SecretSource::OnePassword { reference: "op://a/b/password".into() },
            ..Host::default()
        });
        store.save().unwrap();
        let loaded = HostStore::load(path).unwrap();
        assert_eq!(loaded.hosts, store.hosts);
        assert_eq!(loaded.folders(), vec!["clientes".to_string(), "clientes/acme".to_string()]);
        std::fs::remove_dir_all(dir).ok();
    }
}
