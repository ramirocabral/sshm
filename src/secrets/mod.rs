pub mod keyring;
pub mod vault;

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::store::hosts::SecretSource;
pub use vault::{SecretKind, Vault};

/// How long an external password manager may take before it is killed.
pub const EXTERNAL_TIMEOUT: Duration = Duration::from_secs(60);

/// Fetches a secret from an external password manager. Blocking: call it off the UI thread.
pub fn fetch_external(source: &SecretSource) -> Result<String> {
    fetch_external_within(source, EXTERNAL_TIMEOUT)
}

fn fetch_external_within(source: &SecretSource, timeout: Duration) -> Result<String> {
    let (program, args): (&str, Vec<String>) = match source {
        SecretSource::Bitwarden { item } => ("bw", vec!["get".into(), "password".into(), item.clone()]),
        SecretSource::Rbw { item } => ("rbw", vec!["get".into(), item.clone()]),
        SecretSource::OnePassword { reference } => ("op", vec!["read".into(), reference.clone()]),
        SecretSource::Command { command } => ("sh", vec!["-c".into(), command.clone()]),
        SecretSource::None | SecretSource::Vault | SecretSource::Keyring => bail!("not an external source"),
    };
    let output = run_with_timeout(Command::new(program).args(&args), None, timeout)
        .with_context(|| format!("could not run `{program}` (is it installed and in your PATH?)"))?;
    let Some(output) = output else {
        bail!("`{program}` did not answer within {} s and was stopped", timeout.as_secs());
    };
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

fn kill_group(pid: u32) {
    // SAFETY: kill has no memory preconditions; the child leads its own process group.
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

fn reader(mut r: impl Read + Send + 'static, tx: mpsc::Sender<(bool, Vec<u8>)>, is_err: bool) {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = r.read_to_end(&mut buf);
        let _ = tx.send((is_err, buf));
    });
}

/// Runs `cmd` with `input` on stdin (or none) and both outputs captured. Returns Ok(None) when
/// it did not finish within `timeout`; then it is killed along with anything it started.
pub(crate) fn run_with_timeout(cmd: &mut Command, input: Option<&[u8]>, timeout: Duration) -> std::io::Result<Option<Output>> {
    let deadline = Instant::now() + timeout;
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = cmd.spawn()?;
    let (tx, rx) = mpsc::channel();
    if let Some(out) = child.stdout.take() {
        reader(out, tx.clone(), false);
    }
    if let Some(err) = child.stderr.take() {
        reader(err, tx.clone(), true);
    }
    drop(tx);
    // Small inputs (a password) fit in the pipe buffer, so writing before reading cannot block.
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take())
        && let Err(e) = stdin.write_all(data)
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        kill_group(child.id());
        let _ = child.wait();
        return Err(e);
    }
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let Some(status) = status else {
        kill_group(child.id());
        let _ = child.kill();
        let _ = child.wait();
        return Ok(None);
    };
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    for _ in 0..2 {
        // Something it left running may still hold the pipes: stop the group once time is up.
        let got = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(v) => Some(v),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                kill_group(child.id());
                rx.recv_timeout(Duration::from_millis(500)).ok()
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match got {
            Some((true, b)) => stderr = b,
            Some((false, b)) => stdout = b,
            None => return Ok(None),
        }
    }
    Ok(Some(Output { status, stdout, stderr }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn captures_both_outputs_and_input() {
        let out = run_with_timeout(&mut sh("cat; echo oops >&2"), Some(b"p4ss"), Duration::from_secs(5)).unwrap().unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout, b"p4ss");
        assert_eq!(out.stderr, b"oops\n");
    }

    #[test]
    fn large_output_does_not_deadlock() {
        let out = run_with_timeout(&mut sh("head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2"), None, Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert_eq!((out.stdout.len(), out.stderr.len()), (300_000, 300_000));
    }

    #[test]
    fn hung_commands_are_stopped() {
        let start = Instant::now();
        assert!(run_with_timeout(&mut sh("sleep 30"), None, Duration::from_millis(300)).unwrap().is_none());
        assert!(run_with_timeout(&mut sh("echo hi; sleep 30 &"), None, Duration::from_millis(300)).unwrap().is_some());
        assert!(start.elapsed() < Duration::from_secs(5));
        let err = fetch_external_within(&SecretSource::Command { command: "sleep 30".into() }, Duration::from_millis(200)).unwrap_err();
        assert!(err.to_string().contains("did not answer"), "{err}");
    }

    #[test]
    fn command_source_output() {
        let src = SecretSource::Command { command: "printf 's3cret\\n'".into() };
        assert_eq!(fetch_external(&src).unwrap(), "s3cret");
        assert!(fetch_external(&SecretSource::Command { command: "exit 3".into() }).is_err());
    }
}
