use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::App;
use super::super::dialogs::*;
use super::super::form::FormEvent;
use super::super::widgets::TextInput;
use crate::ssh::command::PromptKind;

impl App {
    /// Keys go to the front dialog, which is put back unless the key closes it.
    pub(super) fn dialog_key(&mut self, key: KeyEvent) {
        let Some(dialog) = self.dialogs.pop_front() else { return };
        let keep = |app: &mut App, d: Dialog| app.dialogs.push_front(d);
        match dialog {
            Dialog::Message { title, lines } => {
                if !matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(' ')) {
                    keep(self, Dialog::Message { title, lines });
                }
            }
            Dialog::Help(mut h) => {
                if self.help_key(&mut h, &key) {
                    keep(self, Dialog::Help(h));
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
                if u.busy.is_some() && key.code != KeyCode::Esc {
                    return keep(self, Dialog::Unlock(u));
                }
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
                        self.submit_unlock(u);
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
                    if a.save
                        && !value.is_empty()
                        && a.kind != PromptKind::Other
                        && let (Some(h), Some(k)) = (a.host_id.clone(), a.secret_kind)
                    {
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
                    _ if down || key.code == KeyCode::Tab => {
                        p.selected = (p.selected + 1).min(matches.len().saturating_sub(1));
                        keep(self, Dialog::Palette(p));
                    }
                    KeyCode::Esc => {}
                    KeyCode::Enter => {
                        if let Some(action) = matches.get(p.selected).copied() {
                            self.run_action(action);
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
            mut d => {
                let Some(form) = d.form_mut() else { return };
                let ev = form.handle_key(&key);
                self.form_event(d, ev, Some(&key));
            }
        }
    }

    /// Returns false when the key closes the help.
    fn help_key(&mut self, h: &mut HelpDialog, key: &KeyEvent) -> bool {
        h.scroll = h.scroll.min(self.layout.help_max_scroll);
        if let Some(search) = h.search.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    h.search = None;
                    h.query.clear();
                }
                KeyCode::Enter => h.search = None,
                _ => {
                    if search.handle_key(key) {
                        h.query = search.value.clone();
                        h.scroll = 0;
                    }
                }
            }
            return true;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('/') => h.search = Some(TextInput::new(h.query.clone())),
            KeyCode::Char('j') | KeyCode::Down => h.scroll = h.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => h.scroll = h.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => h.scroll = h.scroll.saturating_add(10),
            KeyCode::PageUp => h.scroll = h.scroll.saturating_sub(10),
            KeyCode::Char('d') | KeyCode::Char('f') if ctrl => h.scroll = h.scroll.saturating_add(10),
            KeyCode::Char('u') | KeyCode::Char('b') if ctrl => h.scroll = h.scroll.saturating_sub(10),
            KeyCode::Char('g') | KeyCode::Home => h.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => h.scroll = self.layout.help_max_scroll,
            KeyCode::Esc if !h.query.is_empty() => h.query.clear(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter => return false,
            _ => {}
        }
        true
    }

    /// What a form dialog does after a key or a click; `key` is None for the mouse.
    pub(crate) fn form_event(&mut self, mut d: Dialog, ev: FormEvent, key: Option<&KeyEvent>) {
        match ev {
            FormEvent::Cancel => {
                if let Dialog::Theme(t) = &d {
                    self.preview_theme(&t.previous);
                }
            }
            FormEvent::Submit => self.submit_form(d),
            ev => {
                if let FormEvent::Changed(id) = ev {
                    d.form_changed(id);
                    // Picking a theme from the list saves at once: it is the only field.
                    if let Dialog::Theme(t) = &d
                        && t.form.popup.is_none()
                        && key.is_some_and(|k| k.code == KeyCode::Enter)
                    {
                        return self.submit_form(d);
                    }
                }
                if let Dialog::Theme(t) = &d {
                    self.preview_theme(&t.shown());
                }
                if key.is_some()
                    && let Some(form) = d.form_mut()
                {
                    form.error = None;
                }
                self.dialogs.push_front(d);
            }
        }
    }

    fn submit_form(&mut self, d: Dialog) {
        match d {
            Dialog::HostForm(f) => self.save_host_form(f),
            Dialog::Import(f) => self.run_import(f),
            Dialog::Export(f) => self.run_export(f),
            Dialog::Move(f) => self.run_move(f),
            Dialog::Theme(f) => self.save_theme(f),
            other => self.dialogs.push_front(other),
        }
    }

    fn confirm_yes(&mut self, action: ConfirmAction) {
        match action {
            ConfirmAction::HostKey(reply) => {
                let _ = reply.send(Some("yes".into()));
            }
            ConfirmAction::DeleteHosts(ids) => self.delete_hosts(ids),
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
}
