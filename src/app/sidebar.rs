use std::collections::HashSet;

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config as MatchConfig, Matcher, Utf32Str};

use super::widgets::TextInput;
use crate::store::hosts::HostStore;

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
            let count = store.hosts.iter().filter(|h| h.folder == *f || h.folder.starts_with(&format!("{f}/"))).count();
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
        if len == 0 {
            self.selected = 0;
        } else if self.selected >= len {
            self.selected = len - 1;
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
        if let Some(h) = store.get(id) {
            let mut acc = String::new();
            for part in h.folder.split('/').filter(|p| !p.is_empty()) {
                if !acc.is_empty() {
                    acc.push('/');
                }
                acc.push_str(part);
                self.expanded.insert(acc.clone());
            }
        }
        let items = self.items(store);
        if let Some(idx) = items.iter().position(|i| matches!(i, Item::Host { id: hid, .. } if hid == id)) {
            self.selected = idx;
        }
    }
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(p, _)| p).unwrap_or("")
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
}
