//! The desktop keyring (GNOME Keyring, KWallet, KeePassXC… anything that speaks the Secret
//! Service API) through `secret-tool` from libsecret, so sshm links no D-Bus library.
//! Entries carry the attributes application=sshm, host=<host id>, kind=password|root_password.
//! All of these block (the keyring may ask to be unlocked): call them off the UI thread.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use super::SecretKind;

const TOOL: &str = "secret-tool";

/// Whether `secret-tool` is installed (somewhere in PATH).
pub fn available() -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| is_executable(&dir.join(TOOL))))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

fn kind_name(kind: SecretKind) -> &'static str {
    match kind {
        SecretKind::Password => "password",
        SecretKind::RootPassword => "root_password",
        SecretKind::Passphrase => "passphrase",
    }
}

fn attributes(host_id: &str, kind: SecretKind) -> Vec<String> {
    ["application", "sshm", "host", host_id, "kind", kind_name(kind)].map(String::from).to_vec()
}

fn run(args: &[String], input: Option<&str>) -> Result<std::process::Output> {
    let mut child = Command::new(TOOL)
        .args(args)
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run `{TOOL}`: install libsecret (it provides {TOOL})"))?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin.write_all(text.as_bytes()).with_context(|| format!("writing to `{TOOL}`"))?;
    }
    child.wait_with_output().with_context(|| format!("waiting for `{TOOL}`"))
}

fn failure(what: &str, output: &std::process::Output) -> anyhow::Error {
    let err = String::from_utf8_lossy(&output.stderr);
    let err = err.trim();
    if err.is_empty() {
        anyhow::anyhow!("`{TOOL} {what}` failed — is a keyring (e.g. gnome-keyring or KWallet) running?")
    } else {
        anyhow::anyhow!("`{TOOL} {what}` failed: {err}")
    }
}

/// The stored secret, or None when the keyring has none for this host.
pub fn get(host_id: &str, kind: SecretKind) -> Result<Option<String>> {
    let mut args = vec!["lookup".to_string()];
    args.extend(attributes(host_id, kind));
    let output = run(&args, None)?;
    let mut out = String::from_utf8(output.stdout.clone()).context("the keyring returned something that is not UTF-8")?;
    if !output.status.success() {
        // secret-tool exits 1 without a word when nothing matches.
        return if output.stderr.is_empty() && out.is_empty() { Ok(None) } else { Err(failure("lookup", &output)) };
    }
    while out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    Ok(Some(out).filter(|s| !s.is_empty()))
}

/// Stores `value`; `label` is what the keyring app shows for the entry.
pub fn set(host_id: &str, kind: SecretKind, label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("refusing to store an empty password");
    }
    let mut args = vec!["store".to_string(), format!("--label={label}")];
    args.extend(attributes(host_id, kind));
    let output = run(&args, Some(value))?;
    if !output.status.success() {
        return Err(failure("store", &output));
    }
    Ok(())
}

/// Removes the secret; removing one that is not there is not an error.
pub fn clear(host_id: &str, kind: SecretKind) -> Result<()> {
    let mut args = vec!["clear".to_string()];
    args.extend(attributes(host_id, kind));
    let output = run(&args, None)?;
    if !output.status.success() && !output.stderr.is_empty() {
        return Err(failure("clear", &output));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs a running Secret Service: `cargo test -- --ignored keyring`.
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let id = format!("test-{}", crate::store::hosts::new_id());
        assert_eq!(get(&id, SecretKind::Password).unwrap(), None);
        set(&id, SecretKind::Password, "sshm test entry", "p4ss w0rd").unwrap();
        assert_eq!(get(&id, SecretKind::Password).unwrap().as_deref(), Some("p4ss w0rd"));
        assert_eq!(get(&id, SecretKind::RootPassword).unwrap(), None);
        clear(&id, SecretKind::Password).unwrap();
        clear(&id, SecretKind::Password).unwrap();
        assert_eq!(get(&id, SecretKind::Password).unwrap(), None);
    }

    #[test]
    fn attributes_identify_host_and_kind() {
        assert_eq!(attributes("abc", SecretKind::RootPassword), ["application", "sshm", "host", "abc", "kind", "root_password"]);
    }
}
