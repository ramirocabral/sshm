use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

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

fn runtime_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(v) if !v.is_empty() => PathBuf::from(v).join("sshm"),
        _ => PathBuf::from(format!("/tmp/sshm-{}", uid())),
    }
}

/// Private directory for sockets; falls back to /tmp/sshm-<uid> when XDG_RUNTIME_DIR is unset.
pub fn runtime_dir() -> Result<PathBuf> {
    let dir = runtime_path();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Creates `dir` (0700) if needed and makes it private. Only for sshm's own directories.
pub fn ensure_private_dir(dir: &PathBuf) -> Result<()> {
    create_missing_dir(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).ok();
    Ok(())
}

/// Creates `dir` and its missing parents with 0700; existing directories are left alone.
fn create_missing_dir(dir: &Path) -> Result<()> {
    if !dir.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .with_context(|| format!("could not create {}", dir.display()))?;
    }
    Ok(())
}

fn is_own_dir(dir: &Path) -> bool {
    [data_dir(), state_dir(), config_dir(), runtime_path()].iter().any(|d| d == dir)
}

fn uid() -> u32 {
    // SAFETY: getuid never fails and has no preconditions.
    unsafe { libc::getuid() }
}

/// Writes atomically (tmp + rename) with 0600 permissions.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    write_atomic(path, data, 0o600)
}

/// Atomic write (tmp + rename) with `mode`; existing parent directories keep their permissions.
pub fn write_atomic(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    if is_own_dir(dir) {
        ensure_private_dir(&dir.to_path_buf())?;
    } else {
        create_missing_dir(dir)?;
    }
    let name = path.file_name().ok_or_else(|| anyhow!("{} is not a file path", path.display()))?;
    let tmp = dir.join(format!(".{}.{}.{:08x}.tmp", name.to_string_lossy(), std::process::id(), rand::random::<u32>()));
    let written = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
        f.set_permissions(fs::Permissions::from_mode(mode))?;
        f.write_all(data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("could not write {}", path.display()));
    }
    if let Ok(d) = fs::File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Identifies one version of a file: atomic writes replace the inode, edits change mtime/size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

/// The file's current fingerprint, or None when it does not exist (or cannot be read).
pub fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let m = fs::metadata(path).ok()?;
    Some(Fingerprint { dev: m.dev(), ino: m.ino(), len: m.len(), mtime: m.mtime(), mtime_nsec: m.mtime_nsec() })
}

/// Exclusive advisory lock shared by every sshm instance writing `path`; released on drop.
pub struct FileLock {
    _file: fs::File,
}

pub fn lock_for(path: &Path) -> Result<FileLock> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    create_missing_dir(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let lock = dir.join(format!(".{name}.lock"));
    let f = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock)
        .with_context(|| format!("could not open {}", lock.display()))?;
    f.lock().with_context(|| format!("could not lock {}", lock.display()))?;
    Ok(FileLock { _file: f })
}

/// Whether an executable called `program` is somewhere in PATH.
pub fn in_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| dir.join(program).metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sshm-paths-{}", crate::store::hosts::new_id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn write_private_leaves_the_parent_alone() {
        let dir = scratch();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("export.tmp"), "mine").unwrap();
        let path = dir.join("export.age");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();

        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_to_string(dir.join("export.tmp")).unwrap(), "mine");
        let left: Vec<_> = fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 2, "temporary files left behind: {left:?}");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn write_atomic_creates_private_dirs_and_keeps_mode() {
        let dir = scratch();
        let path = dir.join("a/b/config.toml");
        write_atomic(&path, b"x", 0o644).unwrap();
        assert_eq!(fs::metadata(dir.join("a")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn failed_write_removes_the_temporary_file() {
        let dir = scratch();
        let target = dir.join("taken");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("child"), "x").unwrap();
        assert!(write_private(&target, b"data").is_err());
        let names: Vec<String> =
            fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["taken"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn fingerprint_changes_on_rewrite() {
        let dir = scratch();
        let path = dir.join("f");
        assert_eq!(fingerprint(&path), None);
        write_private(&path, b"1").unwrap();
        let a = fingerprint(&path).unwrap();
        assert_eq!(fingerprint(&path), Some(a));
        write_private(&path, b"1").unwrap();
        assert_ne!(fingerprint(&path), Some(a));
        let _lock = lock_for(&path).unwrap();
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn finds_executables_in_path() {
        assert!(in_path("sh"));
        assert!(!in_path("sshm-no-such-program"));
    }
}
