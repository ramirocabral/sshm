use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config as MatchConfig, Matcher, Utf32Str};

use super::widgets::TextInput;
use crate::store::hosts::{Host, HostStore, parent_folder as parent_of, subpath};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Folder { path: String, name: String, depth: usize, expanded: bool, count: usize },
    /// `idx` is the host's position in `store.hosts` when the list was built.
    Host { id: String, idx: usize, depth: usize },
}

impl Item {
    /// The host this row stands for, if the store has not changed since the list was built.
    pub fn host<'a>(&self, store: &'a HostStore) -> Option<&'a Host> {
        match self {
            Item::Host { id, idx, .. } => store.hosts.get(*idx).filter(|h| h.id == *id).or_else(|| store.get(id)),
            Item::Folder { .. } => None,
        }
    }
}

#[derive(Default)]
struct Node<'a> {
    count: usize,
    folders: Vec<&'a str>,
    hosts: Vec<usize>,
}

/// Folder tree of the store in one pass: hosts per folder, subfolders, and recursive counts.
struct Tree<'a> {
    nodes: HashMap<&'a str, Node<'a>>,
}

/// The folder as `HostStore::folders` sees it: empty segments dropped.
fn clean_folder(folder: &str) -> Cow<'_, str> {
    if folder.split('/').any(str::is_empty) && !folder.is_empty() {
        Cow::Owned(folder.split('/').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("/"))
    } else {
        Cow::Borrowed(folder)
    }
}

impl<'a> Tree<'a> {
    /// `folders[i]` is the clean folder of `store.hosts[i]`.
    fn build(folders: &'a [Cow<'a, str>]) -> Self {
        let mut nodes: HashMap<&'a str, Node<'a>> = HashMap::new();
        nodes.insert("", Node::default());
        for (i, folder) in folders.iter().enumerate() {
            let folder: &'a str = folder;
            let mut end = 0;
            while end < folder.len() {
                end = folder[end..].find('/').map(|p| end + p).unwrap_or(folder.len());
                let path = &folder[..end];
                if !nodes.contains_key(path) {
                    nodes.insert(path, Node::default());
                    nodes.get_mut(parent_of(path)).expect("parent is inserted first").folders.push(path);
                }
                nodes.get_mut(path).expect("just inserted").count += 1;
                end += 1;
            }
            nodes.get_mut(folder).expect("inserted above").hosts.push(i);
        }
        for node in nodes.values_mut() {
            node.folders.sort_unstable();
        }
        Tree { nodes }
    }
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
    /// Starts with the folders that were open last time instead of the whole top level.
    pub fn restore(&mut self, expanded: Vec<String>) {
        self.expanded = expanded.into_iter().collect();
        self.initialized = true;
    }

    pub fn items(&mut self, store: &HostStore) -> Vec<Item> {
        if !self.initialized {
            self.initialized = true;
            self.expanded.extend(store.folders().into_iter().filter(|f| !f.contains('/')));
        }
        if !self.query.trim().is_empty() {
            return self.search_items(store);
        }
        let folders: Vec<Cow<str>> = store.hosts.iter().map(|h| clean_folder(&h.folder)).collect();
        let tree = Tree::build(&folders);
        let mut items = Vec::new();
        self.push_level(store, &tree, "", 0, &mut items);
        items
    }

    fn push_level(&self, store: &HostStore, tree: &Tree, parent: &str, depth: usize, out: &mut Vec<Item>) {
        let Some(node) = tree.nodes.get(parent) else { return };
        for &f in &node.folders {
            let expanded = self.expanded.contains(f);
            let count = tree.nodes.get(f).map_or(0, |n| n.count);
            let name = f.rsplit('/').next().unwrap_or(f).to_string();
            out.push(Item::Folder { path: f.to_string(), name, depth, expanded, count });
            if expanded {
                self.push_level(store, tree, f, depth + 1, out);
            }
        }
        for &idx in &node.hosts {
            out.push(Item::Host { id: store.hosts[idx].id.clone(), idx, depth });
        }
    }

    fn search_items(&self, store: &HostStore) -> Vec<Item> {
        let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
        let mut matcher = Matcher::new(MatchConfig::DEFAULT);
        let mut buf = Vec::new();
        let mut scored: Vec<(u32, usize)> = store
            .hosts
            .iter()
            .enumerate()
            .filter_map(|(i, h)| {
                let hay = format!("{} {} {} {}", h.path(), h.host, h.user.as_deref().unwrap_or(""), h.tags.join(" "));
                pattern.score(Utf32Str::new(&hay, &mut buf), &mut matcher).map(|s| (s, i))
            })
            .collect();
        scored.sort_by_key(|s| std::cmp::Reverse(s.0));
        scored.into_iter().map(|(_, idx)| Item::Host { id: store.hosts[idx].id.clone(), idx, depth: 0 }).collect()
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

    #[test]
    fn tree_order_counts_and_rows() {
        let mut store = HostStore::default();
        for (name, folder) in [("r", ""), ("b1", "B"), ("a1", "a"), ("x1", "a/x"), ("y1", "a/x/y"), ("X1", "a/X"), ("odd", "a//x/")] {
            store.upsert(Host { name: name.into(), folder: folder.into(), host: name.into(), ..Host::default() });
        }
        let mut sb = Sidebar::default();
        sb.expanded.extend(store.folders());
        sb.initialized = true;
        let items = sb.items(&store);
        let rows: Vec<String> = items
            .iter()
            .map(|i| match i {
                Item::Folder { path, count, depth, .. } => format!("{depth}:{path}({count})"),
                Item::Host { depth, .. } => format!("{depth}:{}", i.host(&store).unwrap().name),
            })
            .collect();
        assert_eq!(rows, ["0:B(1)", "1:b1", "0:a(5)", "1:a/X(1)", "2:X1", "1:a/x(3)", "2:a/x/y(1)", "3:y1", "2:odd", "2:x1", "1:a1", "0:r"]);
        store.hosts.remove(0);
        assert_eq!(items[1].host(&store).map(|h| h.name.as_str()), Some("b1"));
    }
}
