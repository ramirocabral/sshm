use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{Context, Result};

fn xdg(var: &str, fallback: &str) -> PathBuf {
    match std::env::var_os(var) {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => home().join(fallback),
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("sshm")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("sshm")
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("sshm")
}

/// Private directory for sockets; falls back to /tmp/sshm-<uid> when XDG_RUNTIME_DIR is unset.
pub fn runtime_dir() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("sshm"),
        _ => PathBuf::from(format!("/tmp/sshm-{}", unsafe_uid())),
    };
    ensure_private_dir(&dir)?;
    Ok(dir)
}

pub fn ensure_private_dir(dir: &PathBuf) -> Result<()> {
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok();
    Ok(())
}

pub fn unsafe_uid() -> u32 {
    // SAFETY: getuid never fails and has no preconditions.
    unsafe { libc_getuid() }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Writes atomically (tmp + rename) with 0600 permissions.
pub fn write_private(path: &PathBuf, data: &[u8]) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        ensure_private_dir(&parent.to_path_buf())?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode_private()
            .open(&tmp)
            .with_context(|| format!("could not write {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

trait ModePrivate {
    fn mode_private(&mut self) -> &mut Self;
}

impl ModePrivate for fs::OpenOptions {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::OpenOptionsExt;
        self.mode(0o600)
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        home().join(rest)
    } else if p == "~" {
        home()
    } else {
        PathBuf::from(p)
    }
}
