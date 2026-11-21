//! The desktop keyring (GNOME Keyring, KWallet, KeePassXC… anything that speaks the Secret
//! Service API) through `secret-tool` from libsecret, so sshm links no D-Bus library.
//! Entries carry the attributes application=sshm, host=<host id>, kind=password|root_password.
//! All of these block (the keyring may ask to be unlocked): call them off the UI thread.

use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::SecretKind;

const TOOL: &str = "secret-tool";

/// Unlocking the keyring may pop up a password prompt, so this waits longer than other sources.
const TIMEOUT: Duration = Duration::from_secs(180);

/// Whether `secret-tool` is installed (somewhere in PATH).
pub fn available() -> bool {
    crate::paths::in_path(TOOL)
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
    let output = super::run_with_timeout(Command::new(TOOL).args(args), input.map(str::as_bytes), TIMEOUT)
        .with_context(|| format!("could not run `{TOOL}`: install libsecret (it provides {TOOL})"))?;
    match output {
        Some(o) => Ok(o),
        None => bail!("`{TOOL}` did not answer within {} s — is the keyring waiting to be unlocked?", TIMEOUT.as_secs()),
    }
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
