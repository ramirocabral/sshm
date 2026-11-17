use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Prefix key for app actions while a terminal has focus.
    pub leader: String,
    /// Alt+1..9 switches tabs and Alt+arrows moves between panes without the leader.
    pub alt_shortcuts: bool,
    /// Reuse one SSH connection per host (sftp and duplicated sessions skip authentication).
    pub control_master: bool,
    pub control_persist: u32,
    pub scrollback: usize,
    pub sidebar_width: u16,
    pub ssh_command: String,
    /// TERM exported to the sessions.
    pub term: String,
    /// Overrides clipboard detection, e.g. "wl-copy" or "xclip -selection clipboard".
    pub clipboard_command: Option<String>,
    /// Seconds after which an unused vault gets locked again (0 = never).
    pub vault_timeout: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            leader: "ctrl+space".into(),
            alt_shortcuts: true,
            control_master: true,
            control_persist: 60,
            scrollback: 10_000,
            sidebar_width: 32,
            ssh_command: "ssh".into(),
            term: "xterm-256color".into(),
            clipboard_command: None,
            vault_timeout: 0,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        paths::config_dir().join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        KeySpec::parse(&cfg.leader).with_context(|| format!("invalid `leader` in {}", path.display()))?;
        Ok(cfg)
    }

    pub fn leader_key(&self) -> KeySpec {
        KeySpec::parse(&self.leader).unwrap_or(KeySpec { code: KeyCode::Char(' '), mods: KeyModifiers::CONTROL })
    }

    pub fn sample() -> String {
        toml::to_string_pretty(&Config::default()).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySpec {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeySpec {
    pub fn parse(s: &str) -> Result<Self> {
        let mut mods = KeyModifiers::empty();
        let parts: Vec<String> = s.split('+').map(|p| p.trim().to_lowercase()).collect();
        let Some((last, prefix)) = parts.split_last() else { bail!("empty key") };
        for m in prefix {
            match m.as_str() {
                "ctrl" | "control" | "c" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" | "m" => mods |= KeyModifiers::ALT,
                "shift" | "s" => mods |= KeyModifiers::SHIFT,
                other => bail!("unknown modifier: {other}"),
            }
        }
        let code = match last.as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "esc" | "escape" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            k if k.len() > 1 && k.starts_with('f') && k[1..].parse::<u8>().is_ok() => KeyCode::F(k[1..].parse()?),
            k if k.chars().count() == 1 => KeyCode::Char(k.chars().next().unwrap()),
            other => bail!("unknown key: {other}"),
        };
        Ok(KeySpec { code, mods })
    }

    pub fn matches(&self, ev: &KeyEvent) -> bool {
        let ev_mods = ev.modifiers - KeyModifiers::SHIFT;
        let my_mods = self.mods - KeyModifiers::SHIFT;
        let code_eq = match (self.code, ev.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => a.eq_ignore_ascii_case(&b),
            (a, b) => a == b,
        };
        // Some terminals report Ctrl+Space as Ctrl+'@' (NUL).
        let nul_alias = self.code == KeyCode::Char(' ')
            && self.mods == KeyModifiers::CONTROL
            && ev.code == KeyCode::Char('@')
            && ev.modifiers.contains(KeyModifiers::CONTROL);
        (code_eq && ev_mods == my_mods) || nul_alias
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("Ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("Alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) => s.push(c.to_ascii_uppercase()),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            other => s.push_str(&format!("{other:?}")),
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys() {
        let k = KeySpec::parse("ctrl+space").unwrap();
        assert_eq!(k.code, KeyCode::Char(' '));
        assert!(k.mods.contains(KeyModifiers::CONTROL));
        assert!(k.matches(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
        assert!(k.matches(&KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL)));
        assert!(!k.matches(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty())));
        assert_eq!(KeySpec::parse("F12").unwrap().code, KeyCode::F(12));
        assert!(KeySpec::parse("hyper+x").is_err());
        assert_eq!(KeySpec::parse("ctrl+b").unwrap().label(), "Ctrl+B");
    }
}
