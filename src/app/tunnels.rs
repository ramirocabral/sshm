//! Port forwarding. The running tunnels of a host share one `ssh -N` of their own with its own
//! control socket; each tunnel is added to it and removed with `ssh -O forward/cancel`, so they go
//! on and off one by one and the host authenticates once. A connection that drops comes back with
//! backoff; one that never came up, or whose credentials were refused, fails its tunnels.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use super::dialogs::*;
use super::form::Choice;
use super::keymap::{self, Action, Ctx};
use super::{App, AppEvent, Focus, Owner, Tab, TabKind};
use crate::ssh::command::{self, AskPassEnv, MuxOp, Purpose, SshSpec};
use crate::store::hosts::{HostStore, Tunnel, TunnelKind, TunnelStart, new_id};

/// A tunnel: (host id, tunnel id).
pub type Key = (String, String);

/// Seconds before each reconnection attempt in a row.
const RETRY_SECS: [u64; 6] = [1, 2, 5, 10, 30, 60];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelState {
    /// Waiting for the connection, or for ssh to take the forward.
    Starting,
    On,
    Failed(String),
}

impl TunnelState {
    /// Meant to be running: it keeps its host's connection open.
    pub fn wanted(&self) -> bool {
        matches!(self, TunnelState::Starting | TunnelState::On)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Up,
    /// Lost; the next attempt runs at this time.
    Retrying(Instant),
}

pub struct Conn {
    pub run: u64,
    pub state: ConnState,
    /// The last thing ssh said: why it failed, or a connection through a tunnel that did not go through.
    pub message: Option<String>,
    socket: PathBuf,
    token: String,
    stop: Option<oneshot::Sender<()>>,
    pid: Option<u32>,
    /// It authenticated at least once, so losing it later is worth retrying.
    ever_up: bool,
    up_since: Option<Instant>,
    attempts: usize,
}

pub enum TunnelEvent {
    Up { host: String, run: u64 },
    Line { host: String, run: u64, line: String },
    Exited { host: String, run: u64 },
    Forwarded { host: String, run: u64, tunnel: String, result: Result<(), String> },
}

#[derive(Default)]
pub struct Tunnels {
    /// By host id.
    pub conns: HashMap<String, Conn>,
    /// A tunnel without an entry is off.
    pub states: HashMap<Key, TunnelState>,
    next_run: u64,
}

impl Tunnels {
    pub fn state(&self, host: &str, tunnel: &str) -> Option<&TunnelState> {
        self.states.get(&(host.to_string(), tunnel.to_string()))
    }

    pub fn running(&self) -> usize {
        self.states.values().filter(|s| **s == TunnelState::On).count()
    }

    /// Whether `host` has a tunnel on right now.
    pub fn is_on(&self, host: &str) -> bool {
        self.states.iter().any(|((h, _), s)| h == host && *s == TunnelState::On)
    }

    fn wanted(&self, host: &str) -> bool {
        self.states.iter().any(|((h, _), s)| h == host && s.wanted())
    }

    fn tunnels_of(&self, host: &str, pick: impl Fn(&TunnelState) -> bool) -> Vec<String> {
        self.states.iter().filter(|((h, _), s)| h == host && pick(s)).map(|((_, t), _)| t.clone()).collect()
    }

    /// Ends every connection; called on quit.
    pub fn shutdown(&mut self) {
        for (_, c) in self.conns.drain() {
            if let Some(pid) = c.pid {
                // SAFETY: kill(2) on a child of ours; SIGTERM lets ssh remove its socket.
                unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            }
            let _ = std::fs::remove_file(&c.socket);
        }
    }
}

#[derive(Default)]
pub struct TunnelsView {
    pub selected: usize,
    pub scroll: usize,
    /// Listed even without tunnels, so n can add its first one: the host T was pressed on.
    pub extra: Option<String>,
}

/// Rows of the tunnels tab: each host that has tunnels (or is `extra`), followed by its tunnels.
pub fn rows(store: &HostStore, extra: Option<&str>) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    for h in store.hosts.iter().filter(|h| !h.tunnels.is_empty() || extra == Some(h.id.as_str())) {
        out.push((h.id.clone(), None));
        out.extend(h.tunnels.iter().map(|t| (h.id.clone(), Some(t.id.clone()))));
    }
    out
}

/// What a line from a connection's stderr tells the user, if anything.
fn ssh_message(line: &str) -> Option<String> {
    let l = line.trim();
    if l.is_empty() || l.starts_with("Warning: Permanently added") {
        return None;
    }
    if l.contains("administratively prohibited") {
        return Some("the server does not allow port forwarding (AllowTcpForwarding in its sshd_config)".into());
    }
    if let Some((_, why)) = l.strip_prefix("channel ").and_then(|r| r.split_once(": open failed: ")) {
        return Some(format!("a connection through a tunnel failed: {why}"));
    }
    Some(l.to_string())
}

/// Failures that trying again will not fix.
fn is_fatal(reason: &str) -> bool {
    ["Permission denied", "Host key verification failed", "REMOTE HOST IDENTIFICATION HAS CHANGED", "Too many authentication failures"]
        .iter()
        .any(|p| reason.contains(p))
}

/// The reason in the output of a failed `ssh -O forward`.
fn mux_error(stderr: &str) -> String {
    let lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let reason = lines
        .clone()
        .find_map(|l| l.split_once("request failed: ").map(|(_, r)| r))
        .or_else(|| lines.clone().next())
        .unwrap_or("ssh refused the forward");
    match reason.strip_prefix("remote port forwarding failed for listen port ") {
        Some(port) => format!("the server could not listen on port {port} (in use, or not allowed by its sshd)"),
        None => reason.to_string(),
    }
}

/// ssh takes a local forward as working when it could listen on any address of the name (say ::1
/// while 127.0.0.1:PORT belongs to something else), so the port is tried here first.
fn check_local_port(t: &Tunnel) -> Result<(), String> {
    if t.kind == TunnelKind::Remote {
        return Ok(());
    }
    let addr = match t.bind_address.trim() {
        "" | "localhost" => "127.0.0.1",
        "*" => "0.0.0.0",
        a => a.trim_start_matches('[').trim_end_matches(']'),
    };
    match std::net::TcpListener::bind((addr, t.bind_port)) {
        Err(e) if e.kind() == ErrorKind::AddrInUse => Err(format!("port {} is already in use on this machine", t.bind_port)),
        Err(e) if e.kind() == ErrorKind::PermissionDenied => Err(format!("port {} needs root (ports below 1024)", t.bind_port)),
        _ => Ok(()),
    }
}

async fn run_mux(spec: SshSpec) -> Result<(), String> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args).stdin(Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(Duration::from_secs(30), cmd.output()).await {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => Err(mux_error(&String::from_utf8_lossy(&out.stderr))),
        Ok(Err(e)) => Err(format!("could not run {}: {e}", spec.program)),
        Err(_) => Err("ssh did not answer".into()),
    }
}

/// The connection dies with sshm instead of holding its ports after a crash.
fn die_with_parent(cmd: &mut Command) {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe and touches nothing shared with the parent.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
}

/// Watches a connection: reports when its socket shows up (it authenticated), passes its stderr
/// on, and stops it with SIGTERM (so ssh removes the socket) when `stop` fires or is dropped.
async fn supervise(
    mut child: Child,
    socket: PathBuf,
    mut stop: oneshot::Receiver<()>,
    tx: UnboundedSender<AppEvent>,
    host: String,
    run: u64,
) {
    let send = |ev: TunnelEvent| {
        let _ = tx.send(AppEvent::Tunnel(ev));
    };
    let reader = child.stderr.take().map(|err| {
        let (tx, host) = (tx.clone(), host.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx.send(AppEvent::Tunnel(TunnelEvent::Line { host: host.clone(), run, line }));
            }
        })
    });
    let mut up = false;
    let mut poll = tokio::time::interval(Duration::from_millis(100));
    let stopped = loop {
        tokio::select! {
            _ = child.wait() => break false,
            _ = &mut stop => break true,
            _ = poll.tick(), if !up => {
                if socket.exists() {
                    up = true;
                    send(TunnelEvent::Up { host: host.clone(), run });
                }
            }
        }
    };
    if stopped {
        if let Some(pid) = child.id() {
            // SAFETY: kill(2) on our child, which has not been reaped yet.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
    }
    if let Some(r) = reader {
        // The last lines say why it ended; a ProxyJump child may hold the pipe a moment longer.
        let _ = tokio::time::timeout(Duration::from_secs(1), r).await;
    }
    let _ = std::fs::remove_file(&socket);
    send(TunnelEvent::Exited { host, run });
}

/// Removes sockets left by instances that died without cleaning up (named tun-<pid>-<n>.sock).
pub fn remove_stale_sockets(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("tun-").and_then(|r| r.split_once('-')).map(|(pid, _)| pid) else { continue };
        if pid.parse::<u32>().is_ok() && !Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl App {
    fn conn_mut(&mut self, host: &str, run: u64) -> Option<&mut Conn> {
        self.tunnels.conns.get_mut(host).filter(|c| c.run == run)
    }

    pub fn tunnel_start(&mut self, host: &str, tunnel: &str) {
        let key = (host.to_string(), tunnel.to_string());
        if self.tunnels.states.get(&key).is_some_and(TunnelState::wanted) || self.store.tunnel(host, tunnel).is_none() {
            return;
        }
        self.tunnels.states.insert(key, TunnelState::Starting);
        match self.tunnels.conns.get(host).map(|c| c.state) {
            Some(ConnState::Up) => self.request_forward(host, tunnel),
            Some(ConnState::Connecting) => {}
            Some(ConnState::Retrying(_)) | None => self.connect_tunnels(host),
        }
    }

    /// Call it before the tunnel changes in the store: cancelling needs its old spec.
    pub fn tunnel_stop(&mut self, host: &str, tunnel: &str) {
        if self.tunnels.states.remove(&(host.to_string(), tunnel.to_string())) == Some(TunnelState::On) {
            self.request_cancel(host, tunnel);
        }
        if !self.tunnels.wanted(host) {
            self.disconnect_tunnels(host);
        }
    }

    fn connect_tunnels(&mut self, host_id: &str) {
        let Some(host) = self.store.get(host_id).cloned() else { return };
        let prev = self.tunnels.conns.remove(host_id);
        let (ever_up, attempts) = prev.as_ref().map(|p| (p.ever_up, p.attempts)).unwrap_or((false, 0));
        if let Some(mut p) = prev {
            self.owners.remove(&p.token);
            if let Some(stop) = p.stop.take() {
                let _ = stop.send(());
            }
        }
        self.tunnels.next_run += 1;
        let run = self.tunnels.next_run;
        let token = new_id();
        let label = format!("{} (tunnel)", host.name);
        self.owners.insert(token.clone(), Owner { host_id: host.id.clone(), label, password_attempts: 0, passphrase_attempts: 0 });
        let socket = self.runtime_dir.join(format!("tun-{}-{run}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let ask = AskPassEnv { socket: self.askpass_sock.clone(), token: token.clone() };
        let spec = command::build(&host, &self.cfg, Some(&ask), None, Purpose::Tunnel(socket.clone()));
        let mut cmd = Command::new(&spec.program);
        cmd.args(&spec.args)
            .envs(spec.env.iter().cloned())
            .current_dir(crate::paths::home())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        die_with_parent(&mut cmd);
        tracing::info!(host = %host.name, "opening the tunnel connection");
        match cmd.spawn() {
            Ok(child) => {
                let (stop, stop_rx) = oneshot::channel();
                let pid = child.id();
                tokio::spawn(supervise(child, socket.clone(), stop_rx, self.tx.clone(), host.id.clone(), run));
                let conn = Conn {
                    run,
                    state: ConnState::Connecting,
                    message: None,
                    socket,
                    token,
                    stop: Some(stop),
                    pid,
                    ever_up,
                    up_since: None,
                    attempts,
                };
                self.tunnels.conns.insert(host.id, conn);
            }
            Err(e) => {
                self.owners.remove(&token);
                self.error(format!("Could not run {}: {e}", spec.program));
                self.fail_tunnels(&host.id, format!("could not run {}: {e}", spec.program));
            }
        }
    }

    fn disconnect_tunnels(&mut self, host: &str) {
        if let Some(mut c) = self.tunnels.conns.remove(host) {
            self.owners.remove(&c.token);
            if let Some(stop) = c.stop.take() {
                let _ = stop.send(());
            }
        }
    }

    /// The host's running tunnels fail with `reason`, and its connection goes.
    fn fail_tunnels(&mut self, host: &str, reason: String) {
        for ((h, _), s) in self.tunnels.states.iter_mut() {
            if h == host && s.wanted() {
                *s = TunnelState::Failed(reason.clone());
            }
        }
        self.disconnect_tunnels(host);
    }

    fn request_forward(&mut self, host: &str, tunnel: &str) {
        let Some(c) = self.tunnels.conns.get(host).filter(|c| c.state == ConnState::Up) else { return };
        let Some(t) = self.store.tunnel(host, tunnel).cloned() else { return };
        let spec = command::mux(&self.cfg, &c.socket, MuxOp::Forward, &t.forward_args());
        let (tx, run, host, tunnel) = (self.tx.clone(), c.run, host.to_string(), tunnel.to_string());
        tokio::spawn(async move {
            let checked = tokio::task::spawn_blocking(move || check_local_port(&t)).await.unwrap_or(Ok(()));
            let result = match checked {
                Ok(()) => run_mux(spec).await,
                Err(e) => Err(e),
            };
            let _ = tx.send(AppEvent::Tunnel(TunnelEvent::Forwarded { host, run, tunnel, result }));
        });
    }

    fn request_cancel(&self, host: &str, tunnel: &str) {
        let Some(c) = self.tunnels.conns.get(host).filter(|c| c.state == ConnState::Up) else { return };
        let Some(t) = self.store.tunnel(host, tunnel) else { return };
        let spec = command::mux(&self.cfg, &c.socket, MuxOp::Cancel, &t.forward_args());
        tokio::spawn(async move {
            if let Err(e) = run_mux(spec).await {
                tracing::warn!("could not cancel a forward: {e}");
            }
        });
    }

    pub fn handle_tunnel_event(&mut self, ev: TunnelEvent) {
        match ev {
            TunnelEvent::Up { host, run } => {
                let Some(c) = self.conn_mut(&host, run) else { return };
                c.state = ConnState::Up;
                c.ever_up = true;
                c.up_since = Some(Instant::now());
                c.message = None;
                for t in self.tunnels.tunnels_of(&host, |s| *s == TunnelState::Starting) {
                    self.request_forward(&host, &t);
                }
            }
            TunnelEvent::Line { host, run, line } => {
                tracing::info!(host = %host, "tunnel ssh: {line}");
                if let Some(msg) = ssh_message(&line)
                    && let Some(c) = self.conn_mut(&host, run)
                {
                    c.message = Some(msg);
                }
            }
            TunnelEvent::Exited { host, run } => self.tunnel_conn_exited(&host, run),
            TunnelEvent::Forwarded { host, run, tunnel, result } => {
                // A reply that lost the race with the end of its connection: the next one forwards again.
                if !self.conn_mut(&host, run).is_some_and(|c| c.state == ConnState::Up) {
                    return;
                }
                let key = (host.clone(), tunnel.clone());
                match (self.tunnels.states.get(&key), result) {
                    (Some(TunnelState::Starting), Ok(())) => {
                        self.tunnels.states.insert(key, TunnelState::On);
                    }
                    (Some(TunnelState::Starting), Err(e)) => {
                        let title = self.store.tunnel(&host, &tunnel).map(Tunnel::title).unwrap_or_default();
                        self.error(format!("Tunnel {title}: {e}"));
                        self.tunnels.states.insert(key, TunnelState::Failed(e));
                        if !self.tunnels.wanted(&host) {
                            self.disconnect_tunnels(&host);
                        }
                    }
                    // Turned off while ssh was setting it up.
                    (None | Some(TunnelState::Failed(_)), Ok(())) => self.request_cancel(&host, &tunnel),
                    _ => {}
                }
            }
        }
    }

    fn tunnel_conn_exited(&mut self, host: &str, run: u64) {
        if self.conn_mut(host, run).is_none() {
            return;
        }
        let Some(c) = self.tunnels.conns.remove(host) else { return };
        self.owners.remove(&c.token);
        if !self.tunnels.wanted(host) {
            return;
        }
        let name = self.store.get(host).map(|h| h.name.clone()).unwrap_or_default();
        let reason = c.message.clone().unwrap_or_else(|| "the connection closed".into());
        if !c.ever_up || is_fatal(&reason) {
            self.error(format!("Tunnels through {name}: {reason}"));
            self.fail_tunnels(host, reason);
            return;
        }
        if c.state == ConnState::Up {
            self.error(format!("Tunnels through {name}: {reason} · reconnecting"));
        }
        let attempts = if c.up_since.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) { 0 } else { c.attempts };
        let delay = Duration::from_secs(RETRY_SECS[attempts.min(RETRY_SECS.len() - 1)]);
        for ((h, _), s) in self.tunnels.states.iter_mut() {
            if h == host && *s == TunnelState::On {
                *s = TunnelState::Starting;
            }
        }
        let retry = Conn {
            state: ConnState::Retrying(Instant::now() + delay),
            message: Some(reason),
            stop: None,
            pid: None,
            up_since: None,
            attempts: attempts + 1,
            ..c
        };
        self.tunnels.conns.insert(host.to_string(), retry);
    }

    /// Runs the reconnections that are due and drops what the host list no longer has (deleted
    /// here or by another instance). Returns whether anything changed.
    pub fn tunnels_tick(&mut self) -> bool {
        let now = Instant::now();
        let due: Vec<String> = self
            .tunnels
            .conns
            .iter()
            .filter(|(_, c)| matches!(c.state, ConnState::Retrying(at) if at <= now))
            .map(|(h, _)| h.clone())
            .collect();
        let mut changed = !due.is_empty();
        for h in due {
            self.connect_tunnels(&h);
        }
        let gone: Vec<Key> = self.tunnels.states.keys().filter(|(h, t)| self.store.tunnel(h, t).is_none()).cloned().collect();
        for key in gone {
            changed = true;
            let was_on = self.tunnels.states.remove(&key) == Some(TunnelState::On);
            // Its forward cannot be cancelled without its spec: a new connection drops it.
            if was_on && self.tunnels.wanted(&key.0) {
                self.restart_tunnels(&key.0);
            }
        }
        let idle: Vec<String> = self.tunnels.conns.keys().filter(|h| !self.tunnels.wanted(h)).cloned().collect();
        for h in idle {
            changed = true;
            self.disconnect_tunnels(&h);
        }
        changed
    }

    /// A new connection for the host; its tunnels that were on or failed start again.
    pub(crate) fn restart_tunnels(&mut self, host: &str) {
        let ids: Vec<String> =
            self.tunnels.tunnels_of(host, |_| true).into_iter().filter(|t| self.store.tunnel(host, t).is_some()).collect();
        if ids.is_empty() {
            self.info("No tunnel of this host is on: Enter turns one on");
            return;
        }
        self.disconnect_tunnels(host);
        for t in ids {
            self.tunnels.states.insert((host.to_string(), t), TunnelState::Starting);
        }
        self.connect_tunnels(host);
    }

    /// Turns on the tunnels set to start at `when`, on every host or only on `host`.
    pub fn autostart_tunnels(&mut self, when: TunnelStart, host: Option<&str>) {
        let todo: Vec<Key> = self
            .store
            .hosts
            .iter()
            .filter(|h| host.is_none_or(|id| id == h.id))
            .flat_map(|h| h.tunnels.iter().filter(|t| t.start == when).map(|t| (h.id.clone(), t.id.clone())))
            .collect();
        for (h, t) in todo {
            self.tunnel_start(&h, &t);
        }
    }

    // ---------- the tunnels tab ----------

    fn tunnels_view(&mut self) -> Option<&mut TunnelsView> {
        match &mut self.tabs.get_mut(self.active)?.kind {
            TabKind::Tunnels(v) => Some(v),
            _ => None,
        }
    }

    /// Opens (or goes to) the tunnels tab, on the host of the current session or host list row.
    pub fn open_tunnels(&mut self) {
        let host = self.context_host();
        let idx = match self.tabs.iter().position(|t| matches!(t.kind, TabKind::Tunnels(_))) {
            Some(i) => i,
            None => {
                let id = self.next_id();
                self.tabs.push(Tab { id, title: None, kind: TabKind::Tunnels(TunnelsView::default()) });
                self.tabs.len() - 1
            }
        };
        self.active = idx;
        self.focus = Focus::Main;
        let Some(h) = host else { return };
        let TabKind::Tunnels(view) = &mut self.tabs[idx].kind else { return };
        view.extra = Some(h.clone());
        if let Some(pos) = rows(&self.store, view.extra.as_deref()).iter().position(|(id, t)| *id == h && t.is_none()) {
            view.selected = pos;
        }
    }

    /// The host and tunnel under the cursor in the tunnels tab.
    pub(crate) fn tunnel_selection(&self) -> Option<(String, Option<String>)> {
        let TabKind::Tunnels(v) = &self.tabs.get(self.active)?.kind else { return None };
        rows(&self.store, v.extra.as_deref()).into_iter().nth(v.selected)
    }

    fn select_tunnel_row(&mut self, host: &str, tunnel: &str) {
        let Some(TabKind::Tunnels(v)) = self.tabs.get_mut(self.active).map(|t| &mut t.kind) else { return };
        if let Some(pos) = rows(&self.store, v.extra.as_deref()).iter().position(|(h, t)| h == host && t.as_deref() == Some(tunnel)) {
            v.selected = pos;
        }
    }

    pub fn tunnels_key(&mut self, key: KeyEvent) {
        let Some(action) = keymap::lookup(&[Ctx::Motion, Ctx::Tunnels], &key, &self.leader) else { return };
        let len = match self.tabs.get(self.active).map(|t| &t.kind) {
            Some(TabKind::Tunnels(v)) => rows(&self.store, v.extra.as_deref()).len(),
            _ => return,
        };
        let last = len.saturating_sub(1);
        let Some(v) = self.tunnels_view() else { return };
        match action {
            Action::Down => v.selected = (v.selected + 1).min(last),
            Action::Up => v.selected = v.selected.saturating_sub(1),
            Action::PageDown => v.selected = (v.selected + 10).min(last),
            Action::PageUp => v.selected = v.selected.saturating_sub(10),
            Action::Top => v.selected = 0,
            Action::Bottom => v.selected = last,
            other => self.run_action(other),
        }
    }

    pub fn tunnels_mouse(&mut self, m: MouseEvent) {
        let list = self.layout.tunnel_list;
        let len = self.tunnel_rows_len();
        let Some(v) = self.tunnels_view() else { return };
        match m.kind {
            MouseEventKind::ScrollUp => v.selected = v.selected.saturating_sub(3),
            MouseEventKind::ScrollDown => v.selected = (v.selected + 3).min(len.saturating_sub(1)),
            MouseEventKind::Down(MouseButton::Left) if list.contains(Position::new(m.column, m.row)) => {
                let idx = v.scroll + (m.row - list.y) as usize;
                if idx < len {
                    let again = v.selected == idx;
                    v.selected = idx;
                    if again {
                        self.tunnel_action(Action::TunnelToggle);
                    }
                }
            }
            _ => {}
        }
    }

    fn tunnel_rows_len(&self) -> usize {
        match self.tabs.get(self.active).map(|t| &t.kind) {
            Some(TabKind::Tunnels(v)) => rows(&self.store, v.extra.as_deref()).len(),
            _ => 0,
        }
    }

    /// What the tunnel actions do on the row under the cursor; a new tunnel also works from anywhere.
    pub(crate) fn tunnel_action(&mut self, action: Action) {
        let sel = self.tunnel_selection();
        if action == Action::TunnelNew {
            let host = sel.map(|(h, _)| h).or_else(|| self.context_host());
            return self.open_tunnel_form(host, None);
        }
        let Some((host, tunnel)) = sel else { return };
        match (action, tunnel) {
            (Action::TunnelToggle, Some(t)) => {
                if self.tunnels.state(&host, &t).is_some_and(TunnelState::wanted) {
                    self.tunnel_stop(&host, &t);
                } else {
                    self.tunnel_start(&host, &t);
                }
            }
            (Action::TunnelToggle, None) if self.tunnels.wanted(&host) => {
                self.tunnels.states.retain(|(h, _), s| h != &host || !s.wanted());
                self.disconnect_tunnels(&host);
            }
            (Action::TunnelToggle, None) => {
                let ids: Vec<String> = self.store.get(&host).map(|h| h.tunnels.iter().map(|t| t.id.clone()).collect()).unwrap_or_default();
                if ids.is_empty() {
                    self.info("This host has no tunnels yet: n adds one");
                }
                for t in ids {
                    self.tunnel_start(&host, &t);
                }
            }
            (Action::TunnelEdit, Some(t)) => self.open_tunnel_form(Some(host), Some(&t)),
            (Action::TunnelEdit, None) => self.open_host_form(Some(host)),
            (Action::TunnelDelete, Some(t)) => {
                let title = self.store.tunnel(&host, &t).map(Tunnel::title).unwrap_or_default();
                self.push_dialog(Dialog::Confirm(ConfirmDialog {
                    title: "Delete tunnel".into(),
                    body: format!("Delete the tunnel {title}?"),
                    action: Some(ConfirmAction::DeleteTunnel { host, tunnel: t }),
                }));
            }
            (Action::TunnelRestart, _) => self.restart_tunnels(&host),
            (Action::TunnelCopy, Some(t)) => {
                let Some(t) = self.store.tunnel(&host, &t) else { return };
                let text = if t.kind == TunnelKind::Dynamic { format!("socks5://{}", t.listen()) } else { t.listen() };
                crate::clipboard::copy(&text, self.cfg.clipboard_command.as_deref());
                let where_ = if t.kind == TunnelKind::Remote { " (on the server)" } else { "" };
                self.info(format!("Copied: {text}{where_}"));
            }
            _ => {}
        }
    }

    fn open_tunnel_form(&mut self, host: Option<String>, tunnel: Option<&str>) {
        let hosts: Vec<Choice> = self.store.hosts.iter().map(|h| Choice::new(h.path(), h.path(), h.display_target())).collect();
        let h = host.as_deref().and_then(|id| self.store.get(id));
        let (host_id, path) = h.map(|h| (h.id.clone(), h.path())).unwrap_or_default();
        let (t, editing) = match tunnel.and_then(|t| self.store.tunnel(&host_id, t)) {
            Some(t) => (t.clone(), true),
            None => (Tunnel::default(), false),
        };
        self.push_dialog(Dialog::Tunnel(Box::new(TunnelForm::new(&t, &host_id, &path, hosts, editing))));
    }

    pub(crate) fn save_tunnel_form(&mut self, mut form: Box<TunnelForm>) {
        let path = form.host_path();
        let target = self.store.hosts.iter().find(|h| h.path() == path).map(|h| h.id.clone());
        let checked = match (form.to_tunnel(), target) {
            (Err(e), _) => Err(e),
            (Ok(_), None) if path.is_empty() => Err("pick the host the tunnel goes through".to_string()),
            (Ok(_), None) => Err(format!("there is no host called {path} (Enter in the field lists them)")),
            (Ok(t), Some(h)) => Ok((t, h)),
        };
        let (tunnel, host) = match checked {
            Ok(v) => v,
            Err(e) => {
                form.form.error = Some(e);
                self.dialogs.push_front(Dialog::Tunnel(form));
                return;
            }
        };
        let old = (form.host_id.clone(), form.id.clone());
        if form.editing && old.0 == host && self.store.tunnel(&host, &tunnel.id) == Some(&tunnel) {
            return;
        }
        let was_running = self.tunnels.states.get(&old).is_some_and(TunnelState::wanted);
        if form.editing {
            self.tunnel_stop(&old.0, &old.1);
            self.tunnels.states.remove(&old);
        }
        let in_place = old.0 == host
            && self
                .store
                .get_mut(&host)
                .and_then(|h| h.tunnels.iter_mut().find(|t| t.id == tunnel.id))
                .map(|slot| *slot = tunnel.clone())
                .is_some();
        if !in_place {
            if let Some(h) = self.store.get_mut(&old.0) {
                h.tunnels.retain(|t| t.id != tunnel.id);
            }
            if let Some(h) = self.store.get_mut(&host) {
                h.tunnels.push(tunnel.clone());
            }
        }
        self.persist_hosts();
        self.select_tunnel_row(&host, &tunnel.id);
        if !form.editing || was_running {
            self.tunnel_start(&host, &tunnel.id);
        }
        self.info(if form.editing { "Tunnel saved" } else { "Tunnel created and turned on (Enter turns it off)" });
    }

    pub(crate) fn delete_tunnel(&mut self, host: &str, tunnel: &str) {
        self.tunnel_stop(host, tunnel);
        if let Some(h) = self.store.get_mut(host) {
            h.tunnels.retain(|t| t.id != tunnel);
        }
        self.persist_hosts();
        self.info("Tunnel deleted");
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::UnboundedReceiver;

    use super::super::tests::app_with_events;
    use super::*;
    use crate::store::hosts::Host;

    #[test]
    fn messages_from_ssh() {
        assert_eq!(ssh_message("Warning: Permanently added '[x]:22' (ED25519) to the list of known hosts."), None);
        assert_eq!(
            ssh_message("channel 3: open failed: connect failed: Connection refused").as_deref(),
            Some("a connection through a tunnel failed: connect failed: Connection refused")
        );
        assert!(ssh_message("channel 2: open failed: administratively prohibited: open failed").unwrap().contains("AllowTcpForwarding"));
        assert!(is_fatal("tester@h: Permission denied (publickey,password)."));
        assert!(!is_fatal("ssh: connect to host h port 22: Network is unreachable"));

        let remote = "mux_client_forward: forwarding request failed: remote port forwarding failed for listen port 19000\n\
                      muxclient: master forward request failed\n";
        assert_eq!(mux_error(remote), "the server could not listen on port 19000 (in use, or not allowed by its sshd)");
        assert_eq!(
            mux_error("Control socket connect(/x): No such file or directory\n"),
            "Control socket connect(/x): No such file or directory"
        );
        assert_eq!(mux_error(""), "ssh refused the forward");
    }

    #[test]
    fn a_busy_local_port_is_caught_before_ssh() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let t = Tunnel { bind_port: port, dest_port: 80, ..Tunnel::default() };
        assert!(check_local_port(&t).unwrap_err().contains("already in use"));
        assert!(check_local_port(&Tunnel { kind: TunnelKind::Remote, ..t.clone() }).is_ok(), "remote ports are the server's");
        drop(taken);
        assert!(check_local_port(&t).is_ok());
    }

    #[test]
    fn rows_list_hosts_with_tunnels() {
        let (mut app, _dir) = super::super::tests::app_with_hosts(&[("a", ""), ("b", ""), ("c", "")]);
        app.store.hosts[2].tunnels = vec![Tunnel { bind_port: 1, ..Tunnel::default() }, Tunnel { bind_port: 2, ..Tunnel::default() }];
        let (b, c) = (app.store.hosts[1].id.clone(), app.store.hosts[2].id.clone());
        let r = rows(&app.store, None);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], (c.clone(), None));
        assert!(r[1..].iter().all(|(h, t)| *h == c && t.is_some()));
        let r = rows(&app.store, Some(&b));
        assert_eq!(r[0], (b, None), "the host T was pressed on shows up even without tunnels");
    }

    fn with_tunnel(name: &str) -> (App, crate::import::test_support::TempDir, UnboundedReceiver<AppEvent>, String, String) {
        let (mut app, dir, rx) = app_with_events(&[(name, "")]);
        let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let t = Tunnel { bind_port: free, dest_port: 2, ..Tunnel::default() };
        let tid = t.id.clone();
        app.store.hosts[0].tunnels.push(t);
        let hid = app.store.hosts[0].id.clone();
        (app, dir, rx, hid, tid)
    }

    async fn next_tunnel_event(rx: &mut UnboundedReceiver<AppEvent>) -> TunnelEvent {
        loop {
            match tokio::time::timeout(Duration::from_secs(10), rx.recv()).await.expect("timed out").expect("closed") {
                AppEvent::Tunnel(ev) => return ev,
                _ => continue,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_connection_that_never_comes_up_fails_its_tunnels() {
        let (mut app, _dir, mut rx, hid, tid) = with_tunnel("web1");
        app.cfg.ssh_command = "false".into();
        app.tunnel_start(&hid, &tid);
        assert_eq!(app.tunnels.state(&hid, &tid), Some(&TunnelState::Starting));
        assert_eq!(app.owners.len(), 1, "askpass can answer for the connection");
        loop {
            let ev = next_tunnel_event(&mut rx).await;
            let exited = matches!(ev, TunnelEvent::Exited { .. });
            app.handle_tunnel_event(ev);
            if exited {
                break;
            }
        }
        assert_eq!(app.tunnels.state(&hid, &tid), Some(&TunnelState::Failed("the connection closed".into())));
        assert!(app.tunnels.conns.is_empty() && app.owners.is_empty());

        app.cfg.ssh_command = "/nonexistent/ssh".into();
        app.tunnel_start(&hid, &tid);
        assert!(matches!(app.tunnels.state(&hid, &tid), Some(TunnelState::Failed(e)) if e.contains("could not run")));
    }

    /// A connection as if its ssh had just authenticated, with `true` answering the -O requests.
    fn fake_up(app: &mut App, hid: &str) -> u64 {
        app.cfg.ssh_command = "true".into();
        app.tunnels.next_run += 1;
        let run = app.tunnels.next_run;
        let conn = Conn {
            run,
            state: ConnState::Connecting,
            message: None,
            socket: PathBuf::from("/nonexistent.sock"),
            token: "tok".into(),
            stop: None,
            pid: None,
            ever_up: false,
            up_since: None,
            attempts: 0,
        };
        app.tunnels.conns.insert(hid.to_string(), conn);
        app.handle_tunnel_event(TunnelEvent::Up { host: hid.to_string(), run });
        run
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tunnels_turn_on_drop_and_come_back() {
        let (mut app, _dir, mut rx, hid, tid) = with_tunnel("web1");
        app.tunnels.states.insert((hid.clone(), tid.clone()), TunnelState::Starting);
        let run = fake_up(&mut app, &hid);
        let ev = next_tunnel_event(&mut rx).await;
        assert!(matches!(&ev, TunnelEvent::Forwarded { result: Ok(()), .. }));
        app.handle_tunnel_event(ev);
        assert_eq!(app.tunnels.state(&hid, &tid), Some(&TunnelState::On));
        assert!(app.tunnels.is_on(&hid));
        assert_eq!(app.tunnels.running(), 1);

        // The link drops: the tunnel waits for the reconnection.
        app.handle_tunnel_event(TunnelEvent::Line { host: hid.clone(), run, line: "Timeout, server h not responding.".into() });
        app.handle_tunnel_event(TunnelEvent::Exited { host: hid.clone(), run });
        assert_eq!(app.tunnels.state(&hid, &tid), Some(&TunnelState::Starting));
        let c = &app.tunnels.conns[&hid];
        assert!(matches!(c.state, ConnState::Retrying(_)));
        assert_eq!(c.message.as_deref(), Some("Timeout, server h not responding."));
        // Events from another process, or a late reply from this one, change nothing.
        app.handle_tunnel_event(TunnelEvent::Up { host: hid.clone(), run: run + 100 });
        assert!(matches!(app.tunnels.conns[&hid].state, ConnState::Retrying(_)));
        app.handle_tunnel_event(TunnelEvent::Forwarded { host: hid.clone(), run, tunnel: tid.clone(), result: Ok(()) });
        assert_eq!(app.tunnels.state(&hid, &tid), Some(&TunnelState::Starting));

        // Credentials refused on the way back: no more retries.
        let run = app.tunnels.next_run + 1;
        app.tunnels.next_run = run;
        app.tunnels.conns.get_mut(&hid).unwrap().run = run;
        app.tunnels.conns.get_mut(&hid).unwrap().message = Some("Permission denied (password).".into());
        app.handle_tunnel_event(TunnelEvent::Exited { host: hid.clone(), run });
        assert!(matches!(app.tunnels.state(&hid, &tid), Some(TunnelState::Failed(e)) if e.contains("Permission denied")));
        assert!(app.tunnels.conns.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_forward_fails_only_its_tunnel() {
        let (mut app, _dir, mut rx, hid, tid) = with_tunnel("web1");
        let other = Tunnel { bind_port: 3, dest_port: 4, ..Tunnel::default() };
        let oid = other.id.clone();
        app.store.hosts[0].tunnels.push(other);
        let run = fake_up(&mut app, &hid);
        app.tunnels.states.insert((hid.clone(), tid.clone()), TunnelState::On);
        app.tunnels.states.insert((hid.clone(), oid.clone()), TunnelState::Starting);
        app.handle_tunnel_event(TunnelEvent::Forwarded { host: hid.clone(), run, tunnel: oid.clone(), result: Err("nope".into()) });
        assert_eq!(app.tunnels.state(&hid, &oid), Some(&TunnelState::Failed("nope".into())));
        assert!(app.tunnels.conns.contains_key(&hid), "the other tunnel keeps the connection");

        app.tunnel_stop(&hid, &tid);
        assert!(app.tunnels.conns.is_empty(), "the last tunnel turned off closes the connection");
        while let Ok(ev) = rx.try_recv() {
            drop(ev);
        }
    }

    #[test]
    fn deleted_hosts_take_their_tunnels_along() {
        let (mut app, _dir) = super::super::tests::app_with_hosts(&[("web1", "")]);
        let t = Tunnel { bind_port: 1, dest_port: 2, ..Tunnel::default() };
        let tid = t.id.clone();
        app.store.upsert(Host { name: "b".into(), host: "b".into(), tunnels: vec![t], ..Host::default() });
        let hid = app.store.hosts.iter().find(|h| h.name == "b").unwrap().id.clone();
        app.tunnels.states.insert((hid.clone(), tid.clone()), TunnelState::Failed("x".into()));
        assert!(!app.tunnels_tick());
        app.delete_hosts(vec![hid.clone()]);
        assert!(app.tunnels_tick());
        assert!(app.tunnels.states.is_empty());
    }
}
