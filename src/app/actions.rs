//! What each `Action` does, plus the tab and pane operations behind them.

use crossterm::event::KeyEvent;

use super::dialogs::*;
use super::keymap::{Action, Dir};
use super::sidebar::Item;
use super::widgets::TextInput;
use super::{App, Focus, TabKind};
use crate::secrets::SecretKind;
use crate::term::keys;
use crate::term::pane::PaneId;

impl App {
    pub fn run_action(&mut self, action: Action) {
        match action {
            Action::SendLeader => self.send_leader(),
            Action::ToggleFocus => {
                self.sidebar_visible = true;
                self.focus = if self.focus == Focus::Sidebar && !self.tabs.is_empty() { Focus::Main } else { Focus::Sidebar };
            }
            Action::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                if !self.sidebar_visible && !self.tabs.is_empty() {
                    self.focus = Focus::Main;
                }
            }
            Action::LocalShell => self.open_tab(&[None]),
            Action::Sftp => match self.context_host() {
                Some(h) => self.open_sftp(&h),
                None => self.error("Pick a host to open SFTP"),
            },
            Action::Duplicate => {
                if let Some(p) = self.focused_pane() {
                    let host = p.host_id.clone();
                    self.open_in_current(host);
                }
            }
            Action::ToggleBroadcast => self.toggle_broadcast(),
            Action::ToggleExclude => {
                if let Some(id) = self.focused_pane_id()
                    && let Some(p) = self.panes.get_mut(&id)
                {
                    p.excluded = !p.excluded;
                    let msg = if p.excluded { "Pane excluded from broadcast" } else { "Pane included in broadcast" };
                    self.info(msg);
                }
            }
            Action::TypePassword => self.type_secret(SecretKind::Password),
            Action::TypeRootPassword => self.type_secret(SecretKind::RootPassword),
            Action::CopyMode => {
                if let Some(id) = self.focused_pane_id() {
                    self.enter_copy_mode(id);
                }
            }
            Action::Reconnect => {
                if let Some(id) = self.focused_pane_id() {
                    self.reconnect(id);
                }
            }
            Action::Close => self.close_current(),
            Action::CloseTab => self.remove_tab(self.active),
            Action::Zoom => {
                if let Some(t) = self.active_term_tab_mut() {
                    t.zoomed = !t.zoomed;
                }
            }
            Action::SelectTab(i) => self.select_tab(i),
            Action::FocusPane(dir) => self.move_pane_focus(dir),
            Action::NextPane => self.cycle_pane(1),
            Action::NextTab => {
                if !self.tabs.is_empty() {
                    self.select_tab((self.active + 1) % self.tabs.len());
                }
            }
            Action::PrevTab => {
                if !self.tabs.is_empty() {
                    self.select_tab((self.active + self.tabs.len() - 1) % self.tabs.len());
                }
            }
            Action::RenameTab => {
                if let Some(tab) = self.tabs.get(self.active) {
                    let (id, current) = (tab.id, tab.title.clone().unwrap_or_default());
                    self.push_dialog(Dialog::Input(InputDialog {
                        title: "Tab name (empty = automatic)".into(),
                        input: TextInput::new(current),
                        action: Some(InputAction::RenameTab { tab: id }),
                    }));
                }
            }
            Action::NewHost => self.open_host_form(None),
            Action::Edit => self.edit_current(),
            Action::CopyHost => self.copy_host(),
            Action::DeleteHosts => match self.take_visual_hosts() {
                Some(ids) => self.confirm_delete_hosts(ids),
                None => {
                    if let Some(id) = self.selected_host() {
                        self.confirm_delete_hosts(vec![id]);
                    }
                }
            },
            Action::MoveHosts => match self.take_visual_hosts() {
                Some(ids) => self.open_move_hosts(ids),
                None => self.open_move(),
            },
            Action::Mark => self.sidebar_mark(),
            Action::Visual => {
                if !self.sidebar.items(&self.store).is_empty() {
                    self.sidebar.visual = Some(self.sidebar.selected);
                }
            }
            Action::ExitVisual => self.sidebar.visual = None,
            Action::Search => self.sidebar.search = Some(TextInput::new(self.sidebar.query.clone())),
            Action::Expand => self.sidebar_expand(true),
            Action::Collapse => self.sidebar_expand(false),
            Action::Activate => self.sidebar_activate(false),
            Action::ActivateHere => self.sidebar_activate(true),
            Action::CopyTarget => {
                if let Some(h) = self.selected_host().and_then(|id| self.store.get(&id).cloned()) {
                    let target = h.display_target();
                    crate::clipboard::copy(&target, self.cfg.clipboard_command.as_deref());
                    self.info(format!("Copied: {target}"));
                }
            }
            Action::Down => self.sidebar_move(1),
            Action::Up => self.sidebar_move(-1),
            Action::PageDown => self.sidebar_move(10),
            Action::PageUp => self.sidebar_move(-10),
            Action::Top => self.sidebar.selected = 0,
            Action::Bottom => self.sidebar.selected = self.sidebar.items(&self.store).len().saturating_sub(1),
            Action::ClearOrBack => {
                if !self.sidebar.marked.is_empty() {
                    self.sidebar.marked.clear();
                } else if !self.tabs.is_empty() {
                    self.focus = Focus::Main;
                }
            }
            Action::Import => self.push_dialog(Dialog::Import(ImportForm::new())),
            Action::Export => self.push_dialog(Dialog::Export(ExportForm::new())),
            Action::ImportSshConfig => self.run_import(ImportForm::new()),
            Action::LockVault => {
                self.vault.lock();
                self.info("Vault locked");
            }
            Action::ChangeMaster => {
                if self.vault.is_unlocked() {
                    self.run_after_unlock(AfterUnlock::ChangeMaster);
                } else {
                    let creating = !self.vault.exists();
                    self.push_dialog(Dialog::Unlock(UnlockDialog::new(creating, AfterUnlock::ChangeMaster)));
                }
            }
            Action::Theme => self.push_dialog(Dialog::Theme(ThemeForm::new(&self.cfg.theme))),
            Action::Help => self.push_dialog(Dialog::Help(HelpDialog::default())),
            Action::Palette => self.push_dialog(Dialog::Palette(PaletteDialog::new())),
            Action::Paths => self.show_paths(),
            Action::Quit => self.request_quit(),
        }
    }

    /// The leader twice sends the leader key itself to the focused session.
    fn send_leader(&mut self) {
        let Some(id) = self.focused_pane_id().filter(|_| self.focus == Focus::Main) else { return };
        let Some(p) = self.panes.get(&id) else { return };
        let mode = p.term.lock().mode();
        if let Some(b) = keys::encode(&KeyEvent::new(self.leader.code, self.leader.mods), mode) {
            p.term.write(&b);
        }
    }

    pub fn select_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
            self.focus = Focus::Main;
            if let Some(id) = self.focused_pane_id()
                && let Some(p) = self.panes.get_mut(&id)
            {
                p.bell = false;
            }
        }
    }

    fn move_pane_focus(&mut self, dir: Dir) {
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
                    Dir::Left if o.right() <= r.x => (r.x - o.right(), overlap(r.y, r.bottom(), o.y, o.bottom())),
                    Dir::Right if o.x >= r.right() => (o.x - r.right(), overlap(r.y, r.bottom(), o.y, o.bottom())),
                    Dir::Up if o.bottom() <= r.y => (r.y - o.bottom(), overlap(r.x, r.right(), o.x, o.right())),
                    Dir::Down if o.y >= r.bottom() => (o.y - r.bottom(), overlap(r.x, r.right(), o.x, o.right())),
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
                && let Some(pos) = t.panes.iter().position(|p| *p == id)
            {
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
    pub(crate) fn context_host(&mut self) -> Option<String> {
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

    /// `e`: the host of the focused session, or in the host list the host (edit) or folder (rename).
    fn edit_current(&mut self) {
        if self.focus == Focus::Main {
            if let Some(h) = self.context_host() {
                self.open_host_form(Some(h));
            }
            return;
        }
        match self.sidebar.items(&self.store).get(self.sidebar.selected).cloned() {
            Some(Item::Host { id, .. }) => self.open_host_form(Some(id)),
            Some(Item::Folder { path, name, .. }) => self.push_dialog(Dialog::Input(InputDialog {
                title: format!("Rename folder {path}"),
                input: TextInput::new(name),
                action: Some(InputAction::RenameFolder { from: path }),
            })),
            None => {}
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

    pub(crate) fn close_current(&mut self) {
        match self.tabs.get(self.active).map(|t| &t.kind) {
            Some(TabKind::Sftp(_)) => self.remove_tab(self.active),
            Some(TabKind::Terms(_)) => {
                let Some(id) = self.focused_pane_id() else { return };
                let Some(p) = self.panes.get(&id) else { return };
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

    fn show_paths(&mut self) {
        let lines = vec![
            format!("Config:  {}", crate::store::config::Config::path().display()),
            format!("Hosts:   {}", crate::store::hosts::HostStore::default_path().display()),
            format!("Vault:   {}", self.vault.path().display()),
            format!("Log:     {}", crate::paths::state_dir().join("sshm.log").display()),
            format!("Sockets: {}", self.runtime_dir.display()),
        ];
        self.push_dialog(Dialog::Message { title: "Locations".into(), lines });
    }
}

#[cfg(test)]
mod tests {
    use super::super::Focus;
    use super::super::dialogs::Dialog;
    use super::super::keymap::Action;
    use super::super::tests::app_with_hosts;

    #[test]
    fn actions_without_sessions() {
        let (mut app, _dir) = app_with_hosts(&[("web1", "acme"), ("db1", "")]);
        app.run_action(Action::Quit);
        assert!(app.quit);
        app.quit = false;

        app.run_action(Action::Sftp);
        assert!(app.tabs.is_empty(), "no host under the cursor (the acme folder)");
        app.run_action(Action::Edit);
        assert!(matches!(app.dialogs.pop_front(), Some(Dialog::Input(_))), "e on a folder renames it");

        app.run_action(Action::Bottom);
        app.run_action(Action::Edit);
        assert!(matches!(app.dialogs.pop_front(), Some(Dialog::HostForm(_))));
        app.run_action(Action::CopyHost);
        assert_eq!(app.store.hosts.len(), 3);
        assert!(app.store.hosts.iter().any(|h| h.name == "db1 (copy)"));

        app.run_action(Action::ToggleFocus);
        assert_eq!(app.focus, Focus::Sidebar, "nothing to focus without tabs");
        app.run_action(Action::SelectTab(3));
        assert_eq!(app.focus, Focus::Sidebar);
        app.run_action(Action::Help);
        app.run_action(Action::Palette);
        assert_eq!(app.dialogs.len(), 2);
    }
}
