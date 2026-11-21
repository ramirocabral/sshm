use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use anyhow::{Context, Result};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub type PaneId = u64;

pub enum PaneNotify {
    Output,
    Exited(Option<u32>),
    Title(String),
    Clipboard(String),
    Bell,
}

pub type Notifier = Arc<dyn Fn(PaneId, PaneNotify) + Send + Sync>;

/// Bytes for the PTY input; a writer thread drains it so a stalled session never blocks the caller.
type InputQueue = Sender<Vec<u8>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

pub struct Listener {
    id: PaneId,
    input: InputQueue,
    notify: Notifier,
    size: Arc<Mutex<GridSize>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                let _ = self.input.send(text.into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *lock(&self.size);
                let reply = format(WindowSize { num_lines: size.rows, num_cols: size.cols, cell_width: 8, cell_height: 16 });
                let _ = self.input.send(reply.into_bytes());
            }
            Event::Title(t) => (self.notify)(self.id, PaneNotify::Title(t)),
            Event::ClipboardStore(_, text) => (self.notify)(self.id, PaneNotify::Clipboard(text)),
            Event::Bell => (self.notify)(self.id, PaneNotify::Bell),
            _ => {}
        }
    }
}

pub struct Emulator {
    pub term: Term<Listener>,
    parser: Processor,
}

impl Emulator {
    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Text on the cursor line up to the cursor, used to detect password prompts.
    pub fn cursor_line_text(&self) -> String {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = &grid[cursor.line];
        let mut s = String::new();
        for col in 0..cursor.column.0.min(grid.columns()) {
            s.push(row[Column(col)].c);
        }
        s
    }

    pub fn cursor_line(&self) -> i32 {
        self.term.grid().cursor.point.line.0
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    /// Converts a viewport cell into a grid point (accounts for scrollback offset).
    pub fn viewport_point(&self, col: u16, row: u16) -> Point {
        let offset = self.term.grid().display_offset() as i32;
        let cols = self.term.grid().columns().saturating_sub(1);
        Point::new(Line(row as i32 - offset), Column((col as usize).min(cols)))
    }
}

#[cfg(test)]
impl Emulator {
    pub fn headless(size: GridSize) -> Self {
        let listener = Listener { id: 0, input: channel().0, notify: Arc::new(|_, _| {}), size: Arc::new(Mutex::new(size)) };
        Emulator { term: Term::new(TermConfig::default(), &size, listener), parser: Processor::new() }
    }
}

pub struct TermPane {
    emu: Arc<Mutex<Emulator>>,
    input: InputQueue,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: Arc<Mutex<GridSize>>,
    dirty: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl TermPane {
    pub fn spawn(id: PaneId, cmd: CommandBuilder, size: GridSize, scrollback: usize, notify: Notifier) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize { rows: size.rows.max(1), cols: size.cols.max(1), pixel_width: 0, pixel_height: 0 })
            .context("could not open a PTY")?;
        let mut child = pair.slave.spawn_command(cmd).context("could not start the process")?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let master_fd = pair.master.as_raw_fd().context("the PTY has no file descriptor")?;
        // SAFETY: the fd belongs to pair.master, which is alive for the whole borrow.
        let fd = unsafe { BorrowedFd::borrow_raw(master_fd) };
        let (read_fd, write_fd) = (fd.try_clone_to_owned()?, fd.try_clone_to_owned()?);
        // Both ends share one open file description, so the reader polls too.
        set_nonblocking(&read_fd)?;
        let mut reader = File::from(read_fd);
        let closed = Arc::new(AtomicBool::new(false));
        let (input, input_rx) = channel();
        let writer_closed = closed.clone();
        std::thread::Builder::new()
            .name(format!("pty-in-{id}"))
            .spawn(move || write_loop(File::from(write_fd), input_rx, &writer_closed))?;
        let shared_size = Arc::new(Mutex::new(size));

        let listener = Listener { id, input: input.clone(), notify: notify.clone(), size: shared_size.clone() };
        let config = TermConfig { scrolling_history: scrollback, ..TermConfig::default() };
        let term = Term::new(config, &size, listener);
        let emu = Arc::new(Mutex::new(Emulator { term, parser: Processor::new() }));
        let dirty = Arc::new(AtomicBool::new(false));

        let thread_emu = emu.clone();
        let thread_dirty = dirty.clone();
        std::thread::Builder::new().name(format!("pty-{id}")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut e) = thread_emu.lock() {
                            e.advance(&buf[..n]);
                        }
                        if !thread_dirty.swap(true, Ordering::AcqRel) {
                            notify(id, PaneNotify::Output);
                        }
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {
                        if !wait_for(&reader, libc::POLLIN, -1) {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let code = child.wait().ok().map(|s| s.exit_code());
            notify(id, PaneNotify::Exited(code));
        })?;

        Ok(TermPane { emu, input, master: pair.master, killer, size: shared_size, dirty, closed })
    }

    pub fn lock(&self) -> MutexGuard<'_, Emulator> {
        self.emu.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    /// Queues bytes for the session; never blocks.
    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            let _ = self.input.send(bytes.to_vec());
        }
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let _ = self.input.send(paste_bytes(text, bracketed));
    }

    pub fn size(&self) -> GridSize {
        *lock(&self.size)
    }

    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 || self.size() == size {
            return;
        }
        *lock(&self.size) = size;
        let _ = self.master.resize(PtySize { rows: size.rows, cols: size.cols, pixel_width: 0, pixel_height: 0 });
        self.lock().term.resize(size);
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

impl Drop for TermPane {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.input.send(Vec::new());
        self.kill();
    }
}

fn set_nonblocking(fd: &OwnedFd) -> std::io::Result<()> {
    let raw = fd.as_raw_fd();
    // SAFETY: plain fcntl calls on an fd we own.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Polls `file` for `events` (-1 = no timeout); false once the PTY is gone and waiting again would spin.
fn wait_for(file: &File, events: libc::c_short, timeout_ms: libc::c_int) -> bool {
    let mut pfd = libc::pollfd { fd: file.as_raw_fd(), events, revents: 0 };
    // SAFETY: one valid pollfd for the duration of the call.
    let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if n < 0 {
        return std::io::Error::last_os_error().kind() == ErrorKind::Interrupted;
    }
    pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0 || pfd.revents & events != 0
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let clean = text.replace("\x1b[201~", "");
        format!("\x1b[200~{clean}\x1b[201~").into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Drains the input queue into the non-blocking PTY; ends when the pane closes, the senders are gone or the PTY fails.
fn write_loop(mut out: File, rx: Receiver<Vec<u8>>, closed: &AtomicBool) {
    while let Ok(mut buf) = rx.recv() {
        while buf.len() < 64 * 1024 {
            match rx.try_recv() {
                Ok(more) => buf.extend_from_slice(&more),
                Err(_) => break,
            }
        }
        let mut off = 0;
        while off < buf.len() {
            if closed.load(Ordering::Acquire) {
                return;
            }
            match out.write(&buf[off..]) {
                Ok(0) => return,
                Ok(n) => off += n,
                Err(e) if e.kind() == ErrorKind::Interrupted => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if !wait_for(&out, libc::POLLOUT, 200) {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
        if closed.load(Ordering::Acquire) {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(cmd: &str) -> TermPane {
        let mut c = CommandBuilder::new("/bin/sh");
        c.args(["-c", cmd]);
        TermPane::spawn(1, c, GridSize { cols: 80, rows: 24 }, 100, Arc::new(|_, _| {})).unwrap()
    }

    fn input_threads() -> usize {
        std::fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .filter(|t| std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == "pty-in-1"))
            .count()
    }

    #[test]
    fn writer_keeps_order_and_ends_with_the_senders() {
        let path = std::env::temp_dir().join(format!("sshm-pty-out-{}", std::process::id()));
        let (tx, rx) = channel();
        let out = File::create(&path).unwrap();
        let t = std::thread::spawn(move || write_loop(out, rx, &AtomicBool::new(false)));
        for i in 0..1000u32 {
            tx.send(i.to_string().into_bytes()).unwrap();
        }
        drop(tx);
        t.join().unwrap();
        let want: String = (0..1000u32).map(|i| i.to_string()).collect();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn closing_a_stalled_pane_ends_its_writer() {
        let pane = spawn("sleep 30");
        pane.write(&vec![b'x'; 4 << 20]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert_eq!(input_threads(), 1, "the writer is stalled on the full PTY");
        drop(pane);
        let gone = (0..50).any(|_| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            input_threads() == 0
        });
        assert!(gone, "the writer thread outlived its pane");
    }

    #[test]
    fn paste_modes() {
        assert_eq!(paste_bytes("a\r\nb\nc", false), b"a\rb\rc");
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }
}
