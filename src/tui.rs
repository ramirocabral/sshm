use std::io::{Stdout, stdout};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

static ENHANCED: AtomicBool = AtomicBool::new(false);

pub fn enter() -> Result<Tui> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        ENHANCED.store(true, Ordering::Relaxed);
    }
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave();
        prev(info);
    }));
    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    term.clear()?;
    Ok(term)
}

pub fn leave() -> Result<()> {
    let mut out = stdout();
    if ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste, DisableMouseCapture, DisableFocusChange, LeaveAlternateScreen);
    let _ = execute!(out, crossterm::cursor::Show);
    disable_raw_mode()?;
    Ok(())
}
