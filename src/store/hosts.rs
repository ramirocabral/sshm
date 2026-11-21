use std::collections::{HashMap, HashSet};
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
    /// The desktop keyring (Secret Service), through secret-tool.
    Keyring,
    Bitwarden {
        item: String,
    },
    Rbw {
        item: String,
    },
    OnePassword {
        reference: String,
    },
    Command {
        command: String,
    },
}

impl SecretSource {
    pub fn is_none(&self) -> bool {
        matches!(self, SecretSource::None)
    }

    pub fn label(&self) -> &'static str {
        match self {
            SecretSource::None => "none",
            SecretSource::Vault => "vault",
            SecretSource::Keyring => "system keyring",
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
            SecretSource::Keyring => write!(f, "keyring"),
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
            "keyring" => SecretSource::Keyring,
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
                        "invalid secret source: {s:?} (use none, vault, keyring, bw:<item>, rbw:<item>, op://… or cmd:<command>)"
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
    /// Port forwardings that go through this host.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tunnels: Vec<Tunnel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelKind {
    /// `-L`: a port on this machine reaches host:port as seen from the server.
    #[default]
    Local,
    /// `-R`: a port on the server reaches host:port as seen from this machine.
    Remote,
    /// `-D`: a SOCKS proxy on this machine whose traffic leaves through the server.
    Dynamic,
}

impl TunnelKind {
    pub const ALL: [TunnelKind; 3] = [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic];

    pub fn value(self) -> &'static str {
        match self {
            TunnelKind::Local => "local",
            TunnelKind::Remote => "remote",
            TunnelKind::Dynamic => "dynamic",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.value() == s)
    }

    pub fn flag(self) -> &'static str {
        match self {
            TunnelKind::Local => "-L",
            TunnelKind::Remote => "-R",
            TunnelKind::Dynamic => "-D",
        }
    }

    /// Short name for lists.
    pub fn label(self) -> &'static str {
        match self {
            TunnelKind::Local => "local",
            TunnelKind::Remote => "remote",
            TunnelKind::Dynamic => "socks",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelStart {
    /// Only when turned on in the tunnels tab.
    #[default]
    Manual,
    /// When sshm starts.
    Launch,
    /// When a session to the host opens.
    Connect,
}

impl TunnelStart {
    pub const ALL: [TunnelStart; 3] = [TunnelStart::Manual, TunnelStart::Launch, TunnelStart::Connect];

    pub fn value(self) -> &'static str {
        match self {
            TunnelStart::Manual => "manual",
            TunnelStart::Launch => "launch",
            TunnelStart::Connect => "connect",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.value() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    pub kind: TunnelKind,
    /// Where the port listens (the server for remote tunnels, this machine otherwise); empty = localhost.
    pub bind_address: String,
    pub bind_port: u16,
    /// Where connections are sent; empty = localhost. SOCKS tunnels have none.
    pub dest_host: String,
    pub dest_port: u16,
    pub start: TunnelStart,
}

impl Default for Tunnel {
    fn default() -> Self {
        Tunnel {
            id: new_id(),
            name: String::new(),
            kind: TunnelKind::Local,
            bind_address: String::new(),
            bind_port: 0,
            dest_host: String::new(),
            dest_port: 0,
            start: TunnelStart::Manual,
        }
    }
}

/// An address as ssh's forwarding syntax wants it: IPv6 literals go in brackets.
fn fwd_addr(a: &str) -> String {
    if a.contains(':') && !a.starts_with('[') { format!("[{a}]") } else { a.to_string() }
}

impl Tunnel {
    fn bind(&self) -> &str {
        match self.bind_address.trim() {
            "" => "localhost",
            a => a,
        }
    }

    fn dest(&self) -> &str {
        match self.dest_host.trim() {
            "" => "localhost",
            h => h,
        }
    }

    /// The argument of -L/-R/-D: `[bind:]port[:host:hostport]`.
    pub fn spec(&self) -> String {
        let mut s = match self.bind_address.trim() {
            "" => String::new(),
            a => format!("{}:", fwd_addr(a)),
        };
        s.push_str(&self.bind_port.to_string());
        if self.kind != TunnelKind::Dynamic {
            s.push_str(&format!(":{}:{}", fwd_addr(self.dest()), self.dest_port));
        }
        s
    }

    pub fn forward_args(&self) -> Vec<String> {
        vec![self.kind.flag().to_string(), self.spec()]
    }

    /// Where the port listens, e.g. "localhost:5432".
    pub fn listen(&self) -> String {
        format!("{}:{}", fwd_addr(self.bind()), self.bind_port)
    }

    /// "localhost:5432 → db:5432", or the listening address of a SOCKS proxy.
    pub fn summary(&self) -> String {
        match self.kind {
            TunnelKind::Dynamic => self.listen(),
            _ => format!("{} → {}:{}", self.listen(), fwd_addr(self.dest()), self.dest_port),
        }
    }

    /// The name, or the summary when it has none.
    pub fn title(&self) -> String {
        if self.name.trim().is_empty() { self.summary() } else { self.name.trim().to_string() }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.bind_port == 0 {
            return Err("the port to listen on is required".into());
        }
        if self.kind != TunnelKind::Dynamic && self.dest_port == 0 {
            return Err("the destination port is required".into());
        }
        if [&self.bind_address, &self.dest_host].iter().any(|a| a.trim().contains(char::is_whitespace)) {
            return Err("an address cannot contain spaces".into());
        }
        Ok(())
    }
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
            tunnels: Vec::new(),
        }
    }
}

impl Host {
    pub fn display_target(&self) -> String {
        let mut s = String::new();
        if let Some(u) = self.user.as_ref().filter(|u| !u.is_empty()) {
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

/// Trims every segment of a folder path and drops the empty ones: " a / /b/" → "a/b".
pub fn normalize_folder(s: &str) -> String {
    s.split('/').map(str::trim).filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/")
}

/// The folder that contains `folder` ("" at the top level).
pub fn parent_folder(folder: &str) -> &str {
    folder.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
}

fn join_folder(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

/// The path `folder` gets when renamed to `name` in place, or why `name` is not valid.
pub fn rename_target(folder: &str, name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("The folder name cannot be empty");
    }
    if name.contains('/') {
        return Err("A folder name cannot contain '/': press m to move the folder");
    }
    Ok(join_folder(parent_folder(folder), name))
}

/// The path `folder` gets when moved into `dest` (it keeps its name), or why it cannot go there.
pub fn move_target(folder: &str, dest: &str) -> Result<String, &'static str> {
    let dest = normalize_folder(dest);
    if subpath(&dest, folder).is_some() {
        return Err("A folder cannot be moved inside itself");
    }
    let name = folder.rsplit_once('/').map(|(_, n)| n).unwrap_or(folder);
    Ok(join_folder(&dest, name))
}

/// When `folder` is `parent` or lies below it, the part after `parent` ("" or "/sub/…").
pub fn subpath<'a>(folder: &'a str, parent: &str) -> Option<&'a str> {
    folder.strip_prefix(parent).filter(|rest| rest.is_empty() || rest.starts_with('/'))
}

pub fn new_id() -> String {
    use rand::RngExt;
    let mut rng = rand::rng();
    (0..16).map(|_| format!("{:x}", rng.random_range(0..16u8))).collect()
}

#[derive(Debug, Default, Deserialize)]
struct HostsFile {
    #[serde(default, rename = "host")]
    hosts: Vec<Host>,
}

#[derive(Serialize)]
struct HostsFileRef<'a> {
    #[serde(rename = "host")]
    hosts: &'a [Host],
}

/// Three-way merge of one item: our change wins, else whatever the other side has now.
pub(crate) fn merge3<T: Clone + PartialEq>(base: Option<&T>, ours: Option<&T>, theirs: Option<&T>) -> Option<T> {
    if ours != base { ours.cloned() } else { theirs.cloned() }
}

/// Merges by host id: hosts this instance added, changed or deleted since `base` keep its
/// version; every other host takes the one on disk (`theirs`).
fn merge_hosts(base: &[Host], ours: &[Host], theirs: &[Host]) -> Vec<Host> {
    let index = |hs: &[Host]| hs.iter().map(|h| (h.id.clone(), h.clone())).collect::<HashMap<_, _>>();
    let (b, o, t) = (index(base), index(ours), index(theirs));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for id in ours.iter().chain(theirs).chain(base).map(|h| h.id.as_str()) {
        if seen.insert(id)
            && let Some(h) = merge3(b.get(id), o.get(id), t.get(id))
        {
            out.push(h);
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct HostStore {
    pub hosts: Vec<Host>,
    path: PathBuf,
    /// The hosts as this instance last read or wrote them: the base of merges with other instances.
    base: Vec<Host>,
    disk: Option<paths::Fingerprint>,
    /// A version on disk that failed to parse, so the tick does not report it again.
    unreadable: Option<paths::Fingerprint>,
}

impl HostStore {
    pub fn default_path() -> PathBuf {
        paths::data_dir().join("hosts.toml")
    }

    pub fn load_default() -> Result<Self> {
        Self::load(Self::default_path())
    }

    pub fn load(path: PathBuf) -> Result<Self> {
        let mut store = HostStore { path, ..HostStore::default() };
        if let Some((hosts, fp)) = store.read_disk()? {
            store.hosts = hosts;
            store.sort();
            store.base = store.hosts.clone();
            store.disk = Some(fp);
        }
        Ok(store)
    }

    /// The hosts on disk and their fingerprint, or None when there is no file.
    fn read_disk(&self) -> Result<Option<(Vec<Host>, paths::Fingerprint)>> {
        let Some(fp) = paths::fingerprint(&self.path) else { return Ok(None) };
        let text = std::fs::read_to_string(&self.path).with_context(|| format!("reading {}", self.path.display()))?;
        let file: HostsFile = toml::from_str(&text).with_context(|| format!("parsing {}", self.path.display()))?;
        Ok(Some((file.hosts, fp)))
    }

    /// Writes the hosts. If another instance saved in the meantime, its changes are merged in
    /// first (see `merge_hosts`), so neither side loses work.
    pub fn save(&mut self) -> Result<()> {
        let _lock = paths::lock_for(&self.path)?;
        if paths::fingerprint(&self.path) != self.disk
            && let Some((theirs, _)) = self.read_disk().context("hosts.toml changed on disk and cannot be read; not saved")?
        {
            self.hosts = merge_hosts(&self.base, &self.hosts, &theirs);
            self.sort();
        }
        let text = toml::to_string_pretty(&HostsFileRef { hosts: &self.hosts })?;
        paths::write_private(&self.path, text.as_bytes())?;
        self.base = self.hosts.clone();
        self.disk = paths::fingerprint(&self.path);
        self.unreadable = None;
        Ok(())
    }

    /// Adopts what another sshm instance (or a hand edit) changed in the file, keeping this
    /// instance's unsaved changes. Returns whether the host list changed; only a stat when nothing did.
    pub fn reload_if_changed(&mut self) -> Result<bool> {
        let fp = paths::fingerprint(&self.path);
        if fp.is_none() || fp == self.disk || fp == self.unreadable {
            return Ok(false);
        }
        let (theirs, fp) = match self.read_disk() {
            Ok(Some(v)) => v,
            Ok(None) => return Ok(false),
            Err(e) => {
                self.unreadable = fp;
                return Err(e);
            }
        };
        let before = std::mem::take(&mut self.hosts);
        self.hosts = merge_hosts(&self.base, &before, &theirs);
        self.sort();
        self.base = theirs;
        self.disk = Some(fp);
        self.unreadable = None;
        Ok(before != self.hosts)
    }

    pub fn sort(&mut self) {
        self.hosts.sort_by_cached_key(|h| (h.folder.to_lowercase(), h.name.to_lowercase()));
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.id == id)
    }

    pub fn tunnel(&self, host_id: &str, tunnel_id: &str) -> Option<&Tunnel> {
        self.get(host_id)?.tunnels.iter().find(|t| t.id == tunnel_id)
    }

    #[cfg(test)]
    pub fn find_by_path(&self, path: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.path() == path)
    }

    /// Hosts in `folder` or any of its subfolders.
    pub fn hosts_under<'a>(&'a self, folder: &'a str) -> impl Iterator<Item = &'a Host> + 'a {
        self.hosts.iter().filter(move |h| subpath(&h.folder, folder).is_some())
    }

    /// Moves `from`, with its hosts and subfolders, to `to`. Returns how many hosts moved.
    pub fn rename_folder(&mut self, from: &str, to: &str) -> usize {
        let mut moved = 0;
        for h in &mut self.hosts {
            if let Some(rest) = subpath(&h.folder, from) {
                h.folder = format!("{to}{rest}");
                moved += 1;
            }
        }
        self.sort();
        moved
    }

    /// Puts the hosts with these ids in `folder`. Returns how many it found.
    pub fn move_hosts(&mut self, ids: &[String], folder: &str) -> usize {
        let mut moved = 0;
        for h in self.hosts.iter_mut().filter(|h| ids.contains(&h.id)) {
            h.folder = folder.to_string();
            moved += 1;
        }
        self.sort();
        moved
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
        for s in ["none", "vault", "keyring", "bw:srv-01", "rbw:srv 02", "op://Infra/srv/password", "cmd:pass show ssh/x"] {
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

    #[test]
    fn rename_folder_moves_subfolders_only() {
        let mut store = HostStore::default();
        for (name, folder) in [("a", "tef"), ("b", "tef/mx"), ("c", "tefmx"), ("d", "")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: name.into(), ..Host::default() });
        }
        assert_eq!(store.rename_folder("tef", "AAA"), 2);
        let paths: Vec<String> = store.hosts.iter().map(Host::path).collect();
        assert_eq!(paths, ["d", "AAA/a", "AAA/mx/b", "tefmx/c"]);

        // Moving a folder inside itself keeps working off the old paths.
        assert_eq!(store.rename_folder("AAA", "AAA/old"), 2);
        assert_eq!(store.find_by_path("AAA/old/mx/b").map(|h| h.name.as_str()), Some("b"));
    }

    #[test]
    fn rename_target_stays_in_parent() {
        assert_eq!(rename_target("MAT/AT&T Datacom", " AT&T "), Ok("MAT/AT&T".into()));
        assert_eq!(rename_target("Telefonica", "AAA"), Ok("AAA".into()));
        assert!(rename_target("MAT/x", " ").is_err());
        assert!(rename_target("MAT/x", "Lab/x").is_err());
    }

    #[test]
    fn move_target_keeps_the_name() {
        assert_eq!(move_target("MAT/Lab/AT&T", "MAT"), Ok("MAT/AT&T".into()));
        assert_eq!(move_target("MAT/AT&T", ""), Ok("AT&T".into()));
        assert_eq!(move_target("AT&T", " Lab / x "), Ok("Lab/x/AT&T".into()));
        // Into a sibling that only shares the prefix is fine; into itself or below is not.
        assert_eq!(move_target("MAT", "MATx"), Ok("MATx/MAT".into()));
        assert!(move_target("MAT", "MAT").is_err());
        assert!(move_target("MAT", "MAT/AT&T").is_err());
    }

    #[test]
    fn move_hosts_sets_the_folder() {
        let mut store = HostStore::default();
        for (name, folder) in [("a", "x"), ("b", "y"), ("c", "x")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: name.into(), ..Host::default() });
        }
        let ids: Vec<String> = store.hosts.iter().filter(|h| h.name != "b").map(|h| h.id.clone()).collect();
        assert_eq!(store.move_hosts(&ids, "z/w"), 2);
        let paths: Vec<String> = store.hosts.iter().map(Host::path).collect();
        assert_eq!(paths, ["y/b", "z/w/a", "z/w/c"]);
    }

    fn host(name: &str) -> Host {
        Host { name: name.into(), host: format!("{name}.local"), ..Host::default() }
    }

    fn names(store: &HostStore) -> Vec<String> {
        store.hosts.iter().map(|h| h.name.clone()).collect()
    }

    #[test]
    fn two_instances_merge_on_save_and_reload() {
        let dir = std::env::temp_dir().join(format!("sshm-merge-{}", new_id()));
        let path = dir.join("hosts.toml");
        let mut seed = HostStore::load(path.clone()).unwrap();
        for n in ["keep", "gone", "edited"] {
            seed.upsert(host(n));
        }
        seed.save().unwrap();

        let mut a = HostStore::load(path.clone()).unwrap();
        let mut b = HostStore::load(path.clone()).unwrap();
        a.upsert(host("from-a"));
        let gone = a.hosts.iter().find(|h| h.name == "gone").unwrap().id.clone();
        a.remove(&gone);
        a.save().unwrap();

        b.upsert(host("from-b"));
        let edited = b.hosts.iter_mut().find(|h| h.name == "edited").unwrap();
        edited.port = Some(2200);
        b.save().unwrap();
        assert_eq!(names(&b), ["edited", "from-a", "from-b", "keep"]);
        assert_eq!(names(&HostStore::load(path.clone()).unwrap()), names(&b));

        // a picks up b's work without a save; its own unsaved change survives.
        a.upsert(host("unsaved"));
        assert!(a.reload_if_changed().unwrap());
        assert!(!a.reload_if_changed().unwrap());
        assert_eq!(names(&a), ["edited", "from-a", "from-b", "keep", "unsaved"]);
        assert_eq!(a.hosts.iter().find(|h| h.name == "edited").unwrap().port, Some(2200));

        // b deletes a's host; a saving keeps the deletion and adds its own.
        let from_a = b.hosts.iter().find(|h| h.name == "from-a").unwrap().id.clone();
        b.remove(&from_a);
        b.save().unwrap();
        a.save().unwrap();
        assert_eq!(names(&HostStore::load(path.clone()).unwrap()), ["edited", "from-b", "keep", "unsaved"]);

        // A broken hand edit is reported once and never overwritten by a save.
        std::fs::write(&path, "host = 3").unwrap();
        assert!(a.reload_if_changed().is_err());
        assert!(!a.reload_if_changed().unwrap());
        assert!(a.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "host = 3");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn tunnel_specs() {
        let db = Tunnel { bind_port: 5432, dest_host: "db.internal".into(), dest_port: 5432, ..Tunnel::default() };
        assert_eq!(db.forward_args(), ["-L", "5432:db.internal:5432"]);
        assert_eq!(db.summary(), "localhost:5432 → db.internal:5432");
        assert_eq!(db.title(), db.summary());

        let hook = Tunnel {
            name: "webhook".into(),
            kind: TunnelKind::Remote,
            bind_address: "0.0.0.0".into(),
            bind_port: 9000,
            dest_port: 3000,
            ..Tunnel::default()
        };
        assert_eq!(hook.forward_args(), ["-R", "0.0.0.0:9000:localhost:3000"]);
        assert_eq!(hook.title(), "webhook");

        let socks = Tunnel { kind: TunnelKind::Dynamic, bind_address: "::1".into(), bind_port: 1080, dest_port: 9, ..Tunnel::default() };
        assert_eq!(socks.forward_args(), ["-D", "[::1]:1080"]);
        assert_eq!(socks.summary(), "[::1]:1080");
        assert!(socks.validate().is_ok());

        assert!(Tunnel { bind_port: 80, ..Tunnel::default() }.validate().unwrap_err().contains("destination port"));
        assert!(Tunnel::default().validate().is_err());
        assert!(Tunnel { dest_host: "a b".into(), ..db.clone() }.validate().is_err());
    }

    #[test]
    fn tunnels_live_in_hosts_toml() {
        let dir = std::env::temp_dir().join(format!("sshm-test-{}", new_id()));
        let path = dir.join("hosts.toml");
        let mut store = HostStore::load(path.clone()).unwrap();
        let tunnel = Tunnel { name: "pg".into(), bind_port: 5432, dest_port: 5432, start: TunnelStart::Connect, ..Tunnel::default() };
        store.upsert(Host { name: "bastion".into(), host: "b".into(), tunnels: vec![tunnel.clone()], ..Host::default() });
        store.upsert(Host { name: "plain".into(), host: "p".into(), ..Host::default() });
        store.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[host.tunnels]]") && text.contains("start = \"connect\""));
        assert_eq!(text.matches("tunnels").count(), 1, "hosts without tunnels leave the key out:\n{text}");
        let loaded = HostStore::load(path).unwrap();
        assert_eq!(loaded.hosts, store.hosts);
        let id = loaded.hosts[0].id.clone();
        assert_eq!(loaded.tunnel(&id, &tunnel.id), Some(&tunnel));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn display_target_skips_an_empty_user() {
        let h = Host { host: "srv".into(), user: Some(String::new()), port: Some(22), ..Host::default() };
        assert_eq!(h.display_target(), "srv:22");
        assert_eq!(Host { user: Some("me".into()), ..h }.display_target(), "me@srv:22");
    }

    #[test]
    fn normalize_folder_trims_segments() {
        assert_eq!(normalize_folder(" a / /b/ "), "a/b");
        assert_eq!(normalize_folder(" / "), "");
    }
}
