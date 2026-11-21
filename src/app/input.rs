use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::dialogs::*;
use super::form::{Choice, Form, FormEvent};
use super::sidebar::Item;
use super::widgets::TextInput;
use super::{App, Focus, TabKind};
use crate::secrets::SecretKind;
use crate::ssh::command::PromptKind;
use crate::store::hosts::{Host, SecretSource, move_target, normalize_folder, parent_folder, rename_target};
use crate::term::pane::PaneId;
use crate::term::{keys, mouse};

#[derive(Default)]
pub struct MouseState {
    pub selecting: Option<PaneId>,
    last_click: Option<(Instant, u16, u16)>,
    clicks: u8,
    anchor: Option<Point>,
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    r.contains(Position::new(x, y))
}

impl App {
    pub fn handle_input(&mut self, ev: CEvent) {
        match ev {
            CEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return;
                }
                self.handle_key(key);
            }
            CEvent::Paste(text) => self.handle_paste(text),
            CEvent::Mouse(m) => self.handle_mouse(m),
            _ => {}
        }
    }

    fn handle_paste(&mut self, text: String) {
        if let Some(d) = self.dialogs.front_mut() {
            let input = match d {
                Dialog::Unlock(u) => Some(if u.field == 0 { &mut u.input } else { &mut u.confirm }),
                Dialog::AskPass(a) => Some(&mut a.input),
                Dialog::Input(i) => Some(&mut i.input),
                Dialog::Palette(p) => Some(&mut p.input),
                Dialog::HostForm(f) => {
                    f.form.paste(&text);
                    None
                }
                Dialog::Import(f) => {
                    f.form.paste(&text);
                    None
                }
                Dialog::Export(f) => {
                    f.form.paste(&text);
                    None
                }
                Dialog::Move(f) => {
                    f.form.paste(&text);
                    None
                }
                Dialog::Theme(f) => {
                    f.form.paste(&text);
                    None
                }
                Dialog::Help(h) => h.search.as_mut(),
                _ => None,
            };
            if let Some(i) = input {
                i.insert_str(&text);
            }
            return;
        }
        if self.focus == Focus::Sidebar {
            if let Some(s) = self.sidebar.search.as_mut() {
                s.insert_str(&text);
                self.sidebar.query = s.value.clone();
            }
            return;
        }
        for id in self.input_targets() {
            if let Some(p) = self.panes.get(&id) {
                p.term.lock().scroll_to_bottom();
                p.term.paste(&text);
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !self.dialogs.is_empty() {
            self.dialog_key(key);
            return;
        }
        if self.leader_pending {
            self.leader_pending = false;
            self.leader_action(key);
            return;
        }
        if self.leader.matches(&key) {
            self.leader_pending = true;
            return;
        }
        if self.cfg.alt_shortcuts && key.modifiers == KeyModifiers::ALT {
            match key.code {
                KeyCode::Char(c @ '1'..='9') => {
                    self.select_tab(c as usize - '1' as usize);
                    return;
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down if self.focus == Focus::Main => {
                    self.move_pane_focus(key.code);
                    return;
                }
                _ => {}
            }
        }
        match self.focus {
            Focus::Sidebar => self.sidebar_key(key),
            Focus::Main => match self.tabs.get(self.active).map(|t| matches!(t.kind, TabKind::Sftp(_))) {
                Some(true) => self.sftp_key(key),
                Some(false) => self.pane_key(key),
                None => self.focus = Focus::Sidebar,
            },
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
            self.focus = Focus::Main;
            if let Some(id) = self.focused_pane_id()
                && let Some(p) = self.panes.get_mut(&id) {
                    p.bell = false;
                }
        }
    }

    fn move_pane_focus(&mut self, dir: KeyCode) {
        let Some(cur) = self.focused_pane_id() else { return };
        let Some(&(_, r)) = self.layout.panes.iter().find(|(id, _)| *id == cur) else { return };
        let overlap = |a0: u16, a1: u16, b0: u16, b1: u16| a0 < b1 && b0 < a1;
        let best = self
            .layout
            .panes
            .iter()
            .filter(|(id, _)| *id != cur)
            .filter_map(|(id, o)| {
                let (gap, aligned) = match dir {
                    KeyCode::Left if o.right() <= r.x => (r.x - o.right(), overlap(r.y, r.bottom(), o.y, o.bottom())),
                    KeyCode::Right if o.x >= r.right() => (o.x - r.right(), overlap(r.y, r.bottom(), o.y, o.bottom())),
                    KeyCode::Up if o.bottom() <= r.y => (r.y - o.bottom(), overlap(r.x, r.right(), o.x, o.right())),
                    KeyCode::Down if o.y >= r.bottom() => (o.y - r.bottom(), overlap(r.x, r.right(), o.x, o.right())),
                    _ => return None,
                };
                Some(((!aligned, gap), *id))
            })
            .min_by_key(|(k, _)| *k)
            .map(|(_, id)| id);
        if let Some(target) = best {
            self.focus_pane(target);
        }
    }

    pub fn focus_pane(&mut self, id: PaneId) {
        if let Some(tab_idx) = self.tab_of_pane(id) {
            self.active = tab_idx;
            if let TabKind::Terms(t) = &mut self.tabs[tab_idx].kind
                && let Some(pos) = t.panes.iter().position(|p| *p == id) {
                    t.focused = pos;
                }
            if let Some(p) = self.panes.get_mut(&id) {
                p.bell = false;
            }
            self.focus = Focus::Main;
        }
    }

    fn cycle_pane(&mut self, delta: isize) {
        if let Some(t) = self.active_term_tab_mut() {
            let n = t.panes.len() as isize;
            if n > 0 {
                t.focused = ((t.focused as isize + delta).rem_euclid(n)) as usize;
            }
        }
    }

    /// Host behind the focused session, or the one selected in the sidebar.
    fn context_host(&mut self) -> Option<String> {
        if self.focus == Focus::Main {
            if let Some(h) = self.focused_pane().and_then(|p| p.host_id.clone()) {
                return Some(h);
            }
            if let Some(TabKind::Sftp(v)) = self.tabs.get(self.active).map(|t| &t.kind) {
                return Some(v.host_id.clone());
            }
        }
        self.selected_host()
    }

    pub fn selected_host(&mut self) -> Option<String> {
        let items = self.sidebar.items(&self.store);
        match items.get(self.sidebar.selected) {
            Some(Item::Host { id, .. }) => Some(id.clone()),
            _ => None,
        }
    }

    fn leader_action(&mut self, key: KeyEvent) {
        if self.leader.matches(&key) {
            if let Some(id) = self.focused_pane_id().filter(|_| self.focus == Focus::Main) {
                let mode = self.panes[&id].term.lock().mode();
                let ev = KeyEvent::new(self.leader.code, self.leader.mods);
                if let Some(b) = keys::encode(&ev, mode) {
                    self.panes[&id].term.write(&b);
                }
            }
            return;
        }
        match key.code {
            KeyCode::Esc => {}
            KeyCode::Tab => {
                self.sidebar_visible = true;
                self.focus = if self.focus == Focus::Sidebar && !self.tabs.is_empty() { Focus::Main } else { Focus::Sidebar };
            }
            KeyCode::Char('\\') => {
                self.sidebar_visible = !self.sidebar_visible;
                if !self.sidebar_visible && !self.tabs.is_empty() {
                    self.focus = Focus::Main;
                }
            }
            KeyCode::Char('n') => self.open_host_form(None),
            KeyCode::Char('e') => {
                if let Some(h) = self.context_host() {
                    self.open_host_form(Some(h));
                }
            }
            KeyCode::Char('t') => self.open_tab(&[None]),
            KeyCode::Char('s') => {
                if let Some(h) = self.context_host() {
                    self.open_sftp(&h);
                } else {
                    self.error("Pick a host to open SFTP");
                }
            }
            KeyCode::Char('d') => {
                if let Some(p) = self.focused_pane() {
                    let host = p.host_id.clone();
                    self.open_in_current(host);
                }
            }
            KeyCode::Char('b') => self.toggle_broadcast(),
            KeyCode::Char('x') => {
                if let Some(id) = self.focused_pane_id()
                    && let Some(p) = self.panes.get_mut(&id) {
                        p.excluded = !p.excluded;
                        let msg = if p.excluded { "Pane excluded from broadcast" } else { "Pane included in broadcast" };
                        self.info(msg);
                    }
            }
            KeyCode::Char('p') => self.type_secret(SecretKind::Password),
            KeyCode::Char('P') => self.type_secret(SecretKind::RootPassword),
            KeyCode::Char('[') => {
                if let Some(id) = self.focused_pane_id() {
                    self.enter_copy_mode(id);
                }
            }
            KeyCode::Char('r') => {
                if let Some(id) = self.focused_pane_id() {
                    self.reconnect(id);
                }
            }
            KeyCode::Char('w') => self.close_current(),
            KeyCode::Char('W') => self.remove_tab(self.active),
            KeyCode::Char('z') => {
                if let Some(t) = self.active_term_tab_mut() {
                    t.zoomed = !t.zoomed;
                }
            }
            KeyCode::Char(c @ '1'..='9') => self.select_tab(c as usize - '1' as usize),
            KeyCode::Char('h') | KeyCode::Left => self.move_pane_focus(KeyCode::Left),
            KeyCode::Char('l') | KeyCode::Right => self.move_pane_focus(KeyCode::Right),
            KeyCode::Char('k') | KeyCode::Up => self.move_pane_focus(KeyCode::Up),
            KeyCode::Char('j') | KeyCode::Down => self.move_pane_focus(KeyCode::Down),
            KeyCode::Char('o') => self.cycle_pane(1),
            KeyCode::Char('N') | KeyCode::Char('L') => {
                if !self.tabs.is_empty() {
                    self.select_tab((self.active + 1) % self.tabs.len());
                }
            }
            KeyCode::Char('B') | KeyCode::Char('H') => {
                if !self.tabs.is_empty() {
                    self.select_tab((self.active + self.tabs.len() - 1) % self.tabs.len());
                }
            }
            KeyCode::Char(',') => {
                if let Some(tab) = self.tabs.get(self.active) {
                    let id = tab.id;
                    let current = tab.title.clone().unwrap_or_default();
                    self.push_dialog(Dialog::Input(InputDialog {
                        title: "Tab name (empty = automatic)".into(),
                        input: TextInput::new(current),
                        action: Some(InputAction::RenameTab { tab: id }),
                    }));
                }
            }
            KeyCode::Char('i') => self.push_dialog(Dialog::Import(ImportForm::new())),
            KeyCode::Char('?') => self.push_dialog(Dialog::Help(HelpDialog::default())),
            KeyCode::Char(':') => self.push_dialog(Dialog::Palette(PaletteDialog { input: TextInput::default(), selected: 0 })),
            KeyCode::Char('q') => self.request_quit(),
            _ => self.info(format!("{} {}: no action (leader+? shows help)", self.leader.label(), key_label(&key))),
        }
    }

    pub fn toggle_broadcast(&mut self) {
        if let Some(t) = self.active_term_tab_mut() {
            t.broadcast = !t.broadcast;
            let on = t.broadcast;
            let n = t.panes.len();
            if on {
                self.info(format!("Broadcast ON: typing goes to {n} panes (leader+x excludes one)"));
            } else {
                self.info("Broadcast OFF");
            }
        } else {
            self.error("Broadcast only works in terminal tabs");
        }
    }

    fn close_current(&mut self) {
        match self.tabs.get(self.active).map(|t| &t.kind) {
            Some(TabKind::Sftp(_)) => self.remove_tab(self.active),
            Some(TabKind::Terms(_)) => {
                let Some(id) = self.focused_pane_id() else { return };
                let p = &self.panes[&id];
                if p.exited.is_some() || p.host_id.is_none() {
                    self.close_pane(id);
                } else {
                    let name = p.name.clone();
                    self.push_dialog(Dialog::Confirm(ConfirmDialog {
                        title: "Close session".into(),
                        body: format!("Close the session to {name}?"),
                        action: Some(ConfirmAction::ClosePane(id)),
                    }));
                }
            }
            None => {}
        }
    }

    pub fn request_quit(&mut self) {
        let n = self.running_sessions();
        if n == 0 {
            self.quit = true;
        } else {
            self.push_dialog(Dialog::Confirm(ConfirmDialog {
                title: "Quit".into(),
                body: format!("{n} sessions are still open. Quit anyway?"),
                action: Some(ConfirmAction::Quit),
            }));
        }
    }

    pub fn open_host_form(&mut self, id: Option<String>) {
        let (host, editing) = match id.as_ref().and_then(|i| self.store.get(i)) {
            Some(h) => (h.clone(), true),
            None => {
                let folder = match self.sidebar.items(&self.store).get(self.sidebar.selected) {
                    Some(Item::Folder { path, .. }) => path.clone(),
                    Some(Item::Host { id, .. }) => self.store.get(id).map(|h| h.folder.clone()).unwrap_or_default(),
                    None => String::new(),
                };
                // New hosts keep their passwords in the system keyring when there is one.
                let source = if crate::secrets::keyring::available() { SecretSource::Keyring } else { SecretSource::Vault };
                (Host { folder, password: source.clone(), root_password: source, ..Host::default() }, false)
            }
        };
        let (saved_password, saved_root) = if self.vault.is_unlocked() {
            (
                self.vault.get(&host.id, SecretKind::Password).is_some(),
                self.vault.get(&host.id, SecretKind::RootPassword).is_some(),
            )
        } else {
            (false, false)
        };
        let jump_hosts = self
            .store
            .hosts
            .iter()
            .filter(|h| h.id != host.id)
            .map(|h| Choice::new(jump_target(h), h.path(), h.display_target()))
            .collect();
        let vault_locked = self.vault.exists() && !self.vault.is_unlocked();
        let ctx = HostFormContext { folders: self.store.folders(), jump_hosts, keys: ssh_keys(), saved_password, saved_root, vault_locked };
        self.push_dialog(Dialog::HostForm(Box::new(HostForm::new(&host, editing, ctx))));
    }

    // ---------- sessions ----------

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

    fn pane_key(&mut self, key: KeyEvent) {
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

    fn enter_copy_mode(&mut self, id: PaneId) {
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

    // ---------- sidebar ----------

    fn sidebar_key(&mut self, key: KeyEvent) {
        if let Some(search) = self.sidebar.search.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.sidebar.search = None;
                    self.sidebar.query.clear();
                }
                KeyCode::Enter => {
                    self.sidebar.search = None;
                    if self.sidebar.query.is_empty() {
                        return;
                    }
                    self.sidebar_activate(false);
                }
                KeyCode::Up | KeyCode::Down => {
                    let d = if key.code == KeyCode::Up { -1 } else { 1 };
                    self.sidebar_move(d);
                }
                KeyCode::Char('j') | KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => self.sidebar_move(1),
                KeyCode::Char('k') | KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => self.sidebar_move(-1),
                _ => {
                    if search.handle_key(&key) {
                        self.sidebar.query = search.value.clone();
                        self.sidebar.selected = 0;
                    }
                }
            }
            return;
        }
        if self.sidebar_motion(&key) {
            return;
        }
        if self.sidebar.visual.is_some() {
            match key.code {
                KeyCode::Enter => self.sidebar_activate(false),
                KeyCode::Char('o') => self.sidebar_activate(true),
                KeyCode::Char(' ') => {
                    let items = self.sidebar.items(&self.store);
                    let ids = self.sidebar.visual_hosts(&self.store, &items);
                    self.sidebar.visual = None;
                    self.sidebar.toggle_marks(ids);
                }
                KeyCode::Char('d') | KeyCode::Delete => {
                    let items = self.sidebar.items(&self.store);
                    let ids = self.sidebar.visual_hosts(&self.store, &items);
                    self.sidebar.visual = None;
                    self.confirm_delete_hosts(ids);
                }
                KeyCode::Char('m') => {
                    let items = self.sidebar.items(&self.store);
                    let ids = self.sidebar.visual_hosts(&self.store, &items);
                    self.sidebar.visual = None;
                    self.open_move_hosts(ids);
                }
                KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('q') => self.sidebar.visual = None,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('l') | KeyCode::Right => self.sidebar_expand(true),
            KeyCode::Char('h') | KeyCode::Left => self.sidebar_expand(false),
            KeyCode::Enter => self.sidebar_activate(false),
            KeyCode::Char('o') => self.sidebar_activate(true),
            KeyCode::Char(' ') => self.sidebar_toggle_mark(),
            KeyCode::Char('v') | KeyCode::Char('V') => {
                if !self.sidebar.items(&self.store).is_empty() {
                    self.sidebar.visual = Some(self.sidebar.selected);
                }
            }
            KeyCode::Char('/') => {
                self.sidebar.search = Some(TextInput::new(self.sidebar.query.clone()));
            }
            KeyCode::Char('n') => self.open_host_form(None),
            KeyCode::Char('e') => match self.sidebar.items(&self.store).get(self.sidebar.selected).cloned() {
                Some(Item::Host { id, .. }) => self.open_host_form(Some(id)),
                Some(Item::Folder { path, name, .. }) => self.push_dialog(Dialog::Input(InputDialog {
                    title: format!("Rename folder {path}"),
                    input: TextInput::new(name),
                    action: Some(InputAction::RenameFolder { from: path }),
                })),
                None => {}
            },
            KeyCode::Char('m') => self.open_move(),
            KeyCode::Char('c') => {
                if let Some(h) = self.selected_host().and_then(|id| self.store.get(&id).cloned()) {
                    let mut copy = h.clone();
                    copy.id = crate::store::hosts::new_id();
                    copy.name = format!("{} (copy)", h.name);
                    self.store.upsert(copy.clone());
                    self.persist_hosts();
                    self.sidebar.select_host(&self.store, &copy.id);
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(id) = self.selected_host() {
                    self.confirm_delete_hosts(vec![id]);
                }
            }
            KeyCode::Char('s') => {
                if let Some(h) = self.selected_host() {
                    self.open_sftp(&h);
                }
            }
            KeyCode::Char('y') => {
                if let Some(h) = self.selected_host().and_then(|id| self.store.get(&id).cloned()) {
                    let target = h.display_target();
                    crate::clipboard::copy(&target, self.cfg.clipboard_command.as_deref());
                    self.info(format!("Copied: {target}"));
                }
            }
            KeyCode::Char('i') => self.push_dialog(Dialog::Import(ImportForm::new())),
            KeyCode::Char('x') => self.push_dialog(Dialog::Export(ExportForm::new())),
            KeyCode::Char('t') => self.open_tab(&[None]),
            KeyCode::Char('?') => self.push_dialog(Dialog::Help(HelpDialog::default())),
            KeyCode::Char(':') => self.push_dialog(Dialog::Palette(PaletteDialog { input: TextInput::default(), selected: 0 })),
            KeyCode::Char('q') => self.request_quit(),
            KeyCode::Esc | KeyCode::Tab => {
                if !self.sidebar.marked.is_empty() && key.code == KeyCode::Esc {
                    self.sidebar.marked.clear();
                } else if !self.tabs.is_empty() {
                    self.focus = Focus::Main;
                }
            }
            _ => {}
        }
    }

    /// `m`: moves the marked hosts, or else the folder or host under the cursor.
    fn open_move(&mut self) {
        if !self.sidebar.marked.is_empty() {
            let ids = self.sidebar.marked.iter().cloned().collect();
            return self.open_move_hosts(ids);
        }
        match self.sidebar.items(&self.store).get(self.sidebar.selected).cloned() {
            Some(Item::Folder { path, .. }) => {
                let current = parent_folder(&path).to_string();
                let form = MoveForm::new(MoveWhat::Folder(path.clone()), format!("Move folder {path}"), &current, self.store.folders());
                self.push_dialog(Dialog::Move(form));
            }
            Some(Item::Host { id, .. }) => self.open_move_hosts(vec![id]),
            None => {}
        }
    }

    fn open_move_hosts(&mut self, ids: Vec<String>) {
        let hosts: Vec<&Host> = self.store.hosts.iter().filter(|h| ids.contains(&h.id)).collect();
        let (title, current) = match hosts.as_slice() {
            [] => return self.info("No hosts in the selection"),
            [h] => (format!("Move {}", h.path()), h.folder.clone()),
            hs => (format!("Move {} hosts", hs.len()), hs[0].folder.clone()),
        };
        let ids = hosts.iter().map(|h| h.id.clone()).collect();
        let form = MoveForm::new(MoveWhat::Hosts(ids), title, &current, self.store.folders());
        self.push_dialog(Dialog::Move(form));
    }

    fn run_move(&mut self, mut d: MoveForm) {
        let dest = normalize_folder(&d.form.value("dest"));
        let place = if dest.is_empty() { "the top level".to_string() } else { dest.clone() };
        match &d.what {
            MoveWhat::Folder(from) => {
                let from = from.clone();
                let to = match move_target(&from, &dest) {
                    Ok(to) => to,
                    Err(e) => {
                        d.form.error = Some(e.into());
                        return self.dialogs.push_front(Dialog::Move(d));
                    }
                };
                if to == from {
                    return;
                }
                let moved = self.store.rename_folder(&from, &to);
                self.persist_hosts();
                self.sidebar.folder_renamed(&self.store, &from, &to);
                self.info(format!("Folder {from} moved to {place} ({moved} hosts)"));
            }
            MoveWhat::Hosts(ids) => {
                let moved = self.store.move_hosts(ids, &dest);
                self.persist_hosts();
                ids.iter().for_each(|id| {
                    self.sidebar.marked.remove(id);
                });
                if let Some(id) = ids.first() {
                    self.sidebar.select_host(&self.store, id);
                }
                self.info(if moved == 1 { format!("Host moved to {place}") } else { format!("{moved} hosts moved to {place}") });
            }
        }
    }

    fn confirm_delete_hosts(&mut self, ids: Vec<String>) {
        let hosts: Vec<&Host> = ids.iter().filter_map(|id| self.store.get(id)).collect();
        let (title, body) = match hosts.as_slice() {
            [] => return self.info("No hosts in the selection"),
            [h] => ("Delete host", format!("Delete {} ({})? Its passwords are removed from the vault too.", h.path(), h.display_target())),
            hs => {
                let names: Vec<String> = hs.iter().take(5).map(|h| h.path()).collect();
                let more = if hs.len() > 5 { ", …" } else { "" };
                let body = format!("Delete {} hosts ({}{more})? Their passwords are removed from the vault too.", hs.len(), names.join(", "));
                ("Delete hosts", body)
            }
        };
        let ids = hosts.iter().map(|h| h.id.clone()).collect();
        self.push_dialog(Dialog::Confirm(ConfirmDialog { title: title.into(), body, action: Some(ConfirmAction::DeleteHosts(ids)) }));
    }

    /// Cursor movement, shared by the normal and the visual mode of the host list.
    fn sidebar_motion(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.sidebar_move(1),
            KeyCode::Char('k') | KeyCode::Up => self.sidebar_move(-1),
            KeyCode::Char('d') if ctrl => self.sidebar_move(10),
            KeyCode::Char('u') if ctrl => self.sidebar_move(-10),
            KeyCode::PageDown => self.sidebar_move(10),
            KeyCode::PageUp => self.sidebar_move(-10),
            KeyCode::Char('g') | KeyCode::Home => self.sidebar.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.sidebar.selected = self.sidebar.items(&self.store).len().saturating_sub(1),
            _ => return false,
        }
        true
    }

    fn sidebar_move(&mut self, delta: isize) {
        let len = self.sidebar.items(&self.store).len();
        if len == 0 {
            return;
        }
        let cur = self.sidebar.selected.min(len - 1) as isize;
        self.sidebar.selected = (cur + delta).clamp(0, len as isize - 1) as usize;
    }

    fn sidebar_expand(&mut self, open: bool) {
        let items = self.sidebar.items(&self.store);
        let Some(item) = items.get(self.sidebar.selected).cloned() else { return };
        match item {
            Item::Folder { path, expanded, .. } => {
                if open && !expanded {
                    self.sidebar.expanded.insert(path);
                } else if !open && expanded {
                    self.sidebar.expanded.remove(&path);
                } else if !open {
                    self.select_parent(&path);
                }
            }
            Item::Host { id, .. } => {
                if !open
                    && let Some(folder) = self.store.get(&id).map(|h| h.folder.clone())
                        && !folder.is_empty() {
                            self.select_folder(&folder);
                        }
            }
        }
    }

    fn select_folder(&mut self, folder: &str) {
        let items = self.sidebar.items(&self.store);
        if let Some(idx) = items.iter().position(|i| matches!(i, Item::Folder { path, .. } if path == folder)) {
            self.sidebar.selected = idx;
        }
    }

    fn select_parent(&mut self, folder: &str) {
        if let Some((parent, _)) = folder.rsplit_once('/') {
            let parent = parent.to_string();
            self.select_folder(&parent);
        }
    }

    fn sidebar_toggle_mark(&mut self) {
        let items = self.sidebar.items(&self.store);
        match items.get(self.sidebar.selected) {
            Some(Item::Host { id, .. }) => {
                if !self.sidebar.marked.remove(id) {
                    self.sidebar.marked.insert(id.clone());
                }
                self.sidebar_move(1);
            }
            Some(Item::Folder { path, .. }) => {
                let ids = self.store.hosts_under(path).map(|h| h.id.clone()).collect();
                self.sidebar.toggle_marks(ids);
            }
            None => {}
        }
    }

    /// Enter connects: the visual selection plus the marked hosts in a grid if there are any,
    /// else the host under the cursor. `into_current` adds them to the active tab instead.
    pub fn sidebar_activate(&mut self, into_current: bool) {
        let mut ids = Vec::new();
        if self.sidebar.visual.is_some() {
            let items = self.sidebar.items(&self.store);
            ids = self.sidebar.visual_hosts(&self.store, &items);
            if ids.is_empty() && self.sidebar.marked.is_empty() {
                self.info("No hosts in the selection");
                return;
            }
            self.sidebar.visual = None;
        }
        let mut marked: Vec<String> = self.sidebar.marked.drain().filter(|id| !ids.contains(id)).collect();
        marked.sort_by_key(|id| self.store.hosts.iter().position(|h| h.id == *id).unwrap_or(usize::MAX));
        ids.extend(marked);
        if !ids.is_empty() {
            if into_current {
                for id in ids {
                    self.open_in_current(Some(id));
                }
            } else {
                let hosts: Vec<Option<String>> = ids.into_iter().map(Some).collect();
                self.open_tab(&hosts);
            }
            return;
        }
        let items = self.sidebar.items(&self.store);
        match items.get(self.sidebar.selected).cloned() {
            Some(Item::Folder { path, expanded, .. }) => {
                if expanded {
                    self.sidebar.expanded.remove(&path);
                } else {
                    self.sidebar.expanded.insert(path);
                }
            }
            Some(Item::Host { id, .. }) => {
                if into_current {
                    self.open_in_current(Some(id));
                } else {
                    self.open_tab(&[Some(id)]);
                }
            }
            None => {}
        }
    }

    pub fn persist_hosts(&mut self) {
        if let Err(e) = self.store.save() {
            self.error(format!("Could not save hosts.toml: {e:#}"));
        }
    }

    // ---------- dialogs ----------

    fn dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialogs.pop_front() else { return };
                let keep = |app: &mut App, d: Dialog| app.dialogs.push_front(d);
        match dialog {
            Dialog::Message { title, lines } => {
                if !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(' ')) {
                    keep(self, Dialog::Message { title, lines });
                }
            }
            Dialog::Help(mut h) => {
                h.scroll = h.scroll.min(self.layout.help_max_scroll);
                if let Some(search) = h.search.as_mut() {
                    match key.code {
                        KeyCode::Esc => {
                            h.search = None;
                            h.query.clear();
                        }
                        KeyCode::Enter => h.search = None,
                        _ => {
                            if search.handle_key(&key) {
                                h.query = search.value.clone();
                                h.scroll = 0;
                            }
                        }
                    }
                    keep(self, Dialog::Help(h));
                    return;
                }
                match key.code {
                    KeyCode::Char('/') => {
                        h.search = Some(TextInput::new(h.query.clone()));
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        h.scroll = h.scroll.saturating_add(1);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        h.scroll = h.scroll.saturating_sub(1);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        h.scroll = h.scroll.saturating_add(10);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::PageUp => {
                        h.scroll = h.scroll.saturating_sub(10);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('d') | KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        h.scroll = h.scroll.saturating_add(10);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('u') | KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        h.scroll = h.scroll.saturating_sub(10);
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('g') | KeyCode::Home => {
                        h.scroll = 0;
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Char('G') | KeyCode::End => {
                        h.scroll = self.layout.help_max_scroll;
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Esc if !h.query.is_empty() => {
                        h.query.clear();
                        keep(self, Dialog::Help(h));
                    }
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => {}
                    _ => keep(self, Dialog::Help(h)),
                }
            }
            Dialog::Confirm(mut c) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('s') | KeyCode::Char('Y') | KeyCode::Char('S') | KeyCode::Enter => {
                    if let Some(a) = c.action.take() {
                        self.confirm_yes(a);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    if let Some(ConfirmAction::HostKey(reply)) = c.action.take() {
                        let _ = reply.send(None);
                    }
                }
                _ => keep(self, Dialog::Confirm(c)),
            },
            Dialog::Unlock(mut u) => {
                let two_fields = u.creating || u.changing;
                match key.code {
                    KeyCode::Esc => {
                        let then = std::mem::replace(&mut u.then, AfterUnlock::Nothing);
                        self.cancel_after_unlock(then);
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down if two_fields => {
                        u.field = 1 - u.field;
                        keep(self, Dialog::Unlock(u));
                    }
                    KeyCode::Enter => {
                        if two_fields && u.field == 0 {
                            u.field = 1;
                            keep(self, Dialog::Unlock(u));
                            return;
                        }
                        if two_fields && u.input.value != u.confirm.value {
                            u.error = Some("the passwords do not match".into());
                            u.confirm.clear();
                            keep(self, Dialog::Unlock(u));
                            return;
                        }
                        let result = if u.changing {
                            self.vault.change_passphrase(&u.input.value)
                        } else {
                            self.vault.unlock(&u.input.value)
                        };
                        match result {
                            Ok(()) => {
                                self.last_vault_use = Instant::now();
                                if u.changing {
                                    self.info("Master password changed");
                                }
                                let then = std::mem::replace(&mut u.then, AfterUnlock::Nothing);
                                self.run_after_unlock(then);
                            }
                            Err(e) => {
                                u.error = Some(format!("{e:#}"));
                                u.input.clear();
                                u.confirm.clear();
                                u.field = 0;
                                keep(self, Dialog::Unlock(u));
                            }
                        }
                    }
                    _ => {
                        let input = if u.field == 0 { &mut u.input } else { &mut u.confirm };
                        input.handle_key(&key);
                        keep(self, Dialog::Unlock(u));
                    }
                }
            }
            Dialog::AskPass(mut a) => match key.code {
                KeyCode::Esc => {
                    if let Some(r) = a.reply.take() {
                        let _ = r.send(None);
                    }
                }
                KeyCode::Tab if a.secret_kind.is_some() => {
                    a.save = !a.save;
                    keep(self, Dialog::AskPass(a));
                }
                KeyCode::Enter => {
                    let value = a.input.value.clone();
                    if let Some(r) = a.reply.take() {
                        let _ = r.send(Some(value.clone()));
                    }
                    if a.save && !value.is_empty() && a.kind != PromptKind::Other
                        && let (Some(h), Some(k)) = (a.host_id.clone(), a.secret_kind) {
                            self.save_secrets(&h, vec![(k, Some(value))]);
                        }
                }
                _ => {
                    a.input.handle_key(&key);
                    keep(self, Dialog::AskPass(a));
                }
            },
            Dialog::Input(mut d) => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Some(action) = d.action.take() {
                        let value = d.input.value.trim().to_string();
                        self.input_submit(action, value);
                    }
                }
                _ => {
                    d.input.handle_key(&key);
                    keep(self, Dialog::Input(d));
                }
            },
            Dialog::Palette(mut p) => {
                let matches = p.matches();
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let up = key.code == KeyCode::Up || (ctrl && matches!(key.code, KeyCode::Char('k') | KeyCode::Char('p')));
                let down = key.code == KeyCode::Down || (ctrl && matches!(key.code, KeyCode::Char('j') | KeyCode::Char('n')));
                match key.code {
                    _ if up => {
                        p.selected = p.selected.saturating_sub(1);
                        keep(self, Dialog::Palette(p));
                    }
                    _ if down => {
                        p.selected = (p.selected + 1).min(matches.len().saturating_sub(1));
                        keep(self, Dialog::Palette(p));
                    }
                    KeyCode::Esc => {}
                    KeyCode::Tab => {
                        p.selected = (p.selected + 1).min(matches.len().saturating_sub(1));
                        keep(self, Dialog::Palette(p));
                    }
                    KeyCode::Enter => {
                        if let Some(cmd) = matches.get(p.selected).copied() {
                            self.run_command(cmd);
                        }
                    }
                    _ => {
                        if p.input.handle_key(&key) {
                            p.selected = 0;
                        }
                        keep(self, Dialog::Palette(p));
                    }
                }
            }
            Dialog::HostForm(mut d) => match d.form.handle_key(&key) {
                FormEvent::Cancel => {}
                FormEvent::Submit => self.save_host_form(d),
                ev => {
                    if let FormEvent::Changed(id) = ev {
                        d.on_change(id);
                    }
                    d.form.error = None;
                    keep(self, Dialog::HostForm(d));
                }
            },
            Dialog::Import(mut d) => match d.form.handle_key(&key) {
                FormEvent::Cancel => {}
                FormEvent::Submit => self.run_import(d),
                ev => {
                    if let FormEvent::Changed(id) = ev {
                        d.on_change(id);
                    }
                    keep(self, Dialog::Import(d));
                }
            },
            Dialog::Export(mut d) => match d.form.handle_key(&key) {
                FormEvent::Cancel => {}
                FormEvent::Submit => self.run_export(d),
                ev => {
                    if let FormEvent::Changed(id) = ev {
                        d.on_change(id);
                    }
                    keep(self, Dialog::Export(d));
                }
            },
            Dialog::Move(mut d) => match d.form.handle_key(&key) {
                FormEvent::Cancel => {}
                FormEvent::Submit => self.run_move(d),
                _ => keep(self, Dialog::Move(d)),
            },
            // Picking from the list saves at once: it is the only field.
            Dialog::Theme(mut d) => match d.form.handle_key(&key) {
                FormEvent::Cancel => self.preview_theme(&d.previous),
                FormEvent::Submit => self.save_theme(d),
                FormEvent::Changed(_) if d.form.popup.is_none() && key.code == KeyCode::Enter => self.save_theme(d),
                _ => {
                    self.preview_theme(&d.shown());
                    keep(self, Dialog::Theme(d));
                }
            },
        }
    }

    fn save_host_form(&mut self, mut form: Box<HostForm>) {
        match form.to_host() {
            Ok(host) => {
                let changes = form.secret_changes();
                let id = host.id.clone();
                self.store.upsert(host);
                self.persist_hosts();
                self.sidebar.select_host(&self.store, &id);
                self.info(if form.editing { "Host updated" } else { "Host created" });
                self.save_secrets(&id, changes);
            }
            Err(e) => {
                form.form.error = Some(e);
                self.dialogs.push_front(Dialog::HostForm(form));
            }
        }
    }

    fn confirm_yes(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::HostKey(reply) => {
                let _ = reply.send(Some("yes".into()));
            }
            ConfirmAction::DeleteHosts(ids) => {
                let mut removed = Vec::new();
                for id in &ids {
                    removed.extend(self.store.remove(id));
                    self.sidebar.marked.remove(id);
                }
                self.forget_keyring_secrets(removed);
                self.persist_hosts();
                if self.vault.is_unlocked() {
                    ids.iter().for_each(|id| self.vault.remove(id));
                    if let Err(e) = self.vault.save() {
                        self.error(format!("Could not save the vault: {e:#}"));
                    }
                }
                self.info(if ids.len() == 1 { "Host deleted".to_string() } else { format!("{} hosts deleted", ids.len()) });
            }
            ConfirmAction::ClosePane(id) => self.close_pane(id),
            ConfirmAction::Quit => self.quit = true,
            ConfirmAction::SftpDelete { tab, remote, paths } => self.sftp_delete(tab, remote, paths),
            ConfirmAction::SftpOverwrite { tab, upload, sources } => self.sftp_transfer(tab, upload, sources),
        }
    }

    fn input_submit(&mut self, action: InputAction, value: String) {
        match action {
            InputAction::SftpMkdir { tab, remote } => self.sftp_mkdir(tab, remote, value),
            InputAction::SftpRename { tab, remote, from } => self.sftp_rename(tab, remote, from, value),
            InputAction::SftpGoto { tab, remote } => self.sftp_goto(tab, remote, value),
            InputAction::RenameTab { tab } => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == tab) {
                    t.title = Some(value).filter(|v| !v.is_empty());
                }
            }
            InputAction::RenameFolder { from } => self.rename_folder(&from, &value),
        }
    }

    fn preview_theme(&mut self, name: &str) {
        if let Some(theme) = crate::ui::theme::find(name) {
            crate::ui::theme::apply(theme, self.truecolor);
            self.dirty = true;
        }
    }

    fn save_theme(&mut self, d: ThemeForm) {
        let name = d.form.value("theme");
        self.preview_theme(&name);
        self.cfg.theme = name.clone();
        match crate::store::config::Config::save_theme(&name) {
            Ok(()) => self.info(format!("Theme: {name}")),
            Err(e) => self.error(format!("Theme applied, but could not save config.toml: {e:#}")),
        }
    }

    fn rename_folder(&mut self, from: &str, input: &str) {
        let to = match rename_target(from, input) {
            Ok(to) => to,
            Err(e) => return self.error(e),
        };
        if to == from {
            return;
        }
        let moved = self.store.rename_folder(from, &to);
        self.persist_hosts();
        self.sidebar.folder_renamed(&self.store, from, &to);
        self.info(format!("Folder {from} renamed to {to} ({moved} hosts)"));
    }

    pub fn run_command(&mut self, cmd: Command) {
        match cmd {
            Command::NewHost => self.open_host_form(None),
            Command::LocalShell => self.open_tab(&[None]),
            Command::ImportSshConfig => self.run_import(ImportForm::new()),
            Command::Import => self.push_dialog(Dialog::Import(ImportForm::new())),
            Command::Export => self.push_dialog(Dialog::Export(ExportForm::new())),
            Command::LockVault => {
                self.vault.lock();
                self.info("Vault locked");
            }
            Command::ChangeMaster => {
                if self.vault.is_unlocked() {
                    self.run_after_unlock(AfterUnlock::ChangeMaster);
                } else {
                    let creating = !self.vault.exists();
                    self.push_dialog(Dialog::Unlock(UnlockDialog::new(creating, AfterUnlock::ChangeMaster)));
                }
            }
            Command::ToggleBroadcast => self.toggle_broadcast(),
            Command::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
            Command::Theme => self.push_dialog(Dialog::Theme(ThemeForm::new(&self.cfg.theme))),
            Command::Help => self.push_dialog(Dialog::Help(HelpDialog::default())),
            Command::Paths => self.push_dialog(Dialog::Message {
                title: "Locations".into(),
                lines: vec![
                    format!("Config:  {}", crate::store::config::Config::path().display()),
                    format!("Hosts:   {}", crate::store::hosts::HostStore::default_path().display()),
                    format!("Vault:   {}", self.vault.path().display()),
                    format!("Log:     {}", crate::paths::state_dir().join("sshm.log").display()),
                    format!("Sockets: {}", self.runtime_dir.display()),
                ],
            }),
            Command::Quit => self.request_quit(),
        }
    }

    // ---------- mouse ----------

    fn handle_mouse(&mut self, m: MouseEvent) {
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
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                if let Some(&(idx, _)) = self.layout.tabs.iter().find(|(_, r)| contains(*r, x, y)) {
                    self.select_tab(idx);
                }
            } else if let MouseEventKind::Down(MouseButton::Middle) = m.kind
                && let Some(&(idx, _)) = self.layout.tabs.iter().find(|(_, r)| contains(*r, x, y)) {
                    self.select_tab(idx);
                    self.close_current();
                }
            return;
        }
        if !self.layout.sftp_panels.is_empty() {
            self.sftp_mouse(m);
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
        let act = |form: &mut Form| -> FormEvent {
            match m.kind {
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
                _ => FormEvent::None,
            }
        };
        let ev = match self.dialogs.front_mut() {
            Some(Dialog::HostForm(d)) => {
                let ev = act(&mut d.form);
                if let FormEvent::Changed(id) = ev {
                    d.on_change(id);
                }
                ev
            }
            Some(Dialog::Import(d)) => {
                let ev = act(&mut d.form);
                if let FormEvent::Changed(id) = ev {
                    d.on_change(id);
                }
                ev
            }
            Some(Dialog::Export(d)) => {
                let ev = act(&mut d.form);
                if let FormEvent::Changed(id) = ev {
                    d.on_change(id);
                }
                ev
            }
            Some(Dialog::Move(d)) => act(&mut d.form),
            Some(Dialog::Theme(d)) => {
                let ev = act(&mut d.form);
                let name = d.shown();
                self.preview_theme(&name);
                ev
            }
            Some(Dialog::Help(h)) => {
                match m.kind {
                    MouseEventKind::ScrollUp => h.scroll = h.scroll.saturating_sub(3),
                    MouseEventKind::ScrollDown => h.scroll = h.scroll.saturating_add(3),
                    _ => {}
                }
                FormEvent::None
            }
            _ => FormEvent::None,
        };
        if matches!(ev, FormEvent::Submit | FormEvent::Cancel) {
            match (self.dialogs.pop_front(), ev) {
                (Some(Dialog::HostForm(d)), FormEvent::Submit) => self.save_host_form(d),
                (Some(Dialog::Import(d)), FormEvent::Submit) => self.run_import(d),
                (Some(Dialog::Export(d)), FormEvent::Submit) => self.run_export(d),
                (Some(Dialog::Move(d)), FormEvent::Submit) => self.run_move(d),
                (Some(Dialog::Theme(d)), FormEvent::Submit) => self.save_theme(d),
                (Some(Dialog::Theme(d)), FormEvent::Cancel) => self.preview_theme(&d.previous),
                _ => {}
            }
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
                let double = self
                    .mouse
                    .last_click
                    .is_some_and(|(t, _, y)| now.duration_since(t) < Duration::from_millis(400) && y == m.row);
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

fn key_label(key: &KeyEvent) -> String {
    match key.code {
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

/// ProxyJump destination for a saved host: its ssh_config alias, or user@host:port.
fn jump_target(h: &Host) -> String {
    let mut s = String::new();
    if let Some(u) = h.user.as_ref().filter(|u| !u.is_empty()) {
        s.push_str(u);
        s.push('@');
    }
    s.push_str(&h.host);
    if let Some(p) = h.port {
        s.push_str(&format!(":{p}"));
    }
    s
}

/// Private keys in ~/.ssh: files that have a matching .pub next to them.
fn ssh_keys() -> Vec<Choice> {
    let dir = crate::paths::home().join(".ssh");
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut keys: Vec<Choice> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pubkey = dir.join(format!("{name}.pub"));
            (!name.ends_with(".pub") && pubkey.exists()).then(|| {
                let kind = std::fs::read_to_string(&pubkey)
                    .ok()
                    .and_then(|t| t.split_whitespace().next().map(|k| k.trim_start_matches("ssh-").to_string()))
                    .unwrap_or_default();
                Choice::new(format!("~/.ssh/{name}"), format!("~/.ssh/{name}"), kind)
            })
        })
        .collect();
    keys.sort_by(|a, b| a.value.cmp(&b.value));
    keys
}

fn form_choices_len(form: &Form) -> usize {
    form.popup.as_ref().map(|p| form.choices(p.field).len()).unwrap_or(0)
}
