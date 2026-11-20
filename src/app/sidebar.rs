use std::collections::HashSet;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config as MatchConfig, Matcher, Utf32Str};

use super::widgets::TextInput;
use crate::store::hosts::{HostStore, parent_folder as parent_of, subpath};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Folder { path: String, name: String, depth: usize, expanded: bool, count: usize },
    Host { id: String, depth: usize },
}

#[derive(Default)]
pub struct Sidebar {
    pub expanded: HashSet<String>,
    pub selected: usize,
    pub scroll: usize,
    pub marked: HashSet<String>,
    /// Anchor row of the visual selection; the range runs from it to `selected`.
    pub visual: Option<usize>,
    pub search: Option<TextInput>,
    pub query: String,
    initialized: bool,
}

impl Sidebar {
    pub fn items(&mut self, store: &HostStore) -> Vec<Item> {
        if !self.initialized {
            self.initialized = true;
            self.expanded.extend(store.folders().into_iter().filter(|f| !f.contains('/')));
        }
        if !self.query.trim().is_empty() {
            return self.search_items(store);
        }
        let mut items = Vec::new();
        let folders = store.folders();
        self.push_level(store, &folders, "", 0, &mut items);
        items
    }

    fn push_level(&self, store: &HostStore, folders: &[String], parent: &str, depth: usize, out: &mut Vec<Item>) {
        for f in folders.iter().filter(|f| parent_of(f) == parent) {
            let expanded = self.expanded.contains(f);
            let count = store.hosts_under(f).count();
            let name = f.rsplit('/').next().unwrap_or(f).to_string();
            out.push(Item::Folder { path: f.clone(), name, depth, expanded, count });
            if expanded {
                self.push_level(store, folders, f, depth + 1, out);
            }
        }
        for h in store.hosts.iter().filter(|h| h.folder == parent) {
            out.push(Item::Host { id: h.id.clone(), depth });
        }
    }

    fn search_items(&self, store: &HostStore) -> Vec<Item> {
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(MatchConfig::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, &str)> = store
            .hosts
            .iter()
            .filter_map(|h| {
                let hay = format!("{} {} {} {}", h.path(), h.host, h.user.as_deref().unwrap_or(""), h.tags.join(" "));
                pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher).map(|s| (s, h.id.as_str()))
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().map(|(_, id)| Item::Host { id: id.to_string(), depth: 0 }).collect()
    }

    pub fn clamp(&mut self, len: usize) {
        let last = len.saturating_sub(1);
        self.selected = self.selected.min(last);
        if let Some(anchor) = self.visual.as_mut() {
            *anchor = (*anchor).min(last);
        }
    }

    pub fn visual_range(&self) -> Option<(usize, usize)> {
        self.visual.map(|a| (a.min(self.selected), a.max(self.selected)))
    }

    /// Hosts covered by the visual selection, in list order. A collapsed folder stands for all
    /// its hosts; an expanded one adds nothing, since its visible rows count on their own.
    pub fn visual_hosts(&self, store: &HostStore, items: &[Item]) -> Vec<String> {
        let Some((lo, hi)) = self.visual_range() else { return Vec::new() };
        let hi = hi.min(items.len().saturating_sub(1));
        let mut out: Vec<String> = Vec::new();
        let mut push = |id: &str| {
            if !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        };
        for item in items.get(lo..=hi).unwrap_or(&[]) {
            match item {
                Item::Host { id, .. } => push(id),
                Item::Folder { path, expanded: false, .. } => store.hosts_under(path).for_each(|h| push(&h.id)),
                Item::Folder { .. } => {}
            }
        }
        out
    }

    /// Marks every host in `ids`, or unmarks them all if they were all marked already.
    pub fn toggle_marks(&mut self, ids: Vec<String>) {
        let all = ids.iter().all(|i| self.marked.contains(i));
        for i in ids {
            if all {
                self.marked.remove(&i);
            } else {
                self.marked.insert(i);
            }
        }
    }

    pub fn ensure_visible(&mut self, height: usize) {
        if height == 0 {
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + height {
            self.scroll = self.selected + 1 - height;
        }
    }

    pub fn select_host(&mut self, store: &HostStore, id: &str) {
        if let Some(folder) = store.get(id).map(|h| h.folder.clone()) {
            self.expand_path(&folder);
        }
        let items = self.items(store);
        if let Some(idx) = items.iter().position(|i| matches!(i, Item::Host { id: hid, .. } if hid == id)) {
            self.selected = idx;
        }
    }

    /// Follows a folder renamed from `from` to `to`: its subfolders stay expanded or collapsed
    /// as they were, and the cursor lands on it.
    pub fn folder_renamed(&mut self, store: &HostStore, from: &str, to: &str) {
        let moved: Vec<String> =
            self.expanded.iter().filter_map(|f| subpath(f, from).map(|rest| format!("{to}{rest}"))).collect();
        self.expanded.retain(|f| subpath(f, from).is_none());
        self.expanded.extend(moved);
        self.expand_path(parent_of(to));
        let items = self.items(store);
        if let Some(idx) = items.iter().position(|i| matches!(i, Item::Folder { path, .. } if path == to)) {
            self.selected = idx;
        }
    }

    /// Expands `folder` and every folder above it.
    fn expand_path(&mut self, folder: &str) {
        let mut acc = String::new();
        for part in folder.split('/').filter(|p| !p.is_empty()) {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(part);
            self.expanded.insert(acc.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::hosts::Host;

    #[test]
    fn tree_and_search() {
        let mut store = HostStore::default();
        for (name, folder) in [("web1", "acme/prod"), ("db1", "acme/prod"), ("lab", ""), ("bastion", "acme")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: format!("{name}.local"), ..Host::default() });
        }
        let mut sb = Sidebar::default();
        let items = sb.items(&store);
        assert!(matches!(&items[0], Item::Folder { path, expanded: true, count: 3, .. } if path == "acme"));
        assert!(matches!(&items[1], Item::Folder { path, expanded: false, .. } if path == "acme/prod"));
        sb.expanded.insert("acme/prod".into());
        assert_eq!(sb.items(&store).len(), 6);
        sb.query = "web".into();
        let found = sb.items(&store);
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn visual_selection() {
        let mut store = HostStore::default();
        for (name, folder) in [("web1", "acme/prod"), ("db1", "acme/prod"), ("lab", ""), ("bastion", "acme")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: format!("{name}.local"), ..Host::default() });
        }
        let name = |ids: Vec<String>| -> Vec<String> { ids.iter().map(|id| store.get(id).unwrap().name.clone()).collect() };
        let mut sb = Sidebar::default();
        // acme ▾, acme/prod ▸ (collapsed: stands for db1 + web1), bastion, lab
        let items = sb.items(&store);
        sb.selected = 3;
        sb.visual = Some(1);
        assert_eq!(name(sb.visual_hosts(&store, &items)), ["db1", "web1", "bastion", "lab"]);

        // The cursor above the anchor works too, and an expanded folder row adds nothing by itself.
        sb.expanded.insert("acme/prod".into());
        let items = sb.items(&store);
        sb.visual = Some(2);
        sb.selected = 0;
        assert_eq!(name(sb.visual_hosts(&store, &items)), ["db1"]);

        sb.toggle_marks(sb.visual_hosts(&store, &items));
        assert!(sb.marked.contains(&store.hosts.iter().find(|h| h.name == "db1").unwrap().id));
        sb.clamp(0);
        assert_eq!((sb.selected, sb.visual), (0, Some(0)));
    }

    #[test]
    fn folder_rename_keeps_expansion() {
        let mut store = HostStore::default();
        for (name, folder) in [("web1", "acme/prod"), ("lab", ""), ("x", "zeta")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: format!("{name}.local"), ..Host::default() });
        }
        let mut sb = Sidebar::default();
        sb.items(&store);
        sb.expanded.insert("acme/prod".into());

        store.rename_folder("acme", "zeta/acme2");
        sb.folder_renamed(&store, "acme", "zeta/acme2");
        assert!(!sb.expanded.iter().any(|f| f.starts_with("acme")));
        assert!(sb.expanded.contains("zeta/acme2") && sb.expanded.contains("zeta/acme2/prod"));
        let items = sb.items(&store);
        assert!(matches!(&items[sb.selected], Item::Folder { path, .. } if path == "zeta/acme2"));
    }
}
