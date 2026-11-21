//! Every app action and its keys; dispatch, help, status hints, welcome screen and palette read `BINDINGS`.

use std::sync::LazyLock;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::store::config::KeySpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SendLeader,
    ToggleFocus,
    ToggleSidebar,
    LocalShell,
    Sftp,
    Duplicate,
    ToggleBroadcast,
    ToggleExclude,
    TypePassword,
    TypeRootPassword,
    CopyMode,
    Reconnect,
    Close,
    CloseTab,
    Zoom,
    SelectTab(usize),
    FocusPane(Dir),
    NextPane,
    NextTab,
    PrevTab,
    RenameTab,
    NewHost,
    Edit,
    CopyHost,
    DeleteHosts,
    MoveHosts,
    Mark,
    Visual,
    ExitVisual,
    Search,
    Expand,
    Collapse,
    Activate,
    ActivateHere,
    CopyTarget,
    Down,
    Up,
    PageDown,
    PageUp,
    Top,
    Bottom,
    ClearOrBack,
    Import,
    Export,
    ImportSshConfig,
    LockVault,
    ChangeMaster,
    Theme,
    Help,
    Palette,
    Paths,
    Quit,
    Tunnels,
    TunnelNew,
    TunnelToggle,
    TunnelEdit,
    TunnelDelete,
    TunnelRestart,
    TunnelCopy,
}

/// What the command palette offers, in order.
pub const PALETTE: [Action; 15] = [
    Action::NewHost,
    Action::Tunnels,
    Action::TunnelNew,
    Action::LocalShell,
    Action::ImportSshConfig,
    Action::Import,
    Action::Export,
    Action::LockVault,
    Action::ChangeMaster,
    Action::ToggleBroadcast,
    Action::ToggleSidebar,
    Action::Theme,
    Action::Help,
    Action::Paths,
    Action::Quit,
];

impl Action {
    /// Title in the command palette.
    pub fn title(self) -> &'static str {
        match self {
            Action::NewHost => "New host",
            Action::Tunnels => "Tunnels (port forwarding)",
            Action::TunnelNew => "New tunnel…",
            Action::LocalShell => "Open a local shell",
            Action::ImportSshConfig => "Import ~/.ssh/config",
            Action::Import => "Import from file… (JSON, CSV, bundle, MobaXterm, Ásbrú, Remmina)",
            Action::Export => "Export hosts…",
            Action::LockVault => "Lock the vault",
            Action::ChangeMaster => "Change the master password",
            Action::ToggleBroadcast => "Toggle broadcast in this tab",
            Action::ToggleSidebar => "Show/hide the host list",
            Action::Theme => "Change the theme",
            Action::Help => "Help and key bindings",
            Action::Paths => "Show config and data locations",
            Action::Quit => "Quit",
            _ => "",
        }
    }
}

/// Where a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ctx {
    /// The key after the leader.
    Leader,
    /// Alt shortcuts that skip the leader (`alt_shortcuts = true`).
    Alt,
    /// Cursor movement in the host list, in normal and visual mode.
    Motion,
    Hosts,
    /// Visual selection in the host list.
    Visual,
    /// The tunnels tab.
    Tunnels,
}

#[derive(Clone, Copy, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

fn mods_label(mods: KeyModifiers) -> String {
    let mut s = String::new();
    if mods.contains(KeyModifiers::CONTROL) {
        s.push_str("Ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        s.push_str("Alt+");
    }
    s
}

impl Key {
    pub fn label(&self) -> String {
        let mut s = mods_label(self.mods);
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) => s.push(c),
            KeyCode::Up => s.push('↑'),
            KeyCode::Down => s.push('↓'),
            KeyCode::Left => s.push('←'),
            KeyCode::Right => s.push('→'),
            KeyCode::PageUp => s.push_str("PgUp"),
            KeyCode::PageDown => s.push_str("PgDn"),
            KeyCode::Delete => s.push_str("Del"),
            other => s.push_str(&format!("{other:?}")),
        }
        s
    }
}

pub enum Keys {
    List(Vec<Key>),
    /// 1..9 → SelectTab(0..8) with these modifiers.
    Digits(KeyModifiers),
    /// The leader key itself.
    Leader,
}

pub struct Binding {
    pub ctx: Ctx,
    pub keys: Keys,
    pub action: Action,
    /// Help text; empty when the row above documents this binding too.
    pub desc: &'static str,
    /// Key column of the help row instead of the keys it covers.
    pub help_keys: &'static str,
    /// Status-bar hint; consecutive equal hints merge into "a/b hint".
    pub short: &'static str,
    /// Listed on the welcome screen.
    pub welcome: bool,
}

impl Binding {
    fn help_keys(mut self, s: &'static str) -> Self {
        self.help_keys = s;
        self
    }

    fn short(mut self, s: &'static str) -> Self {
        self.short = s;
        self
    }

    fn welcome(mut self) -> Self {
        self.welcome = true;
        self
    }

    /// The action for `ev`; `exact` requires the same modifiers, otherwise a binding without
    /// modifiers also takes the key with any (Shift never counts: it is in the character).
    fn action_for(&self, ev: &KeyEvent, exact: bool, leader: &KeySpec) -> Option<Action> {
        let got = ev.modifiers - KeyModifiers::SHIFT;
        let mods_ok = |want: KeyModifiers| if exact { got == want } else { want.is_empty() };
        match &self.keys {
            Keys::Leader => leader.matches(ev).then_some(self.action),
            Keys::Digits(mods) => match ev.code {
                KeyCode::Char(c @ '1'..='9') if mods_ok(*mods) => Some(Action::SelectTab(c as usize - '1' as usize)),
                _ => None,
            },
            Keys::List(keys) => keys.iter().any(|k| k.code == ev.code && mods_ok(k.mods)).then_some(self.action),
        }
    }

    /// How its keys are written: "j/↓", "1..9", the leader key.
    pub fn keys_label(&self, leader: &KeySpec) -> String {
        match &self.keys {
            Keys::Leader => leader.label(),
            Keys::Digits(mods) => format!("{}1..9", mods_label(*mods)),
            Keys::List(keys) => keys.iter().map(Key::label).collect::<Vec<_>>().join("/"),
        }
    }

    /// The first key only, for hints.
    fn first_key_label(&self, leader: &KeySpec) -> String {
        match &self.keys {
            Keys::List(keys) => keys.first().map(Key::label).unwrap_or_default(),
            _ => self.keys_label(leader),
        }
    }
}

fn bind(ctx: Ctx, keys: &[Key], action: Action, desc: &'static str) -> Binding {
    Binding { ctx, keys: Keys::List(keys.to_vec()), action, desc, help_keys: "", short: "", welcome: false }
}

fn bind_keys(ctx: Ctx, keys: Keys, action: Action, desc: &'static str) -> Binding {
    Binding { ctx, keys, action, desc, help_keys: "", short: "", welcome: false }
}

const fn ch(c: char) -> Key {
    Key { code: KeyCode::Char(c), mods: KeyModifiers::NONE }
}

const fn code(code: KeyCode) -> Key {
    Key { code, mods: KeyModifiers::NONE }
}

const fn ctrl(c: char) -> Key {
    Key { code: KeyCode::Char(c), mods: KeyModifiers::CONTROL }
}

const fn alt(code: KeyCode) -> Key {
    Key { code, mods: KeyModifiers::ALT }
}

pub static BINDINGS: LazyLock<Vec<Binding>> = LazyLock::new(|| {
    use Action as A;
    use Ctx::*;
    vec![
        bind(Leader, &[code(KeyCode::Tab)], A::ToggleFocus, "go to the host list / back"),
        bind(Leader, &[ch('\\')], A::ToggleSidebar, "show/hide the host list"),
        bind_keys(Leader, Keys::Digits(KeyModifiers::NONE), A::SelectTab(0), "go to tab N"),
        bind(Leader, &[ch('L'), ch('N')], A::NextTab, "next / previous tab").help_keys("L / H  ·  N / B"),
        bind(Leader, &[ch('H'), ch('B')], A::PrevTab, ""),
        bind(Leader, &[ch('h'), code(KeyCode::Left)], A::FocusPane(Dir::Left), "move between panes").help_keys("hjkl  ·  ←↓↑→"),
        bind(Leader, &[ch('j'), code(KeyCode::Down)], A::FocusPane(Dir::Down), ""),
        bind(Leader, &[ch('k'), code(KeyCode::Up)], A::FocusPane(Dir::Up), ""),
        bind(Leader, &[ch('l'), code(KeyCode::Right)], A::FocusPane(Dir::Right), ""),
        bind(Leader, &[ch('o')], A::NextPane, "next pane"),
        bind(Leader, &[ch('z')], A::Zoom, "zoom the pane (full screen)").short("zoom"),
        bind(Leader, &[ch('d')], A::Duplicate, "duplicate the session into this tab (grid)").short("duplicate"),
        bind(Leader, &[ch('t')], A::LocalShell, "local shell in a new tab"),
        bind(Leader, &[ch('s')], A::Sftp, "SFTP to the current host").short("sftp"),
        bind(Leader, &[ch('T')], A::Tunnels, "tunnels (port forwarding)"),
        bind(Leader, &[ch('b')], A::ToggleBroadcast, "broadcast: type into every pane of the tab").short("broadcast"),
        bind(Leader, &[ch('x')], A::ToggleExclude, "exclude/include the pane in broadcast").short("exclude"),
        bind(Leader, &[ch('p')], A::TypePassword, "type the host password (sudo, etc.)").short("password"),
        bind(Leader, &[ch('P')], A::TypeRootPassword, "type the root password (su -)").short("password"),
        bind(Leader, &[ch('[')], A::CopyMode, "copy mode (vi): v/V select, y copies, q exits").short("copy"),
        bind(Leader, &[ch('r')], A::Reconnect, "reconnect").short("reconnect"),
        bind(Leader, &[ch('w')], A::Close, "close pane / tab").short("close pane/tab"),
        bind(Leader, &[ch('W')], A::CloseTab, "close the whole tab right away (no confirmation)").short("close pane/tab"),
        bind(Leader, &[ch(',')], A::RenameTab, "rename tab"),
        bind(Leader, &[ch('n')], A::NewHost, "new host / edit the current host").help_keys("n / e").short("new"),
        bind(Leader, &[ch('e')], A::Edit, ""),
        bind(Leader, &[ch('i')], A::Import, "import"),
        bind(Leader, &[ch(':')], A::Palette, "command palette"),
        bind(Leader, &[ch('?')], A::Help, "this help").short("help"),
        bind_keys(Leader, Keys::Leader, A::SendLeader, "send the leader key to the session"),
        bind(Leader, &[ch('q')], A::Quit, "quit"),
        bind_keys(Alt, Keys::Digits(KeyModifiers::ALT), A::SelectTab(0), "go to tab N"),
        bind(Alt, &[alt(KeyCode::Left)], A::FocusPane(Dir::Left), "move between panes").help_keys("Alt+arrows"),
        bind(Alt, &[alt(KeyCode::Down)], A::FocusPane(Dir::Down), ""),
        bind(Alt, &[alt(KeyCode::Up)], A::FocusPane(Dir::Up), ""),
        bind(Alt, &[alt(KeyCode::Right)], A::FocusPane(Dir::Right), ""),
        bind(Motion, &[ch('j'), code(KeyCode::Down)], A::Down, "move").help_keys("j/k ↑↓ · g/G · Ctrl+d/u"),
        bind(Motion, &[ch('k'), code(KeyCode::Up)], A::Up, ""),
        bind(Motion, &[ctrl('d'), code(KeyCode::PageDown)], A::PageDown, ""),
        bind(Motion, &[ctrl('u'), code(KeyCode::PageUp)], A::PageUp, ""),
        bind(Motion, &[ch('g'), code(KeyCode::Home)], A::Top, ""),
        bind(Motion, &[ch('G'), code(KeyCode::End)], A::Bottom, ""),
        bind(Hosts, &[ch('l'), code(KeyCode::Right)], A::Expand, "collapse/expand folder").help_keys("h/l ←→"),
        bind(Hosts, &[ch('h'), code(KeyCode::Left)], A::Collapse, ""),
        bind(Hosts, &[code(KeyCode::Enter)], A::Activate, "connect (or open every marked host in a grid)").short("connect").welcome(),
        bind(Hosts, &[ch(' ')], A::Mark, "mark host (on a folder: marks all)").short("mark").welcome(),
        bind(Hosts, &[ch('v'), ch('V')], A::Visual, "select a range with j/k, then act on all of it at once")
            .help_keys("v / V")
            .short("select")
            .welcome(),
        bind(Hosts, &[ch('o')], A::ActivateHere, "open in the current tab").short("add to tab").welcome(),
        bind(Hosts, &[ch('s')], A::Sftp, "SFTP").short("sftp").welcome(),
        bind(Hosts, &[ch('T')], A::Tunnels, "tunnels (port forwarding) of the host").short("tunnels").welcome(),
        bind(Hosts, &[ch('n')], A::NewHost, "new / edit (on a folder: rename) / duplicate / delete host")
            .help_keys("n / e / c / d")
            .short("host")
            .welcome(),
        bind(Hosts, &[ch('e')], A::Edit, "").short("host"),
        bind(Hosts, &[ch('c')], A::CopyHost, ""),
        bind(Hosts, &[ch('d'), code(KeyCode::Delete)], A::DeleteHosts, "").short("host"),
        bind(Hosts, &[ch('m')], A::MoveHosts, "move the marked hosts, or the folder/host under the cursor, to another folder")
            .short("move"),
        bind(Hosts, &[ch('/')], A::Search, "search (fuzzy: name, host, user or tag); Ctrl+j/k moves while typing")
            .short("search")
            .welcome(),
        bind(Hosts, &[ch('y')], A::CopyTarget, "copy user@host"),
        bind(Hosts, &[ch('i')], A::Import, "import / export").help_keys("i / x").welcome(),
        bind(Hosts, &[ch('x')], A::Export, ""),
        bind(Hosts, &[ch('t')], A::LocalShell, "local shell"),
        bind(Hosts, &[ch('?')], A::Help, "help").short("help"),
        bind(Hosts, &[ch(':')], A::Palette, "command palette"),
        bind(Hosts, &[ch('q')], A::Quit, "quit"),
        bind(Hosts, &[code(KeyCode::Esc)], A::ClearOrBack, "back to the session (Esc clears the marks first)").help_keys("Esc / Tab"),
        bind(Hosts, &[code(KeyCode::Tab)], A::ToggleFocus, ""),
        bind(Visual, &[code(KeyCode::Enter)], A::Activate, "open the selection in a grid").short("open in a grid"),
        bind(Visual, &[ch('o')], A::ActivateHere, "open it in the current tab").short("add to tab"),
        bind(Visual, &[ch(' ')], A::Mark, "mark it").short("mark"),
        bind(Visual, &[ch('m')], A::MoveHosts, "move it to another folder").short("move"),
        bind(Visual, &[ch('d'), code(KeyCode::Delete)], A::DeleteHosts, "delete it").short("delete"),
        bind(Visual, &[code(KeyCode::Esc), ch('v'), ch('V'), ch('q')], A::ExitVisual, "cancel").short("cancel"),
        bind(Tunnels, &[code(KeyCode::Enter), ch(' ')], A::TunnelToggle, "turn the tunnel on/off (on a host: all of its tunnels)")
            .short("on/off"),
        bind(Tunnels, &[ch('n')], A::TunnelNew, "new / edit / delete tunnel (e on a host edits the host)")
            .help_keys("n / e / d")
            .short("tunnel"),
        bind(Tunnels, &[ch('e')], A::TunnelEdit, "").short("tunnel"),
        bind(Tunnels, &[ch('d'), code(KeyCode::Delete)], A::TunnelDelete, "").short("tunnel"),
        bind(Tunnels, &[ch('r')], A::TunnelRestart, "reconnect the host and restart its tunnels").short("reconnect"),
        bind(Tunnels, &[ch('y')], A::TunnelCopy, "copy the address to connect to, e.g. localhost:5432").short("copy"),
        bind(Tunnels, &[code(KeyCode::Tab)], A::ToggleFocus, "go to the host list"),
        bind(Tunnels, &[ch('?')], A::Help, "help").short("help"),
        bind(Tunnels, &[ch(':')], A::Palette, "command palette"),
        bind(Tunnels, &[ch('q')], A::Close, "close the tab (the tunnels keep running)").short("close"),
    ]
});

/// The action bound to `ev` in the first of `ctxs` that has one; exact modifier matches win.
pub fn lookup(ctxs: &[Ctx], ev: &KeyEvent, leader: &KeySpec) -> Option<Action> {
    let rows = || BINDINGS.iter().filter(|b| ctxs.contains(&b.ctx));
    rows()
        .find_map(|b| b.action_for(ev, true, leader))
        .or_else(|| rows().filter(|b| b.ctx != Ctx::Alt).find_map(|b| b.action_for(ev, false, leader)))
}

pub struct HelpRow {
    pub keys: String,
    pub desc: &'static str,
    pub welcome: bool,
}

/// Help rows of a context; a binding with an empty description joins the row above.
pub fn help_rows(ctx: Ctx, leader: &KeySpec) -> Vec<HelpRow> {
    let mut out: Vec<HelpRow> = Vec::new();
    let mut overridden = false;
    for b in BINDINGS.iter().filter(|b| b.ctx == ctx) {
        if b.desc.is_empty() {
            if let Some(last) = out.last_mut().filter(|_| !overridden) {
                last.keys.push_str(" / ");
                last.keys.push_str(&b.keys_label(leader));
            }
            continue;
        }
        overridden = !b.help_keys.is_empty();
        let keys = if overridden { b.help_keys.to_string() } else { b.keys_label(leader) };
        out.push(HelpRow { keys, desc: b.desc, welcome: b.welcome });
    }
    out
}

/// "a hint · b/c other hint" for the status bar.
pub fn hints(ctx: Ctx, leader: &KeySpec) -> String {
    let mut parts: Vec<(String, &str)> = Vec::new();
    for b in BINDINGS.iter().filter(|b| b.ctx == ctx && !b.short.is_empty()) {
        let key = b.first_key_label(leader);
        match parts.last_mut() {
            Some((keys, short)) if *short == b.short => {
                keys.push('/');
                keys.push_str(&key);
            }
            _ => parts.push((key, b.short)),
        }
    }
    parts.iter().map(|(k, s)| format!("{k} {s}")).collect::<Vec<_>>().join(" · ")
}

/// Keys that run `action` from the host list and after the leader, e.g. "n · Ctrl+Space n".
pub fn keys_for(action: Action, leader: &KeySpec) -> String {
    let mut out = Vec::new();
    for ctx in [Ctx::Hosts, Ctx::Leader] {
        if let Some(b) = BINDINGS.iter().find(|b| b.ctx == ctx && b.action == action) {
            let key = b.first_key_label(leader);
            out.push(if ctx == Ctx::Leader { format!("{} {key}", leader.label()) } else { key });
        }
    }
    out.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leader() -> KeySpec {
        KeySpec::parse("ctrl+space").unwrap()
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn lookup_prefers_exact_modifiers() {
        let l = leader();
        let none = KeyModifiers::empty();
        let hosts = [Ctx::Motion, Ctx::Hosts];
        assert_eq!(lookup(&hosts, &key(KeyCode::Char('d'), KeyModifiers::CONTROL), &l), Some(Action::PageDown));
        assert_eq!(lookup(&hosts, &key(KeyCode::Char('d'), none), &l), Some(Action::DeleteHosts));
        assert_eq!(lookup(&hosts, &key(KeyCode::Char('j'), KeyModifiers::CONTROL), &l), Some(Action::Down));
        assert_eq!(lookup(&hosts, &key(KeyCode::Char('N'), KeyModifiers::SHIFT), &l), None);
        assert_eq!(lookup(&[Ctx::Leader], &key(KeyCode::Char('N'), KeyModifiers::SHIFT), &l), Some(Action::NextTab));
        assert_eq!(lookup(&[Ctx::Leader], &key(KeyCode::Char('3'), none), &l), Some(Action::SelectTab(2)));
        assert_eq!(lookup(&[Ctx::Leader], &key(KeyCode::Char(' '), KeyModifiers::CONTROL), &l), Some(Action::SendLeader));
        assert_eq!(lookup(&[Ctx::Leader], &key(KeyCode::Char('n'), KeyModifiers::CONTROL), &l), Some(Action::NewHost));
        assert_eq!(lookup(&[Ctx::Alt], &key(KeyCode::Char('4'), KeyModifiers::ALT), &l), Some(Action::SelectTab(3)));
        assert_eq!(lookup(&[Ctx::Alt], &key(KeyCode::Char('4'), none), &l), None);
        assert_eq!(lookup(&[Ctx::Visual], &key(KeyCode::Char('n'), none), &l), None);
    }

    #[test]
    fn help_hints_and_palette_come_from_the_table() {
        let l = leader();
        let rows = help_rows(Ctx::Leader, &l);
        assert!(rows.iter().any(|r| r.keys == "1..9" && r.desc == "go to tab N"));
        assert!(rows.iter().any(|r| r.keys == "Ctrl+Space" && r.desc.starts_with("send the leader")));
        assert!(help_rows(Ctx::Alt, &l).iter().any(|r| r.keys == "Alt+1..9"));
        let hosts = help_rows(Ctx::Hosts, &l);
        assert!(hosts.iter().any(|r| r.keys == "n / e / c / d"));
        assert!(hosts.iter().any(|r| r.keys == "Enter" && r.welcome));
        let h = hints(Ctx::Leader, &l);
        assert!(h.contains("p/P password") && h.contains("w/W close pane/tab"));
        assert!(hints(Ctx::Hosts, &l).contains("n/e/d host"));
        assert!(hints(Ctx::Tunnels, &l).starts_with("Enter on/off · n/e/d tunnel · r reconnect"));
        assert_eq!(keys_for(Action::Tunnels, &l), "T · Ctrl+Space T");
        assert_eq!(keys_for(Action::NewHost, &l), "n · Ctrl+Space n");
        assert_eq!(keys_for(Action::LockVault, &l), "");
        assert!(PALETTE.iter().all(|a| !a.title().is_empty()));
    }
}
