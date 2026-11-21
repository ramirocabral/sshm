use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as TokioBufReader};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

use super::command::{ENV_SOCK, ENV_TOKEN};

const ACCEPT_BACKOFF_MIN: Duration = Duration::from_millis(50);
const ACCEPT_BACKOFF_MAX: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    prompt: String,
}

#[derive(Serialize, Deserialize)]
struct Response {
    answer: Option<String>,
}

pub struct AskPassRequest {
    pub token: String,
    pub prompt: String,
    pub reply: oneshot::Sender<Option<String>>,
}

/// True when this process was started by ssh as SSH_ASKPASS.
pub fn invoked_as_askpass() -> bool {
    std::env::var_os(ENV_SOCK).is_some() && std::env::var_os(ENV_TOKEN).is_some()
}

/// Client side: forwards ssh's prompt to the running TUI and prints the answer for ssh.
pub fn run_client() -> i32 {
    let prompt = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    match client(&prompt) {
        Ok(Some(answer)) => {
            let mut out = std::io::stdout();
            let _ = writeln!(out, "{answer}");
            0
        }
        Ok(None) => 1,
        Err(e) => {
            eprintln!("sshm askpass: {e:#}");
            1
        }
    }
}

fn client(prompt: &str) -> Result<Option<String>> {
    let sock = std::env::var(ENV_SOCK)?;
    let token = std::env::var(ENV_TOKEN)?;
    let mut stream = UnixStream::connect(&sock).with_context(|| format!("connecting to {sock}"))?;
    let req = serde_json::to_string(&Request { token, prompt: prompt.to_string() })?;
    stream.write_all(req.as_bytes())?;
    stream.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    let resp: Response = serde_json::from_str(line.trim()).context("invalid askpass response")?;
    Ok(resp.answer)
}

/// Server side: one socket per running instance; each request is handed to the app loop.
pub fn start_server(dir: &std::path::Path, tx: mpsc::UnboundedSender<AskPassRequest>) -> Result<PathBuf> {
    remove_stale_sockets(dir);
    let path = dir.join(format!("askpass-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("could not create {}", path.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok();
    }
    tokio::spawn(async move {
        let mut backoff = ACCEPT_BACKOFF_MIN;
        loop {
            let stream = match listener.accept().await {
                Ok((stream, _)) => {
                    backoff = ACCEPT_BACKOFF_MIN;
                    stream
                }
                Err(e) => {
                    // E.g. EMFILE: retrying at once would spin.
                    if backoff == ACCEPT_BACKOFF_MIN {
                        tracing::warn!("askpass socket: accept failed: {e}");
                    }
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(ACCEPT_BACKOFF_MAX);
                    continue;
                }
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = TokioBufReader::new(read).lines();
                let Ok(Some(line)) = lines.next_line().await else { return };
                let Ok(req) = serde_json::from_str::<Request>(&line) else { return };
                let (reply_tx, reply_rx) = oneshot::channel();
                if tx.send(AskPassRequest { token: req.token, prompt: req.prompt, reply: reply_tx }).is_err() {
                    return;
                }
                let answer = reply_rx.await.unwrap_or(None);
                let resp = serde_json::to_string(&Response { answer }).unwrap_or_else(|_| "{}".into());
                let _ = write.write_all(resp.as_bytes()).await;
                let _ = write.write_all(b"\n").await;
            });
        }
    });
    Ok(path)
}

/// Removes sockets left behind by instances that did not exit cleanly.
fn remove_stale_sockets(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("askpass-").and_then(|r| r.strip_suffix(".sock")) else { continue };
        if pid.parse::<u32>().is_ok() && !std::path::Path::new("/proc").join(pid).exists() {
            let _ = std::fs::remove_file(e.path());
        }
    }
}
