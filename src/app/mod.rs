mod actions;
pub mod dialogs;
pub mod form;
mod hosts;
pub mod input;
pub mod keymap;
pub mod sftp;
pub mod sidebar;
pub mod widgets;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event as CEvent, EventStream};
use futures::StreamExt;
use portable_pty::CommandBuilder;
use ratatui::layout::Rect;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::secrets::{self, SecretKind, Vault};
use crate::ssh::askpass::{self, AskPassRequest};
use crate::ssh::command::{self, AskPassEnv, PromptKind, Purpose, SessionPrompt};
use crate::store::config::{Config, KeySpec};
use crate::store::hosts::{HostStore, SecretSource, new_id};
use crate::store::state::UiState;
use crate::term::pane::{GridSize, PaneId, PaneNotify, TermPane};
use dialogs::*;
use sidebar::Sidebar;

pub enum AppEvent {
    Input(CEvent),
    Pane(PaneId, PaneNotify),
    AskPass(AskPassRequest),
    /// Ok(None): the source has nothing stored for the host.
    SecretFetched { req: u64, result: Result<Option<String>, String> },
    /// Outcome of a background job worth a toast.
    Notice(Result<String, String>),
    Sftp(u64, sftp::SftpEvent),
    Tick,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Sidebar,
    Main,
}

pub struct Pane {
    pub term: TermPane,
    pub host_id: Option<String>,
    pub name: String,
    pub title: String,
    pub token: Option<String>,
    pub exited: Option<Option<u32>>,
    pub excluded: bool,
    pub prompt: Option<(SessionPrompt, i32)>,
    pub last_autofill: Option<Instant>,
    pub on_connect_pending: bool,
    pub copy_mode: bool,
    pub bell: bool,
}

pub struct TermTab {
    pub panes: Vec<PaneId>,
    pub focused: usize,
    pub broadcast: bool,
    pub zoomed: bool,
}

pub enum TabKind {
    Terms(TermTab),
    Sftp(Box<sftp::SftpView>),
}

pub struct Tab {
    pub id: u64,
    pub title: Option<String>,
    pub kind: TabKind,
}

pub struct Owner {
    pub host_id: String,
    pub label: String,
    pub password_attempts: u32,
    pub passphrase_attempts: u32,
}

#[derive(Default)]
pub struct LayoutCache {
    pub sidebar: Rect,
    pub sidebar_list: Rect,
    pub tabbar: Rect,
    pub body: Rect,
    pub tabs: Vec<(usize, Rect)>,
    pub panes: Vec<(PaneId, Rect)>,
    pub sftp_panels: Vec<(bool, Rect)>,
    pub form_rows: Vec<(usize, Rect)>,
    pub popup_rows: Vec<(usize, Rect)>,
    pub help_max_scroll: u16,
}

pub enum ToastKind {
    Info,
    Error,
}

pub struct App {
    pub cfg: Config,
    pub leader: KeySpec,
    pub store: HostStore,
    pub vault: Vault,
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub panes: HashMap<PaneId, Pane>,
    pub focus: Focus,
    pub sidebar: Sidebar,
    pub sidebar_visible: bool,
    pub dialogs: VecDeque<Dialog>,
    pub leader_pending: bool,
    pub toast: Option<(String, Instant, ToastKind)>,
    pub tx: UnboundedSender<AppEvent>,
    pub askpass_sock: PathBuf,
    pub runtime_dir: PathBuf,
    pub owners: HashMap<String, Owner>,
    pub pending_secrets: HashMap<u64, SecretPurpose>,
    pub layout: LayoutCache,
    pub truecolor: bool,
    pub quit: bool,
    pub dirty: bool,
    pub last_vault_use: Instant,
    pub mouse: input::MouseState,
    /// Expanded folders as last written to the state file.
    saved_expanded: HashSet<String>,
    state_path: PathBuf,
    next_id: u64,
}

/// What an App is built from; `App::new` reads it from the XDG files and the terminal.
pub struct AppParts {
    pub cfg: Config,
    pub store: HostStore,
    pub vault: Vault,
    pub state: UiState,
    pub state_path: PathBuf,
    pub askpass_sock: PathBuf,
    pub runtime_dir: PathBuf,
    pub truecolor: bool,
}

impl App {
    pub fn new(cfg: Config, tx: UnboundedSender<AppEvent>, askpass_sock: PathBuf, runtime_dir: PathBuf) -> Result<Self> {
        let truecolor = crate::term::render::truecolor_supported();
        if let Some(theme) = crate::ui::theme::find(&cfg.theme) {
            crate::ui::theme::apply(theme, truecolor);
        }
        let parts = AppParts {
            cfg,
            store: HostStore::load_default()?,
            vault: Vault::new(Vault::default_path()),
            state: UiState::load(),
            state_path: UiState::path(),
            askpass_sock,
            runtime_dir,
            truecolor,
        };
        Ok(App::from_parts(parts, tx))
    }

    pub fn from_parts(parts: AppParts, tx: UnboundedSender<AppEvent>) -> Self {
        let AppParts { cfg, store, vault, state, state_path, askpass_sock, runtime_dir, truecolor } = parts;
        let leader = cfg.leader_key();
        let mut sidebar = Sidebar::default();
        let mut saved_expanded = HashSet::new();
        if let Some(expanded) = state.expanded {
            saved_expanded = expanded.iter().cloned().collect();
            sidebar.restore(expanded);
        }
        App {
            cfg,
            leader,
            store,
            vault,
            tabs: Vec::new(),
            active: 0,
            panes: HashMap::new(),
            focus: Focus::Sidebar,
            sidebar,
            sidebar_visible: true,
            dialogs: VecDeque::new(),
            leader_pending: false,
            toast: None,
            tx,
            askpass_sock,
            runtime_dir,
            owners: HashMap::new(),
            pending_secrets: HashMap::new(),
            layout: LayoutCache::default(),
            truecolor,
            quit: false,
            dirty: true,
            last_vault_use: Instant::now(),
            mouse: input::MouseState::default(),
            saved_expanded,
            state_path,
            next_id: 1,
        }
    }

    /// Writes the UI state when the open folders changed since the last write.
    pub fn persist_ui_state(&mut self) {
        if self.sidebar.expanded == self.saved_expanded {
            return;
        }
        self.saved_expanded = self.sidebar.expanded.clone();
        let folders: HashSet<String> = self.store.folders().into_iter().collect();
        let mut expanded: Vec<String> = self.saved_expanded.iter().filter(|f| folders.contains(*f)).cloned().collect();
        expanded.sort();
        if let Err(e) = (UiState { expanded: Some(expanded) }).save_to(&self.state_path) {
            tracing::warn!("could not save the UI state: {e:#}");
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now(), ToastKind::Info));
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        tracing::warn!("{msg}");
        self.toast = Some((msg, Instant::now(), ToastKind::Error));
    }

    pub fn push_dialog(&mut self, d: Dialog) {
        self.dialogs.push_back(d);
    }

    pub fn handle(&mut self, ev: AppEvent) {
        self.dirty |= !matches!(ev, AppEvent::Tick);
        match ev {
            AppEvent::Input(ev) => self.handle_input(ev),
            AppEvent::Pane(id, n) => self.handle_pane_event(id, n),
            AppEvent::AskPass(req) => self.handle_askpass(req),
            AppEvent::SecretFetched { req, result } => {
                if let Some(purpose) = self.pending_secrets.remove(&req) {
                    match result {
                        Ok(v) => self.fulfill(purpose, v),
                        Err(e) => {
                            self.error(e);
                            self.fulfill(purpose, None);
                        }
                    }
                }
            }
            AppEvent::Notice(Ok(msg)) => self.info(msg),
            AppEvent::Notice(Err(e)) => self.error(e),
            AppEvent::Sftp(tab, ev) => self.handle_sftp_event(tab, ev),
            AppEvent::Tick => {
                if self.toast.as_ref().is_some_and(|t| t.1.elapsed() > Duration::from_secs(4)) {
                    self.toast = None;
                    self.dirty = true;
                }
                if self.cfg.vault_timeout > 0
                    && self.vault.is_unlocked()
                    && self.last_vault_use.elapsed() > Duration::from_secs(self.cfg.vault_timeout)
                {
                    self.vault.lock();
                    self.info("Vault locked after inactivity");
                    self.dirty = true;
                }
            }
        }
    }

    // ---------- tabs & panes ----------

    pub fn active_term_tab(&self) -> Option<&TermTab> {
        match &self.tabs.get(self.active)?.kind {
            TabKind::Terms(t) => Some(t),
            _ => None,
        }
    }

    pub fn active_term_tab_mut(&mut self) -> Option<&mut TermTab> {
        match &mut self.tabs.get_mut(self.active)?.kind {
            TabKind::Terms(t) => Some(t),
            _ => None,
        }
    }

    pub fn focused_pane_id(&self) -> Option<PaneId> {
        let t = self.active_term_tab()?;
        t.panes.get(t.focused).copied()
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused_pane_id()?)
    }

    pub fn tab_of_pane(&self, id: PaneId) -> Option<usize> {
        self.tabs.iter().position(|t| matches!(&t.kind, TabKind::Terms(tt) if tt.panes.contains(&id)))
    }

    fn initial_size(&self) -> GridSize {
        let b = self.layout.body;
        if b.width > 2 && b.height > 2 {
            GridSize { cols: b.width, rows: b.height }
        } else {
            let (c, r) = crossterm::terminal::size().unwrap_or((80, 24));
            GridSize { cols: c.saturating_sub(self.cfg.sidebar_width).max(20), rows: r.saturating_sub(2).max(5) }
        }
    }

    fn notifier(&self) -> crate::term::pane::Notifier {
        let tx = self.tx.clone();
        std::sync::Arc::new(move |id, n| {
            let _ = tx.send(AppEvent::Pane(id, n));
        })
    }

    fn spawn_pane(&mut self, host_id: Option<&str>) -> Result<PaneId> {
        let id = self.next_id();
        let size = self.initial_size();
        let (cmd, name, token, on_connect) = match host_id {
            Some(hid) => {
                let host = self.store.get(hid).ok_or_else(|| anyhow::anyhow!("unknown host"))?.clone();
                let token = new_id();
                let ask = AskPassEnv { socket: self.askpass_sock.clone(), token: token.clone() };
                let spec = command::build(&host, &self.cfg, Some(&ask), Some(&self.runtime_dir), Purpose::Shell);
                let mut cmd = CommandBuilder::new(&spec.program);
                cmd.args(&spec.args);
                for (k, v) in &spec.env {
                    cmd.env(k, v);
                }
                cmd.env("TERM", &self.cfg.term);
                cmd.cwd(crate::paths::home());
                self.owners.insert(
                    token.clone(),
                    Owner { host_id: hid.to_string(), label: host.name.clone(), password_attempts: 0, passphrase_attempts: 0 },
                );
                (cmd, host.name.clone(), Some(token), host.on_connect.is_some())
            }
            None => {
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let mut cmd = CommandBuilder::new(&shell);
                cmd.env("TERM", &self.cfg.term);
                cmd.cwd(crate::paths::home());
                (cmd, "local".to_string(), None, false)
            }
        };
        tracing::info!(pane = id, host = %name, "opening session");
        let term = TermPane::spawn(id, cmd, size, self.cfg.scrollback, self.notifier())?;
        self.panes.insert(
            id,
            Pane {
                term,
                host_id: host_id.map(str::to_string),
                name,
                title: String::new(),
                token,
                exited: None,
                excluded: false,
                prompt: None,
                last_autofill: None,
                on_connect_pending: on_connect,
                copy_mode: false,
                bell: false,
            },
        );
        Ok(id)
    }

    /// Opens sessions to the given hosts in a new tab (a grid when there are several).
    pub fn open_tab(&mut self, hosts: &[Option<String>]) {
        let mut panes = Vec::new();
        for h in hosts {
            match self.spawn_pane(h.as_deref()) {
                Ok(id) => panes.push(id),
                Err(e) => self.error(format!("Could not open the session: {e:#}")),
            }
        }
        if panes.is_empty() {
            return;
        }
        let id = self.next_id();
        self.tabs.push(Tab {
            id,
            title: None,
            kind: TabKind::Terms(TermTab { panes, focused: 0, broadcast: false, zoomed: false }),
        });
        self.active = self.tabs.len() - 1;
        self.focus = Focus::Main;
    }

    /// Adds a session to the current terminal tab (or a new tab when the active one is not a terminal tab).
    pub fn open_in_current(&mut self, host: Option<String>) {
        if self.active_term_tab().is_none() {
            self.open_tab(&[host]);
            return;
        }
        match self.spawn_pane(host.as_deref()) {
            Ok(id) => {
                if let Some(t) = self.active_term_tab_mut() {
                    t.panes.push(id);
                    t.focused = t.panes.len() - 1;
                    t.zoomed = false;
                }
                self.focus = Focus::Main;
            }
            Err(e) => self.error(format!("Could not open the session: {e:#}")),
        }
    }

    pub fn reconnect(&mut self, pane_id: PaneId) {
        let Some(old) = self.panes.get(&pane_id) else { return };
        let host = old.host_id.clone();
        let excluded = old.excluded;
        match self.spawn_pane(host.as_deref()) {
            Ok(new_id) => {
                if let Some(mut p) = self.panes.remove(&pane_id)
                    && let Some(t) = p.token.take() {
                        self.owners.remove(&t);
                    }
                if let Some(p) = self.panes.get_mut(&new_id) {
                    p.excluded = excluded;
                }
                for tab in &mut self.tabs {
                    if let TabKind::Terms(t) = &mut tab.kind {
                        for p in &mut t.panes {
                            if *p == pane_id {
                                *p = new_id;
                            }
                        }
                    }
                }
            }
            Err(e) => self.error(format!("Could not reconnect: {e:#}")),
        }
    }

    pub fn close_pane(&mut self, pane_id: PaneId) {
        if let Some(mut p) = self.panes.remove(&pane_id)
            && let Some(t) = p.token.take() {
                self.owners.remove(&t);
            }
        let mut empty_tab = None;
        for (i, tab) in self.tabs.iter_mut().enumerate() {
            if let TabKind::Terms(t) = &mut tab.kind
                && let Some(pos) = t.panes.iter().position(|p| *p == pane_id) {
                    t.panes.remove(pos);
                    if t.focused >= t.panes.len() {
                        t.focused = t.panes.len().saturating_sub(1);
                    }
                    if t.panes.is_empty() {
                        empty_tab = Some(i);
                    }
                }
        }
        if let Some(i) = empty_tab {
            self.remove_tab(i);
        }
    }

    pub fn remove_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(idx);
        match tab.kind {
            TabKind::Terms(t) => {
                for p in t.panes {
                    if let Some(mut pane) = self.panes.remove(&p)
                        && let Some(tok) = pane.token.take() {
                            self.owners.remove(&tok);
                        }
                }
            }
            TabKind::Sftp(v) => {
                self.owners.remove(&v.token);
            }
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if self.tabs.is_empty() {
            self.focus = Focus::Sidebar;
        }
    }

    pub fn running_sessions(&self) -> usize {
        self.panes.values().filter(|p| p.exited.is_none() && p.host_id.is_some()).count()
    }

    fn handle_pane_event(&mut self, id: PaneId, n: PaneNotify) {
        match n {
            PaneNotify::Output => self.after_output(id),
            PaneNotify::Exited(code) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.exited = Some(code);
                    p.prompt = None;
                }
            }
            PaneNotify::Title(t) => {
                if let Some(p) = self.panes.get_mut(&id) {
                    p.title = t;
                }
            }
            PaneNotify::Clipboard(text) => {
                crate::clipboard::copy(&text, self.cfg.clipboard_command.as_deref());
                self.info("Copied to clipboard (requested by the session)");
            }
            PaneNotify::Bell => {
                if self.focused_pane_id() != Some(id)
                    && let Some(p) = self.panes.get_mut(&id) {
                        p.bell = true;
                    }
            }
        }
    }

    fn after_output(&mut self, id: PaneId) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        p.term.take_dirty();
        let (line_text, line) = {
            let emu = p.term.lock();
            (emu.cursor_line_text(), emu.cursor_line())
        };
        if p.on_connect_pending && command::looks_like_shell_prompt(&line_text) {
            p.on_connect_pending = false;
            let host = p.host_id.as_ref().and_then(|h| self.store.get(h));
            if let Some(text) = host.and_then(|h| h.on_connect.clone()) {
                p.term.write(format!("{text}\r").as_bytes());
            }
            return;
        }
        let detected = command::detect_session_prompt(&line_text);
        let Some(kind) = detected else {
            p.prompt = None;
            return;
        };
        if p.prompt.is_some_and(|(_, l)| l == line) {
            return;
        }
        p.prompt = Some((kind, line));
        let Some(host_id) = p.host_id.clone() else { return };
        let auto = self.store.get(&host_id).is_some_and(|h| h.auto_sudo);
        let recent = p.last_autofill.is_some_and(|t| t.elapsed() < Duration::from_secs(15));
        if auto && kind == SessionPrompt::Sudo && !recent {
            p.last_autofill = Some(Instant::now());
            self.with_secret(&host_id, SecretKind::Password, SecretPurpose::Type { pane: id });
        } else if recent && kind == SessionPrompt::Sudo {
            self.error("sudo asked again: the stored password looks wrong (not resending it)");
        }
    }

    pub fn type_secret(&mut self, kind: SecretKind) {
        let Some(id) = self.focused_pane_id() else { return };
        let Some(host_id) = self.panes.get(&id).and_then(|p| p.host_id.clone()) else {
            self.error("This session has no host attached");
            return;
        };
        if let Some(p) = self.panes.get_mut(&id) {
            p.last_autofill = Some(Instant::now());
        }
        self.with_secret(&host_id, kind, SecretPurpose::Type { pane: id });
    }

    // ---------- secrets ----------

    pub fn with_secret(&mut self, host_id: &str, kind: SecretKind, purpose: SecretPurpose) {
        let Some(host) = self.store.get(host_id) else {
            self.fulfill(purpose, None);
            return;
        };
        let source = match kind {
            SecretKind::Password => host.password.clone(),
            SecretKind::RootPassword => host.root_password.clone(),
            SecretKind::Passphrase => SecretSource::Vault,
        };
        match source {
            SecretSource::None => self.fulfill(purpose, None),
            SecretSource::Vault => {
                if self.vault.is_unlocked() {
                    self.last_vault_use = Instant::now();
                    let v = self.vault.get(host_id, kind);
                    self.fulfill(purpose, v);
                } else if !self.vault.exists() {
                    self.fulfill(purpose, None);
                } else {
                    let then = AfterUnlock::Secret { host_id: host_id.to_string(), kind, purpose };
                    self.push_dialog(Dialog::Unlock(UnlockDialog::new(false, then)));
                }
            }
            SecretSource::Keyring => {
                let req = self.next_id();
                self.pending_secrets.insert(req, purpose);
                let (tx, host_id) = (self.tx.clone(), host_id.to_string());
                tokio::task::spawn_blocking(move || {
                    let result = secrets::keyring::get(&host_id, kind).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(AppEvent::SecretFetched { req, result });
                });
            }
            external => {
                let req = self.next_id();
                self.pending_secrets.insert(req, purpose);
                self.info(format!("Fetching from {}…", external.label()));
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let result = secrets::fetch_external(&external).map(Some).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(AppEvent::SecretFetched { req, result });
                });
            }
        }
    }

    pub fn fulfill(&mut self, purpose: SecretPurpose, value: Option<String>) {
        match purpose {
            SecretPurpose::Type { pane } => match (value, self.panes.get(&pane)) {
                (Some(v), Some(p)) => {
                    p.term.write(v.as_bytes());
                    p.term.write(b"\r");
                }
                (None, Some(_)) => self.error("No password stored for this host (edit it with e to add one)"),
                _ => {}
            },
            SecretPurpose::AskPass { reply, prompt, host_id, label, kind } => match value {
                Some(v) => {
                    let _ = reply.send(Some(v));
                }
                None => self.push_dialog(Dialog::AskPass(AskPassDialog::new(label, prompt, PromptKind::Password, reply).storable(host_id, kind, true))),
            },
        }
    }

    fn handle_askpass(&mut self, req: AskPassRequest) {
        let Some(owner) = self.owners.get_mut(&req.token) else {
            let _ = req.reply.send(None);
            return;
        };
        let host_id = owner.host_id.clone();
        let label = owner.label.clone();
        let kind = command::classify_prompt(&req.prompt);
        tracing::info!(host = %label, ?kind, "ssh askpass request");
        let prompt = req.prompt.trim().to_string();
        match kind {
            PromptKind::Password | PromptKind::Passphrase => {
                let (attempts, secret_kind) = if kind == PromptKind::Password {
                    owner.password_attempts += 1;
                    (owner.password_attempts, SecretKind::Password)
                } else {
                    owner.passphrase_attempts += 1;
                    (owner.passphrase_attempts, SecretKind::Passphrase)
                };
                if attempts == 1 {
                    let purpose = SecretPurpose::AskPass { reply: req.reply, prompt, host_id: host_id.clone(), label, kind: secret_kind };
                    self.with_secret(&host_id, secret_kind, purpose);
                } else {
                    let d = AskPassDialog::new(label, prompt, kind, req.reply).storable(host_id, secret_kind, true).note("The previous password was rejected");
                    self.push_dialog(Dialog::AskPass(d));
                }
            }
            PromptKind::Confirm => self.push_dialog(Dialog::Confirm(ConfirmDialog {
                title: format!("{label}: host key"),
                body: prompt,
                action: Some(ConfirmAction::HostKey(req.reply)),
            })),
            PromptKind::Other => self.push_dialog(Dialog::AskPass(AskPassDialog::new(label, prompt, kind, req.reply))),
        }
    }

    /// Stores (Some) or deletes (None) secrets where the host keeps them: the system keyring
    /// for keyring sources, the vault for everything else.
    pub fn save_secrets(&mut self, host_id: &str, secrets: Vec<(SecretKind, Option<String>)>) {
        let (keyring, secrets): (Vec<_>, Vec<_>) = secrets.into_iter().partition(|(kind, _)| self.uses_keyring(host_id, *kind));
        if !keyring.is_empty() {
            self.save_to_keyring(host_id, keyring);
        }
        if secrets.is_empty() {
            return;
        }
        if !self.vault.is_unlocked() {
            let creating = !self.vault.exists();
            let then = AfterUnlock::SaveSecrets { host_id: host_id.to_string(), secrets };
            self.push_dialog(Dialog::Unlock(UnlockDialog::new(creating, then)));
            return;
        }
        for (kind, v) in secrets {
            self.vault.set(host_id, kind, v);
        }
        match self.vault.save() {
            Ok(()) => self.info("Password saved to the vault"),
            Err(e) => self.error(format!("Could not save the vault: {e:#}")),
        }
    }

    fn uses_keyring(&self, host_id: &str, kind: SecretKind) -> bool {
        self.store.get(host_id).is_some_and(|h| match kind {
            SecretKind::Password => h.password == SecretSource::Keyring,
            SecretKind::RootPassword => h.root_password == SecretSource::Keyring,
            SecretKind::Passphrase => false,
        })
    }

    fn save_to_keyring(&mut self, host_id: &str, secrets: Vec<(SecretKind, Option<String>)>) {
        let path = self.store.get(host_id).map(|h| h.path()).unwrap_or_default();
        let (tx, host_id) = (self.tx.clone(), host_id.to_string());
        tokio::task::spawn_blocking(move || {
            let stored = secrets.iter().any(|(_, v)| v.is_some());
            let result = secrets
                .into_iter()
                .try_for_each(|(kind, value)| match value {
                    Some(v) => {
                        let what = if kind == SecretKind::RootPassword { "root password" } else { "password" };
                        secrets::keyring::set(&host_id, kind, &format!("sshm: {path} ({what})"), &v)
                    }
                    None => secrets::keyring::clear(&host_id, kind),
                })
                .map(|()| if stored { "Password saved to the keyring" } else { "Password removed from the keyring" }.to_string())
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Notice(result));
        });
    }

    /// Removes what deleted hosts kept in the system keyring.
    pub fn forget_keyring_secrets(&self, hosts: Vec<crate::store::hosts::Host>) {
        let jobs: Vec<(String, SecretKind)> = hosts
            .iter()
            .flat_map(|h| {
                [(&h.password, SecretKind::Password), (&h.root_password, SecretKind::RootPassword)]
                    .into_iter()
                    .filter(|(src, _)| **src == SecretSource::Keyring)
                    .map(|(_, kind)| (h.id.clone(), kind))
            })
            .collect();
        if jobs.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            for (id, kind) in jobs {
                if let Err(e) = secrets::keyring::clear(&id, kind) {
                    let _ = tx.send(AppEvent::Notice(Err(format!("{e:#}"))));
                    return;
                }
            }
        });
    }

    pub fn run_after_unlock(&mut self, then: AfterUnlock) {
        match then {
            AfterUnlock::Nothing => self.info("Vault unlocked"),
            AfterUnlock::Secret { host_id, kind, purpose } => self.with_secret(&host_id, kind, purpose),
            AfterUnlock::SaveSecrets { host_id, secrets } => self.save_secrets(&host_id, secrets),
            AfterUnlock::Import(form) => self.run_import(form),
            AfterUnlock::Export(form) => self.run_export(form),
            AfterUnlock::ChangeMaster => {
                let mut d = UnlockDialog::new(true, AfterUnlock::Nothing);
                d.changing = true;
                self.push_dialog(Dialog::Unlock(d));
            }
        }
    }

    /// Fails pending secret purposes when a dialog that would provide them is cancelled.
    pub fn cancel_after_unlock(&mut self, then: AfterUnlock) {
        if let AfterUnlock::Secret { purpose, .. } = then {
            match purpose {
                SecretPurpose::AskPass { reply, prompt, host_id, label, kind } => {
                    let d = AskPassDialog::new(label, prompt, PromptKind::Password, reply).storable(host_id, kind, false).note("Vault locked: type it manually");
                    self.push_dialog(Dialog::AskPass(d));
                }
                SecretPurpose::Type { .. } => {}
            }
        }
    }

    // ---------- import / export ----------

    pub fn run_import(&mut self, mut dlg: ImportForm) {
        use crate::import::{self, ImportOptions};
        let (source, conflict) = (dlg.source(), dlg.conflict());
        let f = &dlg.form;
        let path = crate::paths::expand_tilde(&f.value("path"));
        let opts = ImportOptions { passphrase: Some(f.raw("passphrase")).filter(|p| !p.is_empty()), ..ImportOptions::default() };
        let report = match import::import(source, &path, &opts) {
            Ok(r) => r,
            Err(e) => {
                dlg.form.error = Some(format!("{e:#}"));
                self.dialogs.push_front(Dialog::Import(dlg));
                return;
            }
        };
        let has_secrets = report.hosts.iter().any(|h| h.has_secrets());
        if has_secrets && !self.vault.is_unlocked() {
            let creating = !self.vault.exists();
            self.push_dialog(Dialog::Unlock(UnlockDialog::new(creating, AfterUnlock::Import(dlg))));
            return;
        }
        let warnings = report.warnings.clone();
        let vault = if self.vault.is_unlocked() { Some(&mut self.vault) } else { None };
        match import::merge(&mut self.store, vault, report, conflict) {
            Ok(sum) => {
                if let Err(e) = self.store.save() {
                    self.error(format!("Could not save hosts.toml: {e:#}"));
                }
                if sum.secrets > 0
                    && let Err(e) = self.vault.save() {
                        self.error(format!("Could not save the vault: {e:#}"));
                    }
                let mut lines = vec![
                    format!("Added: {}", sum.added),
                    format!("Updated: {}", sum.updated),
                    format!("Renamed: {}", sum.renamed),
                    format!("Skipped: {}", sum.skipped),
                    format!("Passwords stored in the vault: {}", sum.secrets),
                ];
                if sum.secrets_dropped > 0 {
                    lines.push(format!("Passwords dropped (vault locked): {}", sum.secrets_dropped));
                }
                if !warnings.is_empty() {
                    lines.push(String::new());
                    lines.push("Warnings:".into());
                    lines.extend(warnings.into_iter().map(|w| format!("  • {w}")));
                }
                self.push_dialog(Dialog::Message { title: "Import finished".into(), lines });
            }
            Err(e) => self.error(format!("Import failed: {e:#}")),
        }
    }

    pub fn run_export(&mut self, dlg: ExportForm) {
        use crate::import::{self, ExportFormat};
        let format = dlg.format();
        let f = &dlg.form;
        let want_secrets = f.toggled("with_secrets");
        let pass = f.raw("passphrase");
        let fail = |app: &mut App, mut dlg: ExportForm, msg: String| {
            dlg.form.error = Some(msg);
            app.dialogs.push_front(Dialog::Export(dlg));
        };
        if format == ExportFormat::Bundle {
            if pass.is_empty() {
                return fail(self, dlg, "the bundle needs a password to encrypt it".into());
            }
            if pass != f.raw("confirm") {
                return fail(self, dlg, "the passwords do not match".into());
            }
        }
        if want_secrets && !self.vault.is_unlocked() && self.vault.exists() {
            self.push_dialog(Dialog::Unlock(UnlockDialog::new(false, AfterUnlock::Export(dlg))));
            return;
        }
        let vault = if self.vault.is_unlocked() { Some(&self.vault) } else { None };
        let with_secrets = want_secrets && vault.is_some();
        let pass = Some(pass).filter(|p| !p.is_empty());
        let data = match import::export(&self.store, vault, format, with_secrets, pass.as_deref()) {
            Ok(d) => d,
            Err(e) => return fail(self, dlg, format!("{e:#}")),
        };
        let path = crate::paths::expand_tilde(&dlg.form.value("path"));
        match crate::paths::write_private(&path, &data) {
            Ok(()) => {
                let mut lines = vec![format!("{} hosts exported to {}", self.store.hosts.len(), path.display())];
                if with_secrets && format != ExportFormat::Bundle {
                    lines.push("Warning: the file contains passwords in plain text.".into());
                }
                self.push_dialog(Dialog::Message { title: "Export finished".into(), lines });
            }
            Err(e) => fail(self, dlg, format!("Could not write {}: {e:#}", path.display())),
        }
    }
}

/// Shortest time between two redraws (~60 fps): bursts of output are coalesced into one frame.
const FRAME: Duration = Duration::from_millis(16);

pub async fn run(cfg: Config) -> Result<()> {
    let runtime_dir = crate::paths::runtime_dir()?;
    let (tx, mut rx) = unbounded_channel::<AppEvent>();
    // Must run before the input reader starts: it queries the terminal and reads the reply from stdin.
    let mut terminal = crate::tui::enter()?;

    let (ask_tx, mut ask_rx) = unbounded_channel::<AskPassRequest>();
    let sock = askpass::start_server(&runtime_dir, ask_tx)?;
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(req) = ask_rx.recv().await {
                if tx.send(AppEvent::AskPass(req)).is_err() {
                    break;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut stream = EventStream::new();
            while let Some(Ok(ev)) = stream.next().await {
                if tx.send(AppEvent::Input(ev)).is_err() {
                    break;
                }
            }
        });
    }
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                if tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });
    }

    let mut app = App::new(cfg, tx, sock.clone(), runtime_dir)?;
    if app.store.hosts.is_empty() {
        app.info("No hosts yet: n creates one, i imports (e.g. ~/.ssh/config)");
    }

    let result: Result<()> = async {
        let mut last_draw: Option<Instant> = None;
        loop {
            let next_frame = last_draw.map(|t| t + FRAME);
            if app.dirty && next_frame.is_none_or(|t| Instant::now() >= t) {
                app.dirty = false;
                terminal.draw(|f| crate::ui::draw(f, &mut app))?;
                last_draw = Some(Instant::now());
            }
            let ev = match next_frame.filter(|_| app.dirty) {
                Some(at) => tokio::select! {
                    ev = rx.recv() => Some(ev),
                    () = tokio::time::sleep_until(at.into()) => None,
                },
                None => Some(rx.recv().await),
            };
            let Some(ev) = ev else { continue };
            let Some(ev) = ev else { break };
            app.handle(ev);
            let mut n = 0;
            while let Ok(ev) = rx.try_recv() {
                app.handle(ev);
                n += 1;
                if n > 512 {
                    break;
                }
            }
            app.persist_ui_state();
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    crate::tui::leave()?;
    let _ = std::fs::remove_file(&sock);
    result
}

#[cfg(test)]
pub(crate) mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::import::test_support::TempDir;
    use crate::store::hosts::Host;

    pub fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    /// An App over a scratch directory holding hosts given as (name, folder); no terminal involved.
    pub fn app_with_hosts(hosts: &[(&str, &str)]) -> (App, TempDir) {
        let dir = TempDir::new();
        let mut store = dir.store();
        for (name, folder) in hosts {
            store.upsert(Host { name: (*name).into(), folder: (*folder).into(), host: format!("{name}.local"), ..Host::default() });
        }
        let parts = AppParts {
            cfg: Config::default(),
            store,
            vault: Vault::new(dir.0.join("vault.age")),
            state: UiState::default(),
            state_path: dir.0.join("state.toml"),
            askpass_sock: dir.0.join("askpass.sock"),
            runtime_dir: dir.0.clone(),
            truecolor: false,
        };
        let (tx, _rx) = unbounded_channel();
        (App::from_parts(parts, tx), dir)
    }

    #[test]
    fn ui_state_goes_to_its_own_path() {
        let (mut app, dir) = app_with_hosts(&[("web1", "acme/prod")]);
        app.sidebar.items(&app.store);
        app.sidebar.expanded.insert("acme/prod".into());
        app.persist_ui_state();
        assert_eq!(UiState::load_from(&dir.0.join("state.toml")).expanded, Some(vec!["acme".to_string(), "acme/prod".to_string()]));
    }
}
