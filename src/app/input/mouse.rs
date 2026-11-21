use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::Side;
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::super::dialogs::Dialog;
use super::super::form::{Form, FormEvent};
use super::super::{App, Focus};
use crate::term::{keys, mouse};

fn contains(r: Rect, x: u16, y: u16) -> bool {
    r.contains(Position::new(x, y))
}

impl App {
    pub(super) fn handle_mouse(&mut self, m: MouseEvent) {
        if !self.dialogs.is_empty() {
            self.dialog_mouse(m);
            return;
        }
        let (x, y) = (m.column, m.row);
        if self.sidebar_visible && contains(self.layout.sidebar, x, y) && self.mouse.selecting.is_none() {
            self.sidebar_mouse(m);
            return;
        }
        if contains(self.layout.tabbar, x, y) {
            let tab = self.layout.tabs.iter().find(|(_, r)| contains(*r, x, y)).map(|(i, _)| *i);
            match (m.kind, tab) {
                (MouseEventKind::Down(MouseButton::Left), Some(idx)) => self.select_tab(idx),
                (MouseEventKind::Down(MouseButton::Middle), Some(idx)) => {
                    self.select_tab(idx);
                    self.close_current();
                }
                _ => {}
            }
            return;
        }
        if !self.layout.sftp_panels.is_empty() {
            self.sftp_mouse(m);
            return;
        }
        if matches!(self.tabs.get(self.active).map(|t| &t.kind), Some(super::super::TabKind::Tunnels(_))) {
            if matches!(m.kind, MouseEventKind::Down(_)) {
                self.focus = Focus::Main;
            }
            self.tunnels_mouse(m);
            return;
        }
        let target = match self.mouse.selecting {
            Some(id) => self.layout.panes.iter().find(|(p, _)| *p == id).copied(),
            None => self.layout.panes.iter().find(|(_, r)| contains(*r, x, y)).copied(),
        };
        let Some((id, rect)) = target else { return };
        if matches!(m.kind, MouseEventKind::Down(_)) && self.focused_pane_id() != Some(id) {
            self.focus_pane(id);
        }
        self.focus = Focus::Main;
        let col = x.clamp(rect.x, rect.right().saturating_sub(1)) - rect.x;
        let row = y.clamp(rect.y, rect.bottom().saturating_sub(1)) - rect.y;
        let Some(pane) = self.panes.get(&id) else { return };
        let shift = m.modifiers.contains(KeyModifiers::SHIFT);
        let (mode, copy_mode) = (pane.term.lock().mode(), pane.copy_mode);
        if !copy_mode && !shift && mouse::app_wants_mouse(mode) && self.mouse.selecting.is_none() {
            if let Some(bytes) = mouse::encode(&m, col, row, mode) {
                pane.term.write(&bytes);
            }
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let now = Instant::now();
                let repeat = self
                    .mouse
                    .last_click
                    .is_some_and(|(t, cx, cy)| now.duration_since(t) < Duration::from_millis(400) && cx == x && cy == y);
                self.mouse.clicks = if repeat { (self.mouse.clicks % 3) + 1 } else { 1 };
                self.mouse.last_click = Some((now, x, y));
                let ty = match self.mouse.clicks {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
                };
                let mut emu = pane.term.lock();
                let pt = emu.viewport_point(col, row);
                emu.term.selection = Some(Selection::new(ty, pt, Side::Left));
                self.mouse.anchor = Some(pt);
                self.mouse.selecting = Some(id);
            }
            MouseEventKind::Drag(MouseButton::Left) if self.mouse.selecting == Some(id) => {
                let mut emu = pane.term.lock();
                let pt = emu.viewport_point(col, row);
                let side = if self.mouse.anchor.is_some_and(|a| pt < a) { Side::Left } else { Side::Right };
                if let Some(sel) = emu.term.selection.as_mut() {
                    sel.update(pt, side);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse.selecting = None;
                let text = {
                    let mut emu = pane.term.lock();
                    let t = emu.term.selection_to_string().filter(|s| !s.trim().is_empty());
                    let single_click = self.mouse.clicks == 1 && emu.term.selection.as_ref().is_some_and(|s| s.is_empty());
                    if single_click {
                        emu.term.selection = None;
                    }
                    t
                };
                if let Some(text) = text {
                    crate::clipboard::copy(&text, self.cfg.clipboard_command.as_deref());
                    self.info(format!("Copied ({} characters)", text.chars().count()));
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = m.kind == MouseEventKind::ScrollUp;
                let mut emu = pane.term.lock();
                let alt = emu.mode().contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL);
                if alt && !copy_mode {
                    let code = if up { KeyCode::Up } else { KeyCode::Down };
                    let bytes = keys::encode(&KeyEvent::new(code, KeyModifiers::empty()), emu.mode()).unwrap_or_default();
                    drop(emu);
                    for _ in 0..3 {
                        pane.term.write(&bytes);
                    }
                } else {
                    emu.term.scroll_display(Scroll::Delta(if up { 3 } else { -3 }));
                }
            }
            _ => {}
        }
    }

    fn dialog_mouse(&mut self, m: MouseEvent) {
        let (x, y) = (m.column, m.row);
        let popup_hit = self.layout.popup_rows.iter().find(|(_, r)| contains(*r, x, y)).map(|(i, _)| *i);
        let field_hit = self.layout.form_rows.iter().find(|(_, r)| contains(*r, x, y)).map(|(i, _)| *i);
        let Some(front) = self.dialogs.front_mut() else { return };
        if let Dialog::Help(h) = front {
            match m.kind {
                MouseEventKind::ScrollUp => h.scroll = h.scroll.saturating_sub(3),
                MouseEventKind::ScrollDown => h.scroll = h.scroll.saturating_add(3),
                _ => {}
            }
            return;
        }
        let Some(form) = front.form_mut() else { return };
        let ev = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => match (popup_hit, field_hit) {
                (Some(i), _) => form.choose(i),
                (None, Some(i)) => form.click(i),
                _ => {
                    form.popup = None;
                    FormEvent::None
                }
            },
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let n = form_choices_len(form);
                if let Some(p) = form.popup.as_mut() {
                    p.selected = if m.kind == MouseEventKind::ScrollUp {
                        p.selected.saturating_sub(1)
                    } else {
                        (p.selected + 1).min(n.saturating_sub(1))
                    };
                }
                FormEvent::None
            }
            _ => return,
        };
        if let Some(d) = self.dialogs.pop_front() {
            self.form_event(d, ev, None);
        }
    }

    fn sidebar_mouse(&mut self, m: MouseEvent) {
        let list = self.layout.sidebar_list;
        match m.kind {
            MouseEventKind::ScrollUp => self.sidebar_move(-3),
            MouseEventKind::ScrollDown => self.sidebar_move(3),
            MouseEventKind::Down(MouseButton::Left) if contains(list, m.column, m.row) => {
                self.focus = Focus::Sidebar;
                let idx = self.sidebar.scroll + (m.row - list.y) as usize;
                let now = Instant::now();
                let double =
                    self.mouse.last_click.is_some_and(|(t, _, y)| now.duration_since(t) < Duration::from_millis(400) && y == m.row);
                self.mouse.last_click = Some((now, m.column, m.row));
                if idx < self.sidebar.items(&self.store).len() {
                    self.sidebar.selected = idx;
                    if double {
                        self.sidebar_activate(false);
                    }
                }
            }
            MouseEventKind::Down(_) => self.focus = Focus::Sidebar,
            _ => {}
        }
    }
}

fn form_choices_len(form: &Form) -> usize {
    form.popup.as_ref().map(|p| form.choices(p.field).len()).unwrap_or(0)
}
