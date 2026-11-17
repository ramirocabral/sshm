pub mod vault;

use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::store::hosts::SecretSource;
pub use vault::{SecretKind, Vault};

/// Fetches a secret from an external password manager. Blocking: call it off the UI thread.
pub fn fetch_external(source: &SecretSource) -> Result<String> {
    let (program, args): (&str, Vec<String>) = match source {
        SecretSource::Bitwarden { item } => ("bw", vec!["get".into(), "password".into(), item.clone()]),
        SecretSource::Rbw { item } => ("rbw", vec!["get".into(), item.clone()]),
        SecretSource::OnePassword { reference } => ("op", vec!["read".into(), reference.clone()]),
        SecretSource::Command { command } => ("sh", vec!["-c".into(), command.clone()]),
        SecretSource::None | SecretSource::Vault => bail!("not an external source"),
    };
    let output = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("could not run `{program}` (is it installed and in your PATH?)"))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let hint = match source {
            SecretSource::Bitwarden { .. } => " — did you run `bw unlock` and export BW_SESSION?",
            SecretSource::Rbw { .. } => " — did you run `rbw unlock`? If login fails with \"new device verification\", run `rbw register`",
            SecretSource::OnePassword { .. } => " — did you run `op signin`?",
            _ => "",
        };
        bail!("`{program}` failed: {}{hint}", err.trim());
    }
    let mut out = String::from_utf8(output.stdout).context("the output is not UTF-8")?;
    while out.ends_with('\n') || out.ends_with('\r') {
        out.pop();
    }
    if out.is_empty() {
        bail!("`{program}` returned nothing");
    }
    Ok(out)
}
