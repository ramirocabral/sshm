use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::paths;

/// What the UI remembers between runs, in `$XDG_STATE_HOME/sshm/state.toml`. Losing the file
/// only resets these, so read errors fall back to the defaults.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Folders open in the host list. None before the first save: the top level starts open.
    pub expanded: Option<Vec<String>>,
}

impl UiState {
    pub fn path() -> PathBuf {
        paths::state_dir().join("state.toml")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &PathBuf) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save_to(&self, path: &PathBuf) -> Result<()> {
        paths::write_private(path, toml::to_string_pretty(self)?.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_bad_files() {
        let dir = std::env::temp_dir().join(format!("sshm-state-{}", crate::store::hosts::new_id()));
        let path = dir.join("state.toml");
        assert_eq!(UiState::load_from(&path), UiState::default());

        let state = UiState { expanded: Some(vec!["MAT".into(), "MAT/AT&T".into()]) };
        state.save_to(&path).unwrap();
        assert_eq!(UiState::load_from(&path), state);

        std::fs::write(&path, "expanded = 3").unwrap();
        assert_eq!(UiState::load_from(&path), UiState::default());
        std::fs::remove_dir_all(dir).ok();
    }
}
