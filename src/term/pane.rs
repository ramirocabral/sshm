use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
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

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

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
    writer: SharedWriter,
    notify: Notifier,
    size: Arc<Mutex<GridSize>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(text) => {
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(text.as_bytes());
                    let _ = w.flush();
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *self.size.lock().unwrap();
                let reply = format(WindowSize { num_lines: size.rows, num_cols: size.cols, cell_width: 8, cell_height: 16 });
                if let Ok(mut w) = self.writer.lock() {
                    let _ = w.write_all(reply.as_bytes());
                }
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
        let listener = Listener {
            id: 0,
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            notify: Arc::new(|_, _| {}),
            size: Arc::new(Mutex::new(size)),
        };
        Emulator { term: Term::new(TermConfig::default(), &size, listener), parser: Processor::new() }
    }
}

pub struct TermPane {
    emu: Arc<Mutex<Emulator>>,
    writer: SharedWriter,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    size: Arc<Mutex<GridSize>>,
    dirty: Arc<AtomicBool>,
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
        let mut reader = pair.master.try_clone_reader()?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer()?));
        let shared_size = Arc::new(Mutex::new(size));

        let listener = Listener { id, writer: writer.clone(), notify: notify.clone(), size: shared_size.clone() };
        let config = TermConfig { scrolling_history: scrollback, ..TermConfig::default() };
        let term = Term::new(config, &size, listener);
        let emu = Arc::new(Mutex::new(Emulator { term, parser: Processor::new() }));
        let dirty = Arc::new(AtomicBool::new(false));

        let thread_emu = emu.clone();
        let thread_dirty = dirty.clone();
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
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
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let code = child.wait().ok().map(|s| s.exit_code());
                notify(id, PaneNotify::Exited(code));
            })?;

        Ok(TermPane { emu, writer, master: pair.master, killer, size: shared_size, dirty })
    }

    pub fn lock(&self) -> MutexGuard<'_, Emulator> {
        self.emu.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    pub fn paste(&self, text: &str) {
        let bracketed = self.lock().mode().contains(TermMode::BRACKETED_PASTE);
        if bracketed {
            let clean = text.replace("\x1b[201~", "");
            self.write(format!("\x1b[200~{clean}\x1b[201~").as_bytes());
        } else {
            self.write(text.replace("\r\n", "\r").replace('\n', "\r").as_bytes());
        }
    }

    pub fn size(&self) -> GridSize {
        *self.size.lock().unwrap()
    }

    pub fn resize(&self, size: GridSize) {
        if size.cols == 0 || size.rows == 0 || self.size() == size {
            return;
        }
        *self.size.lock().unwrap() = size;
        let _ = self.master.resize(PtySize { rows: size.rows, cols: size.cols, pixel_width: 0, pixel_height: 0 });
        self.lock().term.resize(size);
    }

    pub fn kill(&mut self) {
        let _ = self.killer.kill();
    }
}

impl Drop for TermPane {
    fn drop(&mut self) {
        self.kill();
    }
}
