//! Host, folder and theme operations behind the host list and its forms.

use super::App;
use super::dialogs::*;
use super::form::Choice;
use super::sidebar::Item;
use crate::secrets::SecretKind;
use crate::store::hosts::{Host, SecretSource, move_target, normalize_folder, parent_folder, rename_target};

impl App {
    pub fn open_host_form(&mut self, id: Option<String>) {
        let (host, editing) = match id.as_ref().and_then(|i| self.store.get(i)) {
            Some(h) => (h.clone(), true),
            None => {
                let folder = match self.sidebar.items(&self.store).get(self.sidebar.selected) {
                    Some(Item::Folder { path, .. }) => path.clone(),
                    Some(item @ Item::Host { .. }) => item.host(&self.store).map(|h| h.folder.clone()).unwrap_or_default(),
                    None => String::new(),
                };
                // New hosts keep their passwords in the system keyring when there is one.
                let source = if crate::secrets::keyring::available() { SecretSource::Keyring } else { SecretSource::Vault };
                (Host { folder, password: source.clone(), root_password: source, ..Host::default() }, false)
            }
        };
        let (saved_password, saved_root) = if self.vault.is_unlocked() {
            (self.vault.get(&host.id, SecretKind::Password).is_some(), self.vault.get(&host.id, SecretKind::RootPassword).is_some())
        } else {
            (false, false)
        };
        let jump_hosts = self
            .store
            .hosts
            .iter()
            .filter(|h| h.id != host.id)
            .map(|h| Choice::new(h.display_target(), h.path(), h.display_target()))
            .collect();
        let vault_locked = self.vault.exists() && !self.vault.is_unlocked();
        let ctx = HostFormContext { folders: self.store.folders(), jump_hosts, keys: ssh_keys(), saved_password, saved_root, vault_locked };
        self.push_dialog(Dialog::HostForm(Box::new(HostForm::new(&host, editing, ctx))));
    }

    pub(crate) fn save_host_form(&mut self, mut form: Box<HostForm>) {
        match form.to_host() {
            Ok(host) => {
                let changes = form.secret_changes();
                let id = host.id.clone();
                let old = self.store.get(&id).cloned();
                self.store.upsert(host.clone());
                self.persist_hosts();
                self.sidebar.select_host(&self.store, &id);
                self.info(if form.editing { "Host updated" } else { "Host created" });
                let typed: Vec<SecretKind> = changes.iter().map(|(k, _)| *k).collect();
                self.save_secrets(&id, changes);
                if let Some(old) = old {
                    self.retire_old_secrets(&old, &host, &typed);
                }
            }
            Err(e) => {
                form.form.error = Some(e);
                self.dialogs.push_front(Dialog::HostForm(form));
            }
        }
    }

    /// A password whose source changed moves along when it went between the vault and the keyring
    /// and nothing new was typed; otherwise the old copy is deleted.
    fn retire_old_secrets(&mut self, old: &Host, new: &Host, typed: &[SecretKind]) {
        let mut vault_changed = false;
        for kind in [SecretKind::Password, SecretKind::RootPassword] {
            let source = |h: &Host| if kind == SecretKind::Password { h.password.clone() } else { h.root_password.clone() };
            let (from, to) = (source(old), source(new));
            if from == to {
                continue;
            }
            let carry = !typed.contains(&kind);
            match from {
                SecretSource::Vault if self.vault.is_unlocked() => {
                    let Some(value) = self.vault.get(&old.id, kind) else { continue };
                    if !(carry && to == SecretSource::Keyring) {
                        self.vault.set(&old.id, kind, None);
                        vault_changed = true;
                        continue;
                    }
                    // The vault copy goes only once the keyring has it.
                    let (id, label) = (new.id.clone(), self.keyring_label(&new.id, kind));
                    let store = {
                        let id = id.clone();
                        move || crate::secrets::keyring::set(&id, kind, &label, &value)
                    };
                    self.spawn_job(store, move |app, result| match result {
                        Ok(()) if app.vault.is_unlocked() => {
                            app.vault.set(&id, kind, None);
                            app.save_vault(Some("Password moved to the keyring"));
                        }
                        Ok(()) => app.info("Password copied to the keyring"),
                        Err(e) => app.error(format!("{e:#}")),
                    });
                }
                SecretSource::Keyring => {
                    let id = old.id.clone();
                    let into_vault = carry && to == SecretSource::Vault;
                    let fetch = move || if into_vault { crate::secrets::keyring::get(&id, kind) } else { Ok(None) };
                    let id = old.id.clone();
                    self.spawn_job(fetch, move |app, value| match value {
                        Ok(Some(v)) if app.vault.is_unlocked() => {
                            app.save_secrets(&id, vec![(kind, Some(v))]);
                            app.save_to_keyring(&id, vec![(kind, None)]);
                        }
                        // The vault is locked: it asks to unlock, and the keyring keeps its copy meanwhile.
                        Ok(Some(v)) => app.save_secrets(&id, vec![(kind, Some(v))]),
                        Ok(None) => app.save_to_keyring(&id, vec![(kind, None)]),
                        Err(e) => app.error(format!("{e:#}")),
                    });
                }
                _ => {}
            }
        }
        if vault_changed {
            self.save_vault(None);
        }
    }

    pub fn persist_hosts(&mut self) {
        if let Err(e) = self.store.save() {
            self.error(format!("Could not save hosts.toml: {e:#}"));
        }
    }

    /// `c`: a copy of the host under the cursor, with a new id.
    pub(crate) fn copy_host(&mut self) {
        let Some(h) = self.selected_host().and_then(|id| self.store.get(&id).cloned()) else { return };
        let copy = Host { id: crate::store::hosts::new_id(), name: format!("{} (copy)", h.name), ..h };
        let id = copy.id.clone();
        self.store.upsert(copy);
        self.persist_hosts();
        self.sidebar.select_host(&self.store, &id);
    }

    pub(crate) fn confirm_delete_hosts(&mut self, ids: Vec<String>) {
        let hosts: Vec<&Host> = ids.iter().filter_map(|id| self.store.get(id)).collect();
        let (title, body) = match hosts.as_slice() {
            [] => return self.info("No hosts in the selection"),
            [h] => ("Delete host", format!("Delete {} ({})? Its passwords are removed from the vault too.", h.path(), h.display_target())),
            hs => {
                let names: Vec<String> = hs.iter().take(5).map(|h| h.path()).collect();
                let more = if hs.len() > 5 { ", …" } else { "" };
                let body =
                    format!("Delete {} hosts ({}{more})? Their passwords are removed from the vault too.", hs.len(), names.join(", "));
                ("Delete hosts", body)
            }
        };
        let ids = hosts.iter().map(|h| h.id.clone()).collect();
        self.push_dialog(Dialog::Confirm(ConfirmDialog { title: title.into(), body, action: Some(ConfirmAction::DeleteHosts(ids)) }));
    }

    pub(crate) fn delete_hosts(&mut self, ids: Vec<String>) {
        let mut removed = Vec::new();
        for id in &ids {
            removed.extend(self.store.remove(id));
            self.sidebar.marked.remove(id);
        }
        self.forget_keyring_secrets(removed);
        self.persist_hosts();
        if self.vault.is_unlocked() {
            ids.iter().for_each(|id| self.vault.remove(id));
            self.save_vault(None);
        }
        self.info(if ids.len() == 1 { "Host deleted".to_string() } else { format!("{} hosts deleted", ids.len()) });
    }

    /// `m`: moves the marked hosts, or else the folder or host under the cursor.
    pub(crate) fn open_move(&mut self) {
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

    pub(crate) fn open_move_hosts(&mut self, ids: Vec<String>) {
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

    pub(crate) fn run_move(&mut self, mut d: MoveForm) {
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

    pub(crate) fn rename_folder(&mut self, from: &str, input: &str) {
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

    pub(crate) fn preview_theme(&mut self, name: &str) {
        if let Some(theme) = crate::ui::theme::find(name) {
            crate::ui::theme::apply(theme, self.truecolor);
            self.dirty = true;
        }
    }

    pub(crate) fn save_theme(&mut self, d: ThemeForm) {
        let name = d.form.value("theme");
        self.preview_theme(&name);
        self.cfg.theme = name.clone();
        match crate::store::config::Config::save_theme(&name) {
            Ok(()) => self.info(format!("Theme: {name}")),
            Err(e) => self.error(format!("Theme applied, but could not save config.toml: {e:#}")),
        }
    }
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
