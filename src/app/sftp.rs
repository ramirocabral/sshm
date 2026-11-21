use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use ratatui::layout::Position;
use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, FileType, Status, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use super::dialogs::*;
use super::widgets::TextInput;
use super::{App, AppEvent, Focus, Owner, Tab, TabKind};
use crate::ssh::command::{self, AskPassEnv, Purpose};
use crate::store::hosts::new_id;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub mtime: Option<u64>,
}

#[derive(Default)]
pub struct Panel {
    pub cwd: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    pub scroll: usize,
    pub marked: HashSet<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// Bumped by every listing request; replies to older ones are dropped.
    request: u64,
}

impl Panel {
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    fn set_entries(&mut self, path: String, mut entries: Vec<Entry>, show_hidden: bool, keep: Option<&str>) {
        if path != self.cwd {
            self.marked.clear();
            self.scroll = 0;
        }
        self.cwd = path;
        if !show_hidden {
            entries.retain(|e| !e.name.starts_with('.'));
        }
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        if self.cwd != "/" {
            entries.insert(0, Entry { name: "..".into(), is_dir: true, is_link: false, size: 0, mtime: None });
        }
        self.entries = entries;
        self.marked.retain(|m| self.entries.iter().any(|e| &e.name == m));
        self.selected = keep.and_then(|k| self.entries.iter().position(|e| e.name == k)).unwrap_or(0);
        self.loading = false;
        self.error = None;
    }

    /// Selected names (marked ones, or the one under the cursor), never "..".
    pub fn targets(&self) -> Vec<String> {
        if !self.marked.is_empty() {
            let mut v: Vec<String> = self.marked.iter().cloned().collect();
            v.sort();
            return v;
        }
        self.current().filter(|e| e.name != "..").map(|e| vec![e.name.clone()]).unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TransferState {
    Queued,
    /// Walking the source to find what to copy.
    Scanning,
    Running,
    Done,
    Failed(String),
}

pub struct Transfer {
    pub id: u64,
    pub label: String,
    pub upload: bool,
    pub done: u64,
    pub total: u64,
    pub state: TransferState,
    /// When the first byte was about to move (the speed leaves out the queue and the scan).
    pub started: Option<Instant>,
    /// What the transfer left out, e.g. links to folders.
    pub note: Option<String>,
}

pub enum Job {
    Upload { id: u64, local: PathBuf, remote: String },
    Download { id: u64, remote: String, local: PathBuf },
}

pub enum SftpEvent {
    Connected { session: Arc<SftpSession>, home: String },
    Failed(String),
    Listing { remote: bool, request: u64, path: String, result: Result<Vec<Entry>, String>, keep: Option<String> },
    Scanning { id: u64 },
    Progress { id: u64, done: u64, total: u64 },
    TransferDone { id: u64, error: Option<String>, skipped: Vec<String> },
    OpDone { remote: bool, result: Result<String, String> },
}

pub struct SftpView {
    pub host_id: String,
    pub name: String,
    pub token: String,
    pub local: Panel,
    pub remote: Panel,
    pub remote_active: bool,
    pub session: Option<Arc<SftpSession>>,
    pub status: String,
    pub jobs: Option<UnboundedSender<Job>>,
    pub transfers: Vec<Transfer>,
    pub show_hidden: bool,
    child: Option<tokio::process::Child>,
    /// Sides that received files since the last refresh; refreshed once the queue drains.
    stale_local: bool,
    stale_remote: bool,
}

impl SftpView {
    pub fn panel(&self, remote: bool) -> &Panel {
        if remote { &self.remote } else { &self.local }
    }

    pub fn panel_mut(&mut self, remote: bool) -> &mut Panel {
        if remote { &mut self.remote } else { &mut self.local }
    }

    fn pending_transfers(&self) -> bool {
        self.transfers.iter().any(|t| matches!(t.state, TransferState::Queued | TransferState::Scanning | TransferState::Running))
    }
}

impl Drop for SftpView {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.start_kill();
        }
    }
}

pub fn join_remote(dir: &str, name: &str) -> String {
    if name == ".." {
        return parent_remote(dir);
    }
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

pub fn parent_remote(dir: &str) -> String {
    let trimmed = dir.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", _)) | None => "/".into(),
        Some((p, _)) => p.into(),
    }
}

/// Readable text for an SFTP error. Servers usually send just the status name ("Failure").
pub fn describe(e: &SftpError) -> String {
    let SftpError::Status(st) = e else { return e.to_string() };
    let generic = match st.status_code {
        StatusCode::Ok => "ok",
        StatusCode::Eof => "unexpected end of file",
        StatusCode::NoSuchFile => "no such file or directory",
        StatusCode::PermissionDenied => "permission denied",
        StatusCode::Failure => "the server refused without giving a reason",
        StatusCode::BadMessage => "the server rejected the request as malformed",
        StatusCode::NoConnection | StatusCode::ConnectionLost => "connection lost",
        StatusCode::OpUnsupported => "not supported by the server",
    };
    let msg = st.error_message.trim();
    let bare = msg.is_empty()
        || msg.eq_ignore_ascii_case(&st.status_code.to_string())
        || (st.status_code == StatusCode::Eof && msg.eq_ignore_ascii_case("end of file"));
    if bare { generic.into() } else { msg.into() }
}

const ST_RDONLY: u64 = 0x1;

/// Why a change inside the remote `dir` failed. OpenSSH reports a read-only or full
/// filesystem as a bare "Failure", so statvfs is asked when the server supports it.
async fn write_error(s: &SftpSession, dir: &str, e: SftpError) -> String {
    if let SftpError::Status(st) = &e
        && st.status_code == StatusCode::Failure
        && let Ok(Some(fs)) = s.fs_info(dir).await
    {
        if fs.flags & ST_RDONLY != 0 {
            return "read-only filesystem".into();
        }
        if fs.blocks_avail == 0 {
            return "no space left on the device".into();
        }
    }
    describe(&e)
}

const STATUS_CODES: [StatusCode; 9] = [
    StatusCode::Ok,
    StatusCode::Eof,
    StatusCode::NoSuchFile,
    StatusCode::PermissionDenied,
    StatusCode::Failure,
    StatusCode::BadMessage,
    StatusCode::NoConnection,
    StatusCode::ConnectionLost,
    StatusCode::OpUnsupported,
];

/// The SFTP status behind an io::Error from a remote file: either wrapped, or only the
/// server's message.
fn status_of(e: &std::io::Error) -> Option<SftpError> {
    match e.get_ref().and_then(|i| i.downcast_ref::<SftpError>()) {
        Some(SftpError::Status(st)) => Some(SftpError::Status(st.clone())),
        Some(_) => None,
        None => {
            let msg = e.to_string();
            let status_code = STATUS_CODES.into_iter().find(|c| c.to_string() == msg)?;
            Some(SftpError::Status(Status { id: 0, status_code, error_message: msg, language_tag: String::new() }))
        }
    }
}

fn describe_io(e: &std::io::Error) -> String {
    status_of(e).map(|s| describe(&s)).unwrap_or_else(|| e.to_string())
}

async fn io_error(s: &SftpSession, dir: &str, e: std::io::Error) -> String {
    match status_of(&e) {
        Some(se) => write_error(s, dir, se).await,
        None => e.to_string(),
    }
}

fn list_local(dir: &str) -> Result<Vec<Entry>, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("{dir}: {e}"))?;
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let link = e.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let meta = std::fs::metadata(e.path()).or_else(|_| e.metadata());
        let (is_dir, size, mtime) = match meta {
            Ok(m) => {
                let mtime = m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs());
                (m.is_dir(), m.len(), mtime)
            }
            Err(_) => (false, 0, None),
        };
        out.push(Entry { name, is_dir, is_link: link, size, mtime });
    }
    Ok(out)
}

/// The file type of remote metadata; servers that leave the mode out get "file".
fn remote_type(m: &FileAttributes) -> FileType {
    if m.permissions.is_none() { FileType::File } else { m.file_type() }
}

/// What each symlink among `entries` points to (None for the rest, or when it cannot be resolved).
async fn stat_links(s: &SftpSession, dir: &str, entries: &[(String, FileAttributes)]) -> Vec<Option<FileAttributes>> {
    let mut out = vec![None; entries.len()];
    let mut links = Vec::new();
    for (i, (name, m)) in entries.iter().enumerate() {
        if remote_type(m) == FileType::Symlink {
            links.push((i, join_remote(dir, name)));
        }
    }
    let mut links = links.into_iter();
    let mut running = FuturesUnordered::new();
    loop {
        while running.len() < 16 {
            let Some((i, path)) = links.next() else { break };
            running.push(async move { (i, s.metadata(path).await.ok()) });
        }
        let Some((i, m)) = running.next().await else { break };
        out[i] = m;
    }
    out
}

/// Directory contents with their lstat metadata, without "." and "..".
async fn read_remote_dir(s: &SftpSession, dir: &str) -> Result<Vec<(String, FileAttributes)>, String> {
    let rd = s.read_dir(dir).await.map_err(|e| format!("{dir}: {}", describe(&e)))?;
    Ok(rd.map(|e| (e.file_name(), e.metadata())).filter(|(name, _)| name != "." && name != "..").collect())
}

async fn list_remote(s: &SftpSession, dir: &str) -> Result<Vec<Entry>, String> {
    let raw = read_remote_dir(s, dir).await?;
    let targets = stat_links(s, dir, &raw).await;
    Ok(raw
        .into_iter()
        .zip(targets)
        .map(|((name, meta), target)| {
            let is_link = remote_type(&meta) == FileType::Symlink;
            let shown = target.as_ref().unwrap_or(&meta);
            let is_dir = remote_type(shown) == FileType::Dir;
            Entry { name, is_dir, is_link, size: shown.size.unwrap_or(0), mtime: meta.mtime.map(u64::from) }
        })
        .collect())
}

/// One item found while planning a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File(u64),
    Dir,
    LinkToFile(u64),
    LinkToDir,
    BrokenLink,
    Other,
}

fn local_kind(path: &Path, ft: std::fs::FileType) -> Kind {
    if ft.is_symlink() {
        return match std::fs::metadata(path) {
            Ok(m) if m.is_file() => Kind::LinkToFile(m.len()),
            Ok(m) if m.is_dir() => Kind::LinkToDir,
            Ok(_) => Kind::Other,
            Err(_) => Kind::BrokenLink,
        };
    }
    if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        std::fs::symlink_metadata(path).map(|m| Kind::File(m.len())).unwrap_or(Kind::BrokenLink)
    } else {
        Kind::Other
    }
}

/// `lstat` is what the directory listing reported; `target` is the stat of a symlink.
fn remote_kind(lstat: &FileAttributes, target: Option<&FileAttributes>) -> Kind {
    match remote_type(lstat) {
        FileType::Dir => Kind::Dir,
        FileType::File => Kind::File(lstat.size.unwrap_or(0)),
        FileType::Other => Kind::Other,
        FileType::Symlink => match target.map(remote_type) {
            Some(FileType::File) => Kind::LinkToFile(target.and_then(|t| t.size).unwrap_or(0)),
            Some(FileType::Dir) => Kind::LinkToDir,
            Some(_) => Kind::Other,
            None => Kind::BrokenLink,
        },
    }
}

/// What a transfer copies, worked out before the first byte moves: folders to create (parents
/// first) and files with their sizes. The selected item is followed if it is a link; below it,
/// links to files are copied as files and links to folders are skipped, so nothing loops.
#[derive(Debug, PartialEq)]
struct Plan<S, D> {
    dirs: Vec<D>,
    files: Vec<(S, D, u64)>,
    skipped: Vec<String>,
    total: u64,
}

impl<S, D> Plan<S, D> {
    fn new() -> Self {
        Plan { dirs: Vec::new(), files: Vec::new(), skipped: Vec::new(), total: 0 }
    }

    /// Records one item (`rel` names it in the report); a folder is handed back to walk into.
    fn add(&mut self, kind: Kind, src: S, dst: D, rel: &str) -> Option<(S, D)> {
        match kind {
            Kind::File(n) | Kind::LinkToFile(n) => {
                self.files.push((src, dst, n));
                self.total += n;
            }
            Kind::Dir => return Some((src, dst)),
            Kind::LinkToDir => self.skipped.push(format!("{rel}/ (link to a folder)")),
            Kind::BrokenLink => self.skipped.push(format!("{rel} (broken link)")),
            Kind::Other => self.skipped.push(format!("{rel} (not a regular file)")),
        }
        None
    }
}

fn base_name(path: &str) -> String {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_string()
}

/// Blocking: call it through spawn_blocking.
fn plan_upload(local: &Path, remote: &str) -> Result<Plan<PathBuf, String>, String> {
    let meta = std::fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let mut plan = Plan::new();
    if meta.is_file() {
        plan.add(Kind::File(meta.len()), local.to_path_buf(), remote.to_string(), "");
        return Ok(plan);
    }
    if !meta.is_dir() {
        return Err(format!("{}: not a regular file or folder", local.display()));
    }
    let mut stack = vec![(local.to_path_buf(), remote.to_string(), base_name(&local.to_string_lossy()))];
    while let Some((dir, rdir, rel)) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        plan.dirs.push(rdir.clone());
        let mut subdirs = Vec::new();
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let kind = e.file_type().map(|ft| local_kind(&path, ft)).unwrap_or(Kind::BrokenLink);
            let child_rel = format!("{rel}/{name}");
            if let Some((src, dst)) = plan.add(kind, path, join_remote(&rdir, &name), &child_rel) {
                subdirs.push((src, dst, child_rel));
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(plan)
}

async fn plan_download(s: &SftpSession, remote: &str, local: &Path) -> Result<Plan<String, PathBuf>, String> {
    let meta = s.metadata(remote).await.map_err(|e| format!("{remote}: {}", describe(&e)))?;
    let mut plan = Plan::new();
    match remote_type(&meta) {
        FileType::File => {
            plan.add(Kind::File(meta.size.unwrap_or(0)), remote.to_string(), local.to_path_buf(), "");
            return Ok(plan);
        }
        FileType::Dir => {}
        _ => return Err(format!("{remote}: not a regular file or folder")),
    }
    let mut stack = vec![(remote.to_string(), local.to_path_buf(), base_name(remote))];
    while let Some((dir, ldir, rel)) = stack.pop() {
        let mut entries = read_remote_dir(s, &dir).await?;
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let targets = stat_links(s, &dir, &entries).await;
        plan.dirs.push(ldir.clone());
        let mut subdirs = Vec::new();
        for ((name, lstat), target) in entries.into_iter().zip(targets) {
            let child_rel = format!("{rel}/{name}");
            let kind = remote_kind(&lstat, target.as_ref());
            if let Some((src, dst)) = plan.add(kind, join_remote(&dir, &name), ldir.join(&name), &child_rel) {
                subdirs.push((src, dst, child_rel));
            }
        }
        stack.extend(subdirs.into_iter().rev());
    }
    Ok(plan)
}

fn send(tx: &UnboundedSender<AppEvent>, tab: u64, ev: SftpEvent) {
    let _ = tx.send(AppEvent::Sftp(tab, ev));
}

impl App {
    pub fn open_sftp(&mut self, host_id: &str) {
        let Some(host) = self.store.get(host_id).cloned() else { return };
        let tab_id = self.next_id();
        let token = new_id();
        self.owners.insert(
            token.clone(),
            Owner { host_id: host.id.clone(), label: format!("{} (sftp)", host.name), password_attempts: 0, passphrase_attempts: 0 },
        );
        let ask = AskPassEnv { socket: self.askpass_sock.clone(), token: token.clone() };
        let spec = command::build(&host, &self.cfg, Some(&ask), Some(&self.runtime_dir), Purpose::Sftp);
        let mut cmd = tokio::process::Command::new(&spec.program);
        cmd.args(&spec.args).envs(spec.env.iter().cloned()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let home = crate::paths::home().to_string_lossy().into_owned();
        let mut view = SftpView {
            host_id: host.id.clone(),
            name: host.name.clone(),
            token,
            local: Panel { cwd: home, loading: true, ..Panel::default() },
            remote: Panel { cwd: String::new(), loading: true, ..Panel::default() },
            remote_active: true,
            session: None,
            status: format!("Connecting to {}…", host.display_target()),
            jobs: None,
            transfers: Vec::new(),
            show_hidden: false,
            child: None,
            stale_local: false,
            stale_remote: false,
        };

        match cmd.spawn() {
            Ok(mut child) => {
                let stdin = child.stdin.take();
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                view.child = Some(child);
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let (Some(stdin), Some(stdout)) = (stdin, stdout) else {
                        send(&tx, tab_id, SftpEvent::Failed("could not open the ssh channel".into()));
                        return;
                    };
                    let stderr_task = tokio::spawn(async move {
                        let mut buf = String::new();
                        if let Some(mut e) = stderr {
                            let _ = e.read_to_string(&mut buf).await;
                        }
                        buf
                    });
                    let stream = tokio::io::join(stdout, stdin);
                    match tokio::time::timeout(Duration::from_secs(120), SftpSession::new(stream)).await {
                        Ok(Ok(session)) => {
                            let home = session.canonicalize(".").await.unwrap_or_else(|_| "/".into());
                            send(&tx, tab_id, SftpEvent::Connected { session: Arc::new(session), home });
                        }
                        Ok(Err(e)) => {
                            let err = stderr_task.await.unwrap_or_default();
                            let msg = if err.trim().is_empty() { e.to_string() } else { err.trim().to_string() };
                            send(&tx, tab_id, SftpEvent::Failed(msg));
                        }
                        Err(_) => send(&tx, tab_id, SftpEvent::Failed("connection timed out".into())),
                    }
                });
            }
            Err(e) => view.status = format!("Could not run {}: {e}", spec.program),
        }

        self.tabs.push(Tab { id: tab_id, title: None, kind: TabKind::Sftp(Box::new(view)) });
        self.active = self.tabs.len() - 1;
        self.focus = Focus::Main;
        self.sftp_refresh(tab_id, false, None);
    }

    fn sftp_view(&mut self, tab: u64) -> Option<&mut SftpView> {
        self.tabs.iter_mut().find(|t| t.id == tab).and_then(|t| match &mut t.kind {
            TabKind::Sftp(v) => Some(v.as_mut()),
            _ => None,
        })
    }

    fn active_sftp(&mut self) -> Option<(u64, &mut SftpView)> {
        let tab = self.tabs.get_mut(self.active)?;
        let id = tab.id;
        match &mut tab.kind {
            TabKind::Sftp(v) => Some((id, v.as_mut())),
            _ => None,
        }
    }

    pub fn handle_sftp_event(&mut self, tab: u64, ev: SftpEvent) {
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        match ev {
            SftpEvent::Connected { session, home } => {
                view.status = format!("Connected to {}", view.name);
                view.session = Some(session.clone());
                view.remote.cwd = home;
                let (jtx, mut jrx) = unbounded_channel::<Job>();
                view.jobs = Some(jtx);
                let worker_tx = tx.clone();
                let worker_session = session.clone();
                tokio::spawn(async move {
                    while let Some(job) = jrx.recv().await {
                        let (id, result) = match job {
                            Job::Upload { id, local, remote } => (id, upload(&worker_session, &local, &remote, id, tab, &worker_tx).await),
                            Job::Download { id, remote, local } => (id, download(&worker_session, &remote, &local, id, tab, &worker_tx).await),
                        };
                        let (error, skipped) = match result {
                            Ok(skipped) => (None, skipped),
                            Err(e) => (Some(e), Vec::new()),
                        };
                        send(&worker_tx, tab, SftpEvent::TransferDone { id, error, skipped });
                    }
                });
                self.sftp_refresh(tab, true, None);
            }
            SftpEvent::Failed(msg) => {
                view.status = format!("Error: {msg}");
                view.remote.loading = false;
                view.remote.error = Some(msg);
            }
            SftpEvent::Listing { remote, request, path, result, keep } => {
                let show_hidden = view.show_hidden;
                let panel = view.panel_mut(remote);
                if request != panel.request {
                    return;
                }
                match result {
                    Ok(entries) => panel.set_entries(path, entries, show_hidden, keep.as_deref()),
                    Err(e) => {
                        panel.loading = false;
                        if panel.entries.is_empty() {
                            panel.error = Some(e.clone());
                        }
                        view.status = format!("Error: {e}");
                    }
                }
            }
            SftpEvent::Scanning { id } => {
                if let Some(t) = view.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = TransferState::Scanning;
                }
            }
            SftpEvent::Progress { id, done, total } => {
                if let Some(t) = view.transfers.iter_mut().find(|t| t.id == id) {
                    t.done = done;
                    t.total = total;
                    t.state = TransferState::Running;
                    t.started.get_or_insert_with(Instant::now);
                }
            }
            SftpEvent::TransferDone { id, error, skipped } => {
                if let Some(t) = view.transfers.iter_mut().find(|t| t.id == id) {
                    t.state = match &error {
                        Some(e) => TransferState::Failed(e.clone()),
                        None => {
                            t.done = t.total;
                            TransferState::Done
                        }
                    };
                    if !skipped.is_empty() {
                        t.note = Some(format!("skipped {}: {}", skipped.len(), skipped.join(", ")));
                    }
                    if t.upload {
                        view.stale_remote = true;
                    } else {
                        view.stale_local = true;
                    }
                }
                let pending = view.pending_transfers();
                if let Some(e) = error {
                    view.status = format!("Transfer failed: {e}");
                } else if !skipped.is_empty() {
                    view.status = format!("Skipped {}: {}", skipped.len(), skipped.join(", "));
                } else if !pending {
                    view.status = "Transfers finished".into();
                }
                if !pending {
                    let remote = std::mem::take(&mut view.stale_remote);
                    let local = std::mem::take(&mut view.stale_local);
                    if remote {
                        self.sftp_refresh(tab, true, None);
                    }
                    if local {
                        self.sftp_refresh(tab, false, None);
                    }
                }
            }
            SftpEvent::OpDone { remote, result } => {
                match result {
                    Ok(msg) => view.status = msg,
                    Err(e) => view.status = format!("Error: {e}"),
                }
                self.sftp_refresh(tab, remote, None);
            }
        }
    }

    /// Lists the current folder of one side again, keeping the cursor on the same name.
    pub fn sftp_refresh(&mut self, tab: u64, remote: bool, keep: Option<String>) {
        let Some(view) = self.sftp_view(tab) else { return };
        let panel = view.panel(remote);
        let keep = keep.or_else(|| panel.current().map(|e| e.name.clone()));
        let path = panel.cwd.clone();
        self.sftp_list(tab, remote, path, keep);
    }

    /// Lists `path` in the background; the panel moves there once the listing arrives.
    fn sftp_list(&mut self, tab: u64, remote: bool, path: String, keep: Option<String>) {
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        let session = view.session.clone();
        if remote && session.is_none() {
            return;
        }
        let panel = view.panel_mut(remote);
        panel.request += 1;
        panel.loading = true;
        let request = panel.request;
        match session.filter(|_| remote) {
            Some(session) => {
                tokio::spawn(async move {
                    let result = list_remote(&session, &path).await;
                    send(&tx, tab, SftpEvent::Listing { remote, request, path, result, keep });
                });
            }
            None => {
                tokio::task::spawn_blocking(move || {
                    let result = list_local(&path);
                    send(&tx, tab, SftpEvent::Listing { remote, request, path, result, keep });
                });
            }
        }
    }

    fn sftp_enter(&mut self, tab: u64) {
        let Some(view) = self.sftp_view(tab) else { return };
        let remote = view.remote_active;
        let panel = view.panel(remote);
        let Some(entry) = panel.current().cloned() else { return };
        if !entry.is_dir {
            return;
        }
        let old_cwd = panel.cwd.clone();
        let came_from = (entry.name == "..").then(|| old_cwd.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string());
        let new_cwd = if remote {
            join_remote(&old_cwd, &entry.name)
        } else {
            let p = if entry.name == ".." {
                Path::new(&old_cwd).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("/"))
            } else {
                Path::new(&old_cwd).join(&entry.name)
            };
            p.to_string_lossy().into_owned()
        };
        self.sftp_list(tab, remote, new_cwd, came_from);
    }

    fn sftp_up(&mut self, tab: u64) {
        let Some(view) = self.sftp_view(tab) else { return };
        let remote = view.remote_active;
        let panel = view.panel_mut(remote);
        if panel.cwd == "/" {
            return;
        }
        if let Some(pos) = panel.entries.iter().position(|e| e.name == "..") {
            panel.selected = pos;
            self.sftp_enter(tab);
        }
    }

    pub fn sftp_goto(&mut self, tab: u64, remote: bool, path: String) {
        if path.is_empty() {
            return;
        }
        let path = if remote { path } else { crate::paths::expand_tilde(&path).to_string_lossy().into_owned() };
        self.sftp_list(tab, remote, path, None);
    }

    pub fn sftp_key(&mut self, key: KeyEvent) {
        let Some((tab, view)) = self.active_sftp() else { return };
        let remote = view.remote_active;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let len = view.panel(remote).entries.len();
        let mv = |view: &mut SftpView, d: isize| {
            let p = view.panel_mut(remote);
            if len > 0 {
                p.selected = (p.selected as isize + d).clamp(0, len as isize - 1) as usize;
            }
        };
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => view.remote_active = !view.remote_active,
            KeyCode::Char('j') | KeyCode::Down => mv(view, 1),
            KeyCode::Char('k') | KeyCode::Up => mv(view, -1),
            KeyCode::PageDown => mv(view, 15),
            KeyCode::PageUp => mv(view, -15),
            KeyCode::Char('d') if ctrl => mv(view, 15),
            KeyCode::Char('u') if ctrl => mv(view, -15),
            KeyCode::Char('g') | KeyCode::Home => view.panel_mut(remote).selected = 0,
            KeyCode::Char('G') | KeyCode::End => view.panel_mut(remote).selected = len.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.sftp_enter(tab),
            KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => self.sftp_up(tab),
            KeyCode::Char(' ') => {
                let p = view.panel_mut(remote);
                if let Some(name) = p.current().map(|e| e.name.clone()).filter(|n| n != "..")
                    && !p.marked.remove(&name) {
                        p.marked.insert(name);
                    }
                mv(view, 1);
            }
            KeyCode::Char('.') => {
                view.show_hidden = !view.show_hidden;
                self.sftp_refresh(tab, true, None);
                self.sftp_refresh(tab, false, None);
            }
            KeyCode::Char('r') | KeyCode::F(2) if !ctrl => {
                self.sftp_refresh(tab, true, None);
                self.sftp_refresh(tab, false, None);
            }
            KeyCode::F(5) | KeyCode::Char('c') => {
                let sources = view.panel(remote).targets();
                if sources.is_empty() {
                    return;
                }
                self.sftp_transfer_checked(tab, !remote, sources);
            }
            KeyCode::F(6) | KeyCode::Char('m') => {
                if let Some(name) = view.panel(remote).current().map(|e| e.name.clone()).filter(|n| n != "..") {
                    self.push_dialog(Dialog::Input(InputDialog {
                        title: format!("Rename {name}"),
                        input: TextInput::new(name.clone()),
                        action: Some(InputAction::SftpRename { tab, remote, from: name }),
                    }));
                }
            }
            KeyCode::F(7) | KeyCode::Char('n') => self.push_dialog(Dialog::Input(InputDialog {
                title: "New folder".into(),
                input: TextInput::default(),
                action: Some(InputAction::SftpMkdir { tab, remote }),
            })),
            KeyCode::F(8) | KeyCode::Char('d') | KeyCode::Delete => {
                let paths = view.panel(remote).targets();
                if paths.is_empty() {
                    return;
                }
                let side = if remote { "remote" } else { "local" };
                let body = if paths.len() == 1 {
                    format!("Delete {} ({side})? Folders are deleted with everything inside.", paths[0])
                } else {
                    format!("Delete {} items ({side})? Folders are deleted with everything inside.", paths.len())
                };
                self.push_dialog(Dialog::Confirm(ConfirmDialog {
                    title: "Delete".into(),
                    body,
                    action: Some(ConfirmAction::SftpDelete { tab, remote, paths }),
                }));
            }
            KeyCode::Char('/') | KeyCode::Char('o') => {
                let cwd = view.panel(remote).cwd.clone();
                self.push_dialog(Dialog::Input(InputDialog {
                    title: if remote { "Go to (remote)".into() } else { "Go to (local)".into() },
                    input: TextInput::new(cwd),
                    action: Some(InputAction::SftpGoto { tab, remote }),
                }));
            }
            KeyCode::Char('x') => view.transfers.retain(|t| matches!(t.state, TransferState::Queued | TransferState::Scanning | TransferState::Running)),
            KeyCode::Char('q') | KeyCode::Esc => {
                if !view.panel(remote).marked.is_empty() {
                    view.panel_mut(remote).marked.clear();
                } else if key.code == KeyCode::Char('q') {
                    self.remove_tab(self.active);
                }
            }
            _ => {}
        }
    }

    /// Asks before overwriting files that already exist on the destination side.
    fn sftp_transfer_checked(&mut self, tab: u64, upload: bool, sources: Vec<String>) {
        let Some(view) = self.sftp_view(tab) else { return };
        let dest = view.panel(upload);
        let clashes: Vec<&String> = sources.iter().filter(|s| dest.entries.iter().any(|e| &e.name == *s)).collect();
        if clashes.is_empty() {
            self.sftp_transfer(tab, upload, sources);
        } else {
            let body = format!(
                "Already exist{} at the destination: {}. Overwrite?",
                if clashes.len() > 1 { "" } else { "s" },
                clashes.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
            self.push_dialog(Dialog::Confirm(ConfirmDialog {
                title: "Overwrite".into(),
                body,
                action: Some(ConfirmAction::SftpOverwrite { tab, upload, sources }),
            }));
        }
    }

    pub fn sftp_transfer(&mut self, tab: u64, upload: bool, sources: Vec<String>) {
        let mut ids = Vec::new();
        for _ in &sources {
            ids.push(self.next_id());
        }
        let Some(view) = self.sftp_view(tab) else { return };
        let Some(jobs) = view.jobs.clone() else {
            view.status = "SFTP is not connected yet".into();
            return;
        };
        let (local_dir, remote_dir) = (view.local.cwd.clone(), view.remote.cwd.clone());
        for (name, id) in sources.into_iter().zip(ids) {
            let local = Path::new(&local_dir).join(&name);
            let remote = join_remote(&remote_dir, &name);
            view.transfers.push(Transfer {
                id,
                label: name.clone(),
                upload,
                done: 0,
                total: 0,
                state: TransferState::Queued,
                started: None,
                note: None,
            });
            let job = if upload { Job::Upload { id, local, remote } } else { Job::Download { id, remote, local } };
            let _ = jobs.send(job);
        }
        view.local.marked.clear();
        view.remote.marked.clear();
        view.status = if upload { "Uploading…".into() } else { "Downloading…".into() };
    }

    pub fn sftp_mkdir(&mut self, tab: u64, remote: bool, name: String) {
        if name.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        if remote {
            let Some(s) = view.session.clone() else { return };
            let cwd = view.remote.cwd.clone();
            let path = join_remote(&cwd, &name);
            tokio::spawn(async move {
                let result = match s.create_dir(path.clone()).await {
                    Ok(_) => Ok(format!("Folder created: {path}")),
                    Err(e) => Err(format!("{path}: {}", write_error(&s, &cwd, e).await)),
                };
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        } else {
            let path = Path::new(&view.local.cwd).join(&name);
            tokio::task::spawn_blocking(move || {
                let result = std::fs::create_dir_all(&path).map(|_| format!("Folder created: {}", path.display())).map_err(|e| e.to_string());
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        }
    }

    pub fn sftp_rename(&mut self, tab: u64, remote: bool, from: String, to: String) {
        if to.is_empty() || to == from {
            return;
        }
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        if remote {
            let Some(s) = view.session.clone() else { return };
            let cwd = view.remote.cwd.clone();
            let a = join_remote(&cwd, &from);
            let b = if to.starts_with('/') { to.clone() } else { join_remote(&cwd, &to) };
            tokio::spawn(async move {
                let result = match s.rename(a.clone(), b.clone()).await {
                    Ok(_) => Ok(format!("Renamed to {b}")),
                    Err(e) => Err(format!("{a}: {}", write_error(&s, &cwd, e).await)),
                };
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        } else {
            let a = Path::new(&view.local.cwd).join(&from);
            let b = Path::new(&view.local.cwd).join(&to);
            tokio::task::spawn_blocking(move || {
                let result = std::fs::rename(&a, &b).map(|_| format!("Renamed to {}", b.display())).map_err(|e| e.to_string());
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        }
    }

    pub fn sftp_delete(&mut self, tab: u64, remote: bool, names: Vec<String>) {
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        if remote {
            let Some(s) = view.session.clone() else { return };
            let cwd = view.remote.cwd.clone();
            tokio::spawn(async move {
                let mut errors = Vec::new();
                for n in &names {
                    if let Err(e) = remove_remote(&s, &join_remote(&cwd, n)).await {
                        errors.push(format!("{n}: {e}"));
                    }
                }
                let result = if errors.is_empty() { Ok(format!("Deleted: {}", names.len())) } else { Err(errors.join("; ")) };
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        } else {
            let cwd = PathBuf::from(&view.local.cwd);
            tokio::task::spawn_blocking(move || {
                let mut errors = Vec::new();
                for n in &names {
                    let p = cwd.join(n);
                    let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    if let Err(e) = r {
                        errors.push(format!("{n}: {e}"));
                    }
                }
                let result = if errors.is_empty() { Ok(format!("Deleted: {}", names.len())) } else { Err(errors.join("; ")) };
                send(&tx, tab, SftpEvent::OpDone { remote, result });
            });
        }
    }

    pub fn sftp_mouse(&mut self, m: MouseEvent) {
        let panels = self.layout.sftp_panels.clone();
        let Some((tab, view)) = self.active_sftp() else { return };
        let Some(&(remote, rect)) = panels.iter().find(|(_, r)| r.contains(Position::new(m.column, m.row))) else { return };
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                view.remote_active = remote;
                let p = view.panel_mut(remote);
                let idx = p.scroll + (m.row - rect.y) as usize;
                if idx < p.entries.len() {
                    let double = p.selected == idx;
                    p.selected = idx;
                    if double {
                        self.sftp_enter(tab);
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let p = view.panel_mut(remote);
                let len = p.entries.len();
                let d: isize = if m.kind == MouseEventKind::ScrollUp { -3 } else { 3 };
                if len > 0 {
                    p.selected = (p.selected as isize + d).clamp(0, len as isize - 1) as usize;
                }
            }
            _ => {}
        }
    }
}

fn remove_remote<'a>(s: &'a SftpSession, path: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let meta = s.symlink_metadata(path).await.map_err(|e| describe(&e))?;
        let removed = if remote_type(&meta) == FileType::Dir {
            for (name, _) in read_remote_dir(s, path).await? {
                remove_remote(s, &join_remote(path, &name)).await?;
            }
            s.remove_dir(path).await
        } else {
            s.remove_file(path).await
        };
        match removed {
            Ok(()) => Ok(()),
            Err(e) => Err(write_error(s, &parent_remote(path), e).await),
        }
    })
}

struct Progress<'a> {
    tx: &'a UnboundedSender<AppEvent>,
    tab: u64,
    id: u64,
    done: u64,
    total: u64,
    last: Instant,
}

impl Progress<'_> {
    fn start(tx: &UnboundedSender<AppEvent>, tab: u64, id: u64, total: u64) -> Progress<'_> {
        send(tx, tab, SftpEvent::Progress { id, done: 0, total });
        Progress { tx, tab, id, done: 0, total, last: Instant::now() }
    }

    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() > Duration::from_millis(150) {
            self.last = Instant::now();
            send(self.tx, self.tab, SftpEvent::Progress { id: self.id, done: self.done, total: self.total });
        }
    }
}

/// Returns what was skipped (links to folders, special files).
async fn upload(s: &SftpSession, local: &Path, remote: &str, id: u64, tab: u64, tx: &UnboundedSender<AppEvent>) -> Result<Vec<String>, String> {
    send(tx, tab, SftpEvent::Scanning { id });
    let (l, r) = (local.to_path_buf(), remote.to_string());
    let plan = tokio::task::spawn_blocking(move || plan_upload(&l, &r)).await.map_err(|e| e.to_string())??;
    let mut prog = Progress::start(tx, tab, id, plan.total);
    for dir in &plan.dirs {
        ensure_remote_dir(s, dir).await?;
    }
    for (src, dst, _) in &plan.files {
        upload_file(s, src, dst, &mut prog).await?;
    }
    Ok(plan.skipped)
}

/// Creates `dir` unless a folder is already there.
async fn ensure_remote_dir(s: &SftpSession, dir: &str) -> Result<(), String> {
    let Err(e) = s.create_dir(dir).await else { return Ok(()) };
    match s.metadata(dir).await {
        Ok(m) if remote_type(&m) == FileType::Dir => Ok(()),
        _ => Err(format!("{dir}: {}", write_error(s, &parent_remote(dir), e).await)),
    }
}

async fn upload_file(s: &SftpSession, local: &Path, remote: &str, prog: &mut Progress<'_>) -> Result<(), String> {
    let parent = parent_remote(remote);
    let mut src = tokio::fs::File::open(local).await.map_err(|e| format!("{}: {e}", local.display()))?;
    let mut dst = match s.create(remote).await {
        Ok(f) => f,
        Err(e) => return Err(format!("{remote}: {}", write_error(s, &parent, e).await)),
    };
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = src.read(&mut buf).await.map_err(|e| format!("{}: {e}", local.display()))?;
        if n == 0 {
            break;
        }
        if let Err(e) = dst.write_all(&buf[..n]).await {
            return Err(format!("{remote}: {}", io_error(s, &parent, e).await));
        }
        prog.add(n as u64);
    }
    if let Err(e) = dst.shutdown().await {
        return Err(format!("{remote}: {}", io_error(s, &parent, e).await));
    }
    Ok(())
}

/// Returns what was skipped (links to folders, special files).
async fn download(s: &SftpSession, remote: &str, local: &Path, id: u64, tab: u64, tx: &UnboundedSender<AppEvent>) -> Result<Vec<String>, String> {
    send(tx, tab, SftpEvent::Scanning { id });
    let plan = plan_download(s, remote, local).await?;
    let mut prog = Progress::start(tx, tab, id, plan.total);
    for dir in &plan.dirs {
        tokio::fs::create_dir_all(dir).await.map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for (src, dst, _) in &plan.files {
        download_file(s, src, dst, &mut prog).await?;
    }
    Ok(plan.skipped)
}

/// Where a download is written until it is complete: `<name>.sshm-part` next to it.
fn part_path(local: &Path) -> PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(".sshm-part");
    local.with_file_name(name)
}

async fn download_file(s: &SftpSession, remote: &str, local: &Path, prog: &mut Progress<'_>) -> Result<(), String> {
    let mut src = s.open(remote).await.map_err(|e| format!("{remote}: {}", describe(&e)))?;
    let tmp = part_path(local);
    let mut dst = tokio::fs::File::create(&tmp).await.map_err(|e| format!("{}: {e}", local.display()))?;
    let copied = async {
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = src.read(&mut buf).await.map_err(|e| format!("{remote}: {}", describe_io(&e)))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(|e| format!("{}: {e}", local.display()))?;
            prog.add(n as u64);
        }
        dst.flush().await.map_err(|e| format!("{}: {e}", local.display()))
    }
    .await;
    drop(dst);
    if let Err(e) = copied {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    tokio::fs::rename(&tmp, local).await.map_err(|e| format!("{}: {e}", local.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        assert_eq!(join_remote("/home/u", "x"), "/home/u/x");
        assert_eq!(join_remote("/", "etc"), "/etc");
        assert_eq!(join_remote("/home/u", ".."), "/home");
        assert_eq!(parent_remote("/home"), "/");
        assert_eq!(parent_remote("/"), "/");
    }

    fn status(status_code: StatusCode, msg: &str) -> SftpError {
        SftpError::Status(Status { id: 1, status_code, error_message: msg.into(), language_tag: String::new() })
    }

    #[test]
    fn readable_errors() {
        assert_eq!(describe(&status(StatusCode::Failure, "Failure")), "the server refused without giving a reason");
        assert_eq!(describe(&status(StatusCode::PermissionDenied, "Permission denied")), "permission denied");
        assert_eq!(describe(&status(StatusCode::NoSuchFile, "")), "no such file or directory");
        assert_eq!(describe(&status(StatusCode::Eof, "End of file")), "unexpected end of file");
        assert_eq!(describe(&status(StatusCode::Failure, "Quota exceeded")), "Quota exceeded");

        let wrapped = std::io::Error::from(status(StatusCode::PermissionDenied, "Permission denied"));
        assert_eq!(describe_io(&wrapped), "permission denied");
        assert_eq!(describe_io(&std::io::Error::other("Failure")), "the server refused without giving a reason");
        assert_eq!(describe_io(&std::io::Error::other("file size unknown")), "file size unknown");
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("sshm-sftp-{}", new_id()));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// root/ a.txt (3) · link.txt → a.txt · broken → nowhere · sock · sub/ b.txt (5) · sub/loop → ..
    fn sample_tree() -> (TempDir, PathBuf) {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new();
        let root = tmp.0.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "abc").unwrap();
        std::fs::write(root.join("sub/b.txt"), "hello").unwrap();
        symlink("a.txt", root.join("link.txt")).unwrap();
        symlink("nowhere", root.join("broken")).unwrap();
        symlink("..", root.join("sub/loop")).unwrap();
        std::os::unix::net::UnixListener::bind(root.join("sock")).unwrap();
        (tmp, root)
    }

    #[test]
    fn upload_plan_skips_links_to_folders() {
        let (_tmp, root) = sample_tree();
        let plan = plan_upload(&root, "/r").unwrap();
        assert_eq!(plan.dirs, ["/r", "/r/sub"]);
        let files: Vec<(&str, u64)> = plan.files.iter().map(|(_, d, n)| (d.as_str(), *n)).collect();
        assert_eq!(files, [("/r/a.txt", 3), ("/r/link.txt", 3), ("/r/sub/b.txt", 5)]);
        assert_eq!(plan.total, 11);
        assert_eq!(plan.skipped, ["root/broken (broken link)", "root/sock (not a regular file)", "root/sub/loop/ (link to a folder)"]);
    }

    #[test]
    fn a_selected_link_is_followed_once() {
        let (_tmp, root) = sample_tree();
        let plan = plan_upload(&root.join("sub/loop"), "/x").unwrap();
        assert_eq!(plan.dirs, ["/x", "/x/sub"]);
        assert_eq!(plan.total, 11);
        assert!(plan.skipped.contains(&"loop/sub/loop/ (link to a folder)".to_string()));

        let single = plan_upload(&root.join("link.txt"), "/y.txt").unwrap();
        assert_eq!((single.dirs.len(), single.total), (0, 3));
        assert!(plan_upload(&root.join("sock"), "/z").is_err());
    }

    #[test]
    fn remote_kinds() {
        let attrs = |mode: Option<u32>, size: u64| FileAttributes { permissions: mode, size: Some(size), ..FileAttributes::default() };
        let (file, dir, link, sock) = (attrs(Some(0o100644), 7), attrs(Some(0o040755), 0), attrs(Some(0o120777), 3), attrs(Some(0o140755), 0));
        assert_eq!(remote_kind(&file, None), Kind::File(7));
        assert_eq!(remote_kind(&dir, None), Kind::Dir);
        assert_eq!(remote_kind(&sock, None), Kind::Other);
        assert_eq!(remote_kind(&link, Some(&file)), Kind::LinkToFile(7));
        assert_eq!(remote_kind(&link, Some(&dir)), Kind::LinkToDir);
        assert_eq!(remote_kind(&link, None), Kind::BrokenLink);
        assert_eq!(remote_kind(&attrs(None, 9), None), Kind::File(9));
    }

    #[test]
    fn part_file_keeps_the_whole_name() {
        assert_eq!(part_path(Path::new("/d/a.tar.gz")), Path::new("/d/a.tar.gz.sshm-part"));
        assert_eq!(part_path(Path::new("/d/.bashrc")), Path::new("/d/.bashrc.sshm-part"));
    }

    #[test]
    fn listing_another_folder_drops_the_marks() {
        let entry = |name: &str| Entry { name: name.into(), is_dir: false, is_link: false, size: 0, mtime: None };
        let mut p = Panel { cwd: "/a".into(), ..Panel::default() };
        p.set_entries("/a".into(), vec![entry("x"), entry("y")], false, None);
        p.marked.insert("x".into());
        p.set_entries("/a".into(), vec![entry("x"), entry("y")], false, Some("y"));
        assert!(p.marked.contains("x"));
        assert_eq!(p.current().map(|e| e.name.as_str()), Some("y"));
        p.set_entries("/b".into(), vec![entry("x")], false, None);
        assert!(p.marked.is_empty());
        assert_eq!((p.cwd.as_str(), p.selected), ("/b", 0));
    }
}
