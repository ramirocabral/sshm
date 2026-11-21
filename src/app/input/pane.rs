use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::App;
use crate::term::keys;
use crate::term::pane::PaneId;

impl App {
    /// Panes that receive typing: the focused one, or every included pane while broadcasting.
    fn input_targets(&self) -> Vec<PaneId> {
        let Some(t) = self.active_term_tab() else { return Vec::new() };
        let Some(&focused) = t.panes.get(t.focused) else { return Vec::new() };
        if !t.broadcast {
            return vec![focused];
        }
        t.panes
            .iter()
            .copied()
            .filter(|id| *id == focused || self.panes.get(id).is_some_and(|p| !p.excluded && p.exited.is_none() && !p.copy_mode))
            .collect()
    }

    pub(super) fn paste_into_sessions(&mut self, text: &str) {
        for id in self.input_targets() {
            if let Some(p) = self.panes.get(&id) {
                p.term.lock().scroll_to_bottom();
                p.term.paste(text);
            }
        }
    }

    pub(super) fn pane_key(&mut self, key: KeyEvent) {
        let Some(id) = self.focused_pane_id() else { return };
        let Some(p) = self.panes.get(&id) else { return };
        if p.exited.is_some() {
            match key.code {
                KeyCode::Char('r') | KeyCode::Enter => self.reconnect(id),
                KeyCode::Char('w') | KeyCode::Char('q') => self.close_pane(id),
                _ => {}
            }
            return;
        }
        if p.copy_mode {
            self.copy_key(id, key);
            return;
        }
        for target in self.input_targets() {
            let Some(tp) = self.panes.get(&target) else { continue };
            let bytes = {
                let mut emu = tp.term.lock();
                emu.scroll_to_bottom();
                keys::encode(&key, emu.mode())
            };
            if let Some(b) = bytes {
                tp.term.write(&b);
            }
        }
    }

    pub(crate) fn enter_copy_mode(&mut self, id: PaneId) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        p.copy_mode = true;
        let mut emu = p.term.lock();
        if !emu.term.mode().contains(TermMode::VI) {
            emu.term.toggle_vi_mode();
        }
        drop(emu);
        self.info("Copy mode: hjkl/wb/0$/gG move · v/V/Ctrl+v select · y copies · q exits");
    }

    fn exit_copy_mode(&mut self, id: PaneId) {
        let Some(p) = self.panes.get_mut(&id) else { return };
        p.copy_mode = false;
        let mut emu = p.term.lock();
        emu.term.selection = None;
        if emu.term.mode().contains(TermMode::VI) {
            emu.term.toggle_vi_mode();
        }
        emu.scroll_to_bottom();
    }

    fn copy_key(&mut self, id: PaneId, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut copied = None;
        let mut exit = false;
        {
            let Some(p) = self.panes.get(&id) else { return };
            let mut emu = p.term.lock();
            let term = &mut emu.term;
            let half = (term.screen_lines() / 2).max(1) as i32;
            let motion = match key.code {
                KeyCode::Char('h') | KeyCode::Left => Some(ViMotion::Left),
                KeyCode::Char('j') | KeyCode::Down => Some(ViMotion::Down),
                KeyCode::Char('k') | KeyCode::Up => Some(ViMotion::Up),
                KeyCode::Char('l') | KeyCode::Right => Some(ViMotion::Right),
                KeyCode::Char('w') => Some(ViMotion::SemanticRight),
                KeyCode::Char('b') if !ctrl => Some(ViMotion::SemanticLeft),
                KeyCode::Char('e') => Some(ViMotion::SemanticRightEnd),
                KeyCode::Char('0') | KeyCode::Home => Some(ViMotion::First),
                KeyCode::Char('$') | KeyCode::End => Some(ViMotion::Last),
                KeyCode::Char('^') => Some(ViMotion::FirstOccupied),
                KeyCode::Char('H') => Some(ViMotion::High),
                KeyCode::Char('M') => Some(ViMotion::Middle),
                KeyCode::Char('L') => Some(ViMotion::Low),
                _ => None,
            };
            if let Some(m) = motion {
                term.vi_motion(m);
                let pt = term.vi_mode_cursor.point;
                term.scroll_to_point(pt);
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => exit = true,
                    KeyCode::Char('g') => {
                        let top = term.grid().topmost_line();
                        term.vi_goto_point(Point::new(top, Column(0)));
                    }
                    KeyCode::Char('G') => {
                        let bottom = term.grid().bottommost_line();
                        term.vi_goto_point(Point::new(bottom, Column(0)));
                    }
                    KeyCode::Char('u') if ctrl => term.scroll_display(Scroll::Delta(half)),
                    KeyCode::Char('d') if ctrl => term.scroll_display(Scroll::Delta(-half)),
                    KeyCode::Char('b') if ctrl => term.scroll_display(Scroll::PageUp),
                    KeyCode::Char('f') if ctrl => term.scroll_display(Scroll::PageDown),
                    KeyCode::PageUp => term.scroll_display(Scroll::PageUp),
                    KeyCode::PageDown => term.scroll_display(Scroll::PageDown),
                    KeyCode::Char('v') | KeyCode::Char('V') => {
                        let ty = match (key.code, ctrl) {
                            (_, true) => SelectionType::Block,
                            (KeyCode::Char('V'), _) => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        if term.selection.as_ref().is_some_and(|s| s.ty == ty) {
                            term.selection = None;
                        } else {
                            let pt = term.vi_mode_cursor.point;
                            let mut sel = Selection::new(ty, pt, Side::Left);
                            sel.include_all();
                            term.selection = Some(sel);
                        }
                    }
                    KeyCode::Char('y') | KeyCode::Enter => {
                        copied = term.selection_to_string().filter(|s| !s.is_empty());
                        exit = true;
                    }
                    _ => {}
                }
            }
        }
        if let Some(text) = copied {
            crate::clipboard::copy(&text, self.cfg.clipboard_command.as_deref());
            self.info(format!("Copied ({} characters)", text.chars().count()));
        }
        if exit {
            self.exit_copy_mode(id);
        }
    }
}
