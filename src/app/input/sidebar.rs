use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::super::keymap::{self, Ctx};
use super::super::sidebar::Item;
use super::super::App;

impl App {
    pub(super) fn sidebar_key(&mut self, key: KeyEvent) {
        if self.sidebar.search.is_some() {
            return self.search_key(key);
        }
        let ctx = if self.sidebar.visual.is_some() { Ctx::Visual } else { Ctx::Hosts };
        if let Some(action) = keymap::lookup(&[Ctx::Motion, ctx], &key, &self.leader) {
            self.run_action(action);
        }
    }

    /// Typing a search query; the list filters as it changes.
    fn search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.sidebar.search = None;
                self.sidebar.query.clear();
            }
            KeyCode::Enter => {
                self.sidebar.search = None;
                if !self.sidebar.query.is_empty() {
                    self.sidebar_activate(false);
                }
            }
            KeyCode::Up => self.sidebar_move(-1),
            KeyCode::Down => self.sidebar_move(1),
            KeyCode::Char('j') | KeyCode::Char('n') if ctrl => self.sidebar_move(1),
            KeyCode::Char('k') | KeyCode::Char('p') if ctrl => self.sidebar_move(-1),
            _ => {
                if let Some(search) = self.sidebar.search.as_mut()
                    && search.handle_key(&key)
                {
                    self.sidebar.query = search.value.clone();
                    self.sidebar.selected = 0;
                }
            }
        }
    }

    pub(crate) fn sidebar_move(&mut self, delta: isize) {
        let len = self.sidebar.items(&self.store).len();
        if len == 0 {
            return;
        }
        let cur = self.sidebar.selected.min(len - 1) as isize;
        self.sidebar.selected = (cur + delta).clamp(0, len as isize - 1) as usize;
    }

    pub(crate) fn sidebar_expand(&mut self, open: bool) {
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
                    && !folder.is_empty()
                {
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

    /// Space: marks the host under the cursor (and moves on), every host of a folder, or the
    /// hosts of the visual selection.
    pub(crate) fn sidebar_mark(&mut self) {
        let items = self.sidebar.items(&self.store);
        if self.sidebar.visual.is_some() {
            let ids = self.sidebar.visual_hosts(&self.store, &items);
            self.sidebar.visual = None;
            self.sidebar.toggle_marks(ids);
            return;
        }
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

    /// The hosts of the visual selection (ending it), or None outside visual mode.
    pub(crate) fn take_visual_hosts(&mut self) -> Option<Vec<String>> {
        self.sidebar.visual?;
        let items = self.sidebar.items(&self.store);
        let ids = self.sidebar.visual_hosts(&self.store, &items);
        self.sidebar.visual = None;
        Some(ids)
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
}
