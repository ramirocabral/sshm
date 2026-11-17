use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use russh_sftp::client::SftpSession;
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
}

impl Panel {
    pub fn current(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    fn set_entries(&mut self, mut entries: Vec<Entry>, show_hidden: bool, keep: Option<&str>) {
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
    pub started: Instant,
}

pub enum Job {
    Upload { id: u64, local: PathBuf, remote: String },
    Download { id: u64, remote: String, local: PathBuf },
}

pub enum SftpEvent {
    Connected { session: Arc<SftpSession>, home: String },
    Failed(String),
    Listing { path: String, result: Result<Vec<Entry>, String>, keep: Option<String> },
    Progress { id: u64, done: u64, total: u64 },
    TransferDone { id: u64, error: Option<String> },
    OpDone { result: Result<String, String> },
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
}

impl SftpView {
    pub fn panel(&self, remote: bool) -> &Panel {
        if remote { &self.remote } else { &self.local }
    }

    pub fn panel_mut(&mut self, remote: bool) -> &mut Panel {
        if remote { &mut self.remote } else { &mut self.local }
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

async fn list_remote(s: &SftpSession, dir: &str) -> Result<Vec<Entry>, String> {
    let rd = s.read_dir(dir).await.map_err(|e| format!("{dir}: {e}"))?;
    let mut out = Vec::new();
    for e in rd {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let meta = e.metadata();
        let is_link = meta.is_symlink();
        let mut is_dir = meta.is_dir();
        if is_link
            && let Ok(m) = s.metadata(join_remote(dir, &name)).await {
                is_dir = m.is_dir();
            }
        out.push(Entry { name, is_dir, is_link, size: meta.size.unwrap_or(0), mtime: meta.mtime.map(u64::from) });
    }
    Ok(out)
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
            local: Panel { cwd: home.clone(), ..Panel::default() },
            remote: Panel { cwd: String::new(), loading: true, ..Panel::default() },
            remote_active: true,
            session: None,
            status: format!("Connecting to {}…", host.display_target()),
            jobs: None,
            transfers: Vec::new(),
            show_hidden: false,
            child: None,
        };
        match list_local(&home) {
            Ok(e) => view.local.set_entries(e, false, None),
            Err(e) => view.local.error = Some(e),
        }

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
                view.remote.cwd = home.clone();
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
                        send(&worker_tx, tab, SftpEvent::TransferDone { id, error: result.err() });
                    }
                });
                self.sftp_refresh(tab, true, None);
            }
            SftpEvent::Failed(msg) => {
                view.status = format!("Error: {msg}");
                view.remote.loading = false;
                view.remote.error = Some(msg);
            }
            SftpEvent::Listing { path, result, keep } => {
                let show_hidden = view.show_hidden;
                match result {
                    Ok(entries) => {
                        view.remote.cwd = path;
                        view.remote.set_entries(entries, show_hidden, keep.as_deref());
                    }
                    Err(e) => {
                        view.remote.loading = false;
                        view.status = format!("Error: {e}");
                    }
                }
            }
            SftpEvent::Progress { id, done, total } => {
                if let Some(t) = view.transfers.iter_mut().find(|t| t.id == id) {
                    t.done = done;
                    t.total = total;
                    t.state = TransferState::Running;
                }
            }
            SftpEvent::TransferDone { id, error } => {
                let mut upload = false;
                if let Some(t) = view.transfers.iter_mut().find(|t| t.id == id) {
                    upload = t.upload;
                    t.state = match &error {
                        Some(e) => TransferState::Failed(e.clone()),
                        None => {
                            t.done = t.total;
                            TransferState::Done
                        }
                    };
                }
                let pending = view.transfers.iter().any(|t| matches!(t.state, TransferState::Queued | TransferState::Running));
                if let Some(e) = error {
                    view.status = format!("Transfer failed: {e}");
                } else if !pending {
                    view.status = "Transfers finished".into();
                }
                if upload {
                    self.sftp_refresh(tab, true, None);
                } else {
                    self.sftp_refresh(tab, false, None);
                }
            }
            SftpEvent::OpDone { result } => {
                match result {
                    Ok(msg) => view.status = msg,
                    Err(e) => view.status = format!("Error: {e}"),
                }
                self.sftp_refresh(tab, true, None);
                self.sftp_refresh(tab, false, None);
            }
        }
    }

    pub fn sftp_refresh(&mut self, tab: u64, remote: bool, keep: Option<String>) {
        let tx = self.tx.clone();
        let Some(view) = self.sftp_view(tab) else { return };
        if remote {
            let Some(session) = view.session.clone() else { return };
            let path = view.remote.cwd.clone();
            let keep = keep.or_else(|| view.remote.current().map(|e| e.name.clone()));
            view.remote.loading = true;
            tokio::spawn(async move {
                let result = list_remote(&session, &path).await;
                send(&tx, tab, SftpEvent::Listing { path, result, keep });
            });
        } else {
            let keep = keep.or_else(|| view.local.current().map(|e| e.name.clone()));
            let show_hidden = view.show_hidden;
            match list_local(&view.local.cwd) {
                Ok(e) => view.local.set_entries(e, show_hidden, keep.as_deref()),
                Err(e) => view.status = format!("Error: {e}"),
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
        let panel = view.panel_mut(remote);
        panel.cwd = new_cwd;
        panel.marked.clear();
        panel.selected = 0;
        self.sftp_refresh(tab, remote, came_from);
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
        let Some(view) = self.sftp_view(tab) else { return };
        let path = if remote { path } else { crate::paths::expand_tilde(&path).to_string_lossy().into_owned() };
        view.panel_mut(remote).cwd = path;
        self.sftp_refresh(tab, remote, None);
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
            KeyCode::Char('x') => view.transfers.retain(|t| matches!(t.state, TransferState::Queued | TransferState::Running)),
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
                started: Instant::now(),
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
            let path = join_remote(&view.remote.cwd, &name);
            tokio::spawn(async move {
                let result = s.create_dir(path.clone()).await.map(|_| format!("Folder created: {path}")).map_err(|e| e.to_string());
                send(&tx, tab, SftpEvent::OpDone { result });
            });
        } else {
            let path = Path::new(&view.local.cwd).join(&name);
            let result = std::fs::create_dir_all(&path).map(|_| format!("Folder created: {}", path.display())).map_err(|e| e.to_string());
            send(&tx, tab, SftpEvent::OpDone { result });
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
            let a = join_remote(&view.remote.cwd, &from);
            let b = if to.starts_with('/') { to.clone() } else { join_remote(&view.remote.cwd, &to) };
            tokio::spawn(async move {
                let result = s.rename(a, b.clone()).await.map(|_| format!("Renamed to {b}")).map_err(|e| e.to_string());
                send(&tx, tab, SftpEvent::OpDone { result });
            });
        } else {
            let a = Path::new(&view.local.cwd).join(&from);
            let b = Path::new(&view.local.cwd).join(&to);
            let result = std::fs::rename(&a, &b).map(|_| format!("Renamed to {}", b.display())).map_err(|e| e.to_string());
            send(&tx, tab, SftpEvent::OpDone { result });
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
                send(&tx, tab, SftpEvent::OpDone { result });
            });
        } else {
            let mut errors = Vec::new();
            for n in &names {
                let p = Path::new(&view.local.cwd).join(n);
                let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                if let Err(e) = r {
                    errors.push(format!("{n}: {e}"));
                }
            }
            let result = if errors.is_empty() { Ok(format!("Deleted: {}", names.len())) } else { Err(errors.join("; ")) };
            send(&tx, tab, SftpEvent::OpDone { result });
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
        let meta = s.symlink_metadata(path).await.map_err(|e| e.to_string())?;
        if meta.is_dir() {
            let entries = s.read_dir(path).await.map_err(|e| e.to_string())?;
            for e in entries {
                let name = e.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                remove_remote(s, &join_remote(path, &name)).await?;
            }
            s.remove_dir(path).await.map_err(|e| e.to_string())
        } else {
            s.remove_file(path).await.map_err(|e| e.to_string())
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
    fn add(&mut self, n: u64) {
        self.done += n;
        if self.last.elapsed() > Duration::from_millis(150) {
            self.last = Instant::now();
            send(self.tx, self.tab, SftpEvent::Progress { id: self.id, done: self.done, total: self.total });
        }
    }
}

fn local_size(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| local_size(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

async fn upload(s: &SftpSession, local: &Path, remote: &str, id: u64, tab: u64, tx: &UnboundedSender<AppEvent>) -> Result<(), String> {
    let total = local_size(local);
    let mut prog = Progress { tx, tab, id, done: 0, total, last: Instant::now() };
    send(tx, tab, SftpEvent::Progress { id, done: 0, total });
    upload_rec(s, local, remote, &mut prog).await
}

fn upload_rec<'a>(
    s: &'a SftpSession,
    local: &'a Path,
    remote: &'a str,
    prog: &'a mut Progress<'_>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let meta = std::fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
        if meta.is_dir() {
            if !s.try_exists(remote).await.unwrap_or(false) {
                s.create_dir(remote).await.map_err(|e| format!("{remote}: {e}"))?;
            }
            let entries: Vec<_> = std::fs::read_dir(local).map_err(|e| e.to_string())?.flatten().collect();
            for e in entries {
                let name = e.file_name().to_string_lossy().into_owned();
                upload_rec(s, &e.path(), &join_remote(remote, &name), prog).await?;
            }
            return Ok(());
        }
        let mut src = tokio::fs::File::open(local).await.map_err(|e| format!("{}: {e}", local.display()))?;
        let mut dst = s.create(remote).await.map_err(|e| format!("{remote}: {e}"))?;
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = src.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(|e| format!("{remote}: {e}"))?;
            prog.add(n as u64);
        }
        dst.shutdown().await.map_err(|e| format!("{remote}: {e}"))?;
        Ok(())
    })
}

fn remote_size<'a>(s: &'a SftpSession, path: &'a str) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + 'a>> {
    Box::pin(async move {
        let Ok(meta) = s.metadata(path).await else { return 0 };
        if !meta.is_dir() {
            return meta.size.unwrap_or(0);
        }
        let Ok(rd) = s.read_dir(path).await else { return 0 };
        let mut total = 0;
        for e in rd {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            if e.metadata().is_dir() {
                total += remote_size(s, &join_remote(path, &name)).await;
            } else {
                total += e.metadata().size.unwrap_or(0);
            }
        }
        total
    })
}

async fn download(s: &SftpSession, remote: &str, local: &Path, id: u64, tab: u64, tx: &UnboundedSender<AppEvent>) -> Result<(), String> {
    let total = remote_size(s, remote).await;
    let mut prog = Progress { tx, tab, id, done: 0, total, last: Instant::now() };
    send(tx, tab, SftpEvent::Progress { id, done: 0, total });
    download_rec(s, remote, local, &mut prog).await
}

fn download_rec<'a>(
    s: &'a SftpSession,
    remote: &'a str,
    local: &'a Path,
    prog: &'a mut Progress<'_>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let meta = s.metadata(remote).await.map_err(|e| format!("{remote}: {e}"))?;
        if meta.is_dir() {
            std::fs::create_dir_all(local).map_err(|e| format!("{}: {e}", local.display()))?;
            let rd = s.read_dir(remote).await.map_err(|e| format!("{remote}: {e}"))?;
            let names: Vec<String> = rd.map(|e| e.file_name()).filter(|n| n != "." && n != "..").collect();
            for name in names {
                download_rec(s, &join_remote(remote, &name), &local.join(&name), prog).await?;
            }
            return Ok(());
        }
        let mut src = s.open(remote).await.map_err(|e| format!("{remote}: {e}"))?;
        let tmp = local.with_extension("sshm-part");
        let mut dst = tokio::fs::File::create(&tmp).await.map_err(|e| format!("{}: {e}", tmp.display()))?;
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = src.read(&mut buf).await.map_err(|e| format!("{remote}: {e}"))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(|e| e.to_string())?;
            prog.add(n as u64);
        }
        dst.flush().await.map_err(|e| e.to_string())?;
        drop(dst);
        tokio::fs::rename(&tmp, local).await.map_err(|e| format!("{}: {e}", local.display()))?;
        Ok(())
    })
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
}
