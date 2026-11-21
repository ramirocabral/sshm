//! Turns terminal events into app actions; what each action does lives in actions.rs and hosts.rs.

mod dialog;
mod mouse;
mod pane;
mod sidebar;

use std::time::Instant;

use alacritty_terminal::index::Point;
use crossterm::event::{Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::dialogs::Dialog;
use super::keymap::{self, Action, Ctx};
use super::{App, Focus, TabKind};
use crate::term::pane::PaneId;

#[derive(Default)]
pub struct MouseState {
    pub selecting: Option<PaneId>,
    last_click: Option<(Instant, u16, u16)>,
    clicks: u8,
    anchor: Option<Point>,
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
            if let Some(form) = d.form_mut() {
                form.paste(&text);
                return;
            }
            match d {
                Dialog::Unlock(u) => if u.field == 0 { &mut u.input } else { &mut u.confirm }.insert_str(&text),
                Dialog::AskPass(a) => a.input.insert_str(&text),
                Dialog::Input(i) => i.input.insert_str(&text),
                Dialog::Palette(p) => {
                    p.input.insert_str(&text);
                    p.selected = 0;
                }
                Dialog::Help(h) => {
                    if let Some(search) = h.search.as_mut() {
                        search.insert_str(&text);
                        h.query = search.value.clone();
                        h.scroll = 0;
                    }
                }
                _ => {}
            }
            return;
        }
        if self.focus == Focus::Sidebar {
            if let Some(s) = self.sidebar.search.as_mut() {
                s.insert_str(&text);
                self.sidebar.query = s.value.clone();
                self.sidebar.selected = 0;
            }
            return;
        }
        self.paste_into_sessions(&text);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if !self.dialogs.is_empty() {
            self.dialog_key(key);
            return;
        }
        if self.leader_pending {
            self.leader_pending = false;
            self.leader_key(key);
            return;
        }
        if self.leader.matches(&key) {
            self.leader_pending = true;
            return;
        }
        if self.cfg.alt_shortcuts
            && key.modifiers == KeyModifiers::ALT
            && let Some(action) = keymap::lookup(&[Ctx::Alt], &key, &self.leader)
            && (self.focus == Focus::Main || !matches!(action, Action::FocusPane(_)))
        {
            self.run_action(action);
            return;
        }
        match self.focus {
            Focus::Sidebar => self.sidebar_key(key),
            Focus::Main => match self.tabs.get(self.active).map(|t| &t.kind) {
                Some(TabKind::Sftp(_)) => self.sftp_key(key),
                Some(TabKind::Tunnels(_)) => self.tunnels_key(key),
                Some(TabKind::Terms(_)) => self.pane_key(key),
                None => self.focus = Focus::Sidebar,
            },
        }
    }

    fn leader_key(&mut self, key: KeyEvent) {
        match keymap::lookup(&[Ctx::Leader], &key, &self.leader) {
            Some(action) => self.run_action(action),
            None if key.code == KeyCode::Esc => {}
            None => self.info(format!("{} {}: no action (leader+? shows help)", self.leader.label(), key_label(&key))),
        }
    }
}

fn key_label(key: &KeyEvent) -> String {
    match key.code {
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::super::dialogs::Dialog;
    use super::super::sidebar::Item;
    use super::super::tests::{app_with_hosts, key};
    use super::super::{Focus, keymap::Action};

    #[test]
    fn host_list_keys_run_actions() {
        let (mut app, _dir) = app_with_hosts(&[("web1", "acme/prod"), ("db1", "acme/prod"), ("lab", "")]);
        // acme ▾, acme/prod ▸, lab
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('l')));
        assert!(app.sidebar.expanded.contains("acme/prod"));
        app.handle_key(key(KeyCode::Char('G')));
        assert!(matches!(app.sidebar.items(&app.store).get(app.sidebar.selected), Some(Item::Host { .. })));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.sidebar.selected, 0);

        // acme ▾, acme/prod ▾, db1, web1, lab
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Down));
        app.handle_key(key(KeyCode::Char('v')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.dialogs.is_empty(), "n does nothing in visual mode");
        app.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(app.sidebar.visual, None);
        assert_eq!(app.sidebar.marked.len(), 2);
        app.handle_key(key(KeyCode::Esc));
        assert!(app.sidebar.marked.is_empty());

        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(key(KeyCode::Char('d')));
        assert!(app.dialogs.is_empty(), "d on a folder deletes nothing");
        app.handle_key(key(KeyCode::Char('G')));
        app.handle_key(key(KeyCode::Char('d')));
        assert!(matches!(app.dialogs.front(), Some(Dialog::Confirm(_))));
        app.handle_key(key(KeyCode::Char('n')));
        assert!(app.dialogs.is_empty());
    }

    #[test]
    fn leader_and_palette() {
        let (mut app, _dir) = app_with_hosts(&[("web1", "")]);
        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(app.leader_pending);
        app.handle_key(key(KeyCode::Char('\\')));
        assert!(!app.sidebar_visible);
        app.run_action(Action::ToggleSidebar);
        assert!(app.sidebar_visible);

        app.handle_key(key(KeyCode::Char(':')));
        let Some(Dialog::Palette(p)) = app.dialogs.front() else { panic!("palette expected") };
        assert_eq!(p.matches().len(), super::super::keymap::PALETTE.len());
        for c in "locations".chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
        let Some(Dialog::Palette(p)) = app.dialogs.front() else { panic!("palette expected") };
        assert_eq!(p.matches(), [Action::Paths]);
        app.handle_key(key(KeyCode::Enter));
        assert!(matches!(app.dialogs.front(), Some(Dialog::Message { .. })));
        app.handle_key(key(KeyCode::Esc));
        assert!(app.dialogs.is_empty());

        app.handle_key(key(KeyCode::Char('/')));
        app.handle_input(crossterm::event::Event::Paste("web".into()));
        assert_eq!((app.sidebar.query.as_str(), app.sidebar.selected), ("web", 0));
        assert_eq!(app.focus, Focus::Sidebar);
    }

    #[test]
    fn forms_share_one_key_path() {
        let (mut app, _dir) = app_with_hosts(&[("web1", "acme")]);
        app.handle_key(key(KeyCode::Char('G')));
        app.handle_key(key(KeyCode::Char('e')));
        let Some(Dialog::HostForm(f)) = app.dialogs.front() else { panic!("host form expected") };
        assert!(f.editing);
        for k in [KeyCode::Char('g'), KeyCode::Char('g'), KeyCode::Char('S')] {
            app.handle_key(key(k));
        }
        app.handle_input(crossterm::event::Event::Paste("renamed".into()));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Char('w')));
        assert!(app.dialogs.is_empty());
        assert_eq!(app.store.hosts[0].name, "renamed");

        app.handle_key(key(KeyCode::Char('x')));
        assert!(matches!(app.dialogs.front(), Some(Dialog::Export(_))));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.dialogs.is_empty());
    }
}
