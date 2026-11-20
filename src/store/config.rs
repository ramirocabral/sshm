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
    /// Colors of the UI: default, light, nord, gruvbox, dracula, catppuccin, solarized.
    pub theme: String,
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
            theme: "default".into(),
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
        if crate::ui::theme::find(&cfg.theme).is_none() {
            bail!("unknown `theme` \"{}\" in {} (available: {})", cfg.theme, path.display(), crate::ui::theme::names());
        }
        Ok(cfg)
    }

    /// Sets `theme` in config.toml and leaves the rest of the file, comments included, as it was.
    pub fn save_theme(name: &str) -> Result<()> {
        let path = Self::path();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let value = toml::Value::String(name.to_string()).to_string();
        let new = set_top_level_key(&text, "theme", &value);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, new).with_context(|| format!("writing {}", tmp.display()))?;
        if let Ok(meta) = std::fs::metadata(&path) {
            std::fs::set_permissions(&tmp, meta.permissions()).ok();
        }
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
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

/// Replaces the top-level `key = …` line, or adds one before the first [table] (or at the end).
fn set_top_level_key(text: &str, key: &str, value: &str) -> String {
    let line = format!("{key} = {value}");
    let mut out: Vec<String> = Vec::new();
    let mut done = false;
    let mut in_table = false;
    for l in text.lines() {
        let trimmed = l.trim_start();
        if trimmed.starts_with('[') {
            if !done {
                out.push(line.clone());
                done = true;
            }
            in_table = true;
        }
        let is_key = !trimmed.starts_with('#') && trimmed.split_once('=').is_some_and(|(k, _)| k.trim() == key);
        if !in_table && is_key && !done {
            out.push(line.clone());
            done = true;
            continue;
        }
        out.push(l.to_string());
    }
    if !done {
        out.push(line);
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_top_level_key_keeps_the_rest() {
        let text = "leader = \"ctrl+space\"\n# theme = \"x\"\ntheme = \"nord\"   # mine\nscrollback = 5\n";
        assert_eq!(
            set_top_level_key(text, "theme", "\"dracula\""),
            "leader = \"ctrl+space\"\n# theme = \"x\"\ntheme = \"dracula\"\nscrollback = 5\n"
        );
        assert_eq!(set_top_level_key("a = 1", "theme", "\"x\""), "a = 1\ntheme = \"x\"\n");
        assert_eq!(set_top_level_key("a = 1\n[t]\ntheme = 2\n", "theme", "\"x\""), "a = 1\ntheme = \"x\"\n[t]\ntheme = 2\n");
        assert_eq!(set_top_level_key("", "theme", "\"x\""), "theme = \"x\"\n");
    }

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
