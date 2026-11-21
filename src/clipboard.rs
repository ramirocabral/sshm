use std::io::Write;
use std::process::{Command, Stdio};

use base64::Engine;

use crate::paths::in_path;

/// Copies via OSC 52 (works over SSH and in most terminals) and, when available, a local tool.
pub fn copy(text: &str, override_cmd: Option<&str>) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07");
    let _ = out.flush();

    let cmd = override_cmd.map(str::to_string).or_else(detect_tool);
    if let Some(cmd) = cmd {
        let mut parts = cmd.split_whitespace();
        let Some(program) = parts.next() else { return };
        if let Ok(mut child) = Command::new(program).args(parts).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

fn detect_tool() -> Option<String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let x11 = std::env::var_os("DISPLAY").is_some();
    if wayland && in_path("wl-copy") {
        return Some("wl-copy".into());
    }
    if x11 && in_path("xclip") {
        return Some("xclip -selection clipboard".into());
    }
    if x11 && in_path("xsel") {
        return Some("xsel --clipboard --input".into());
    }
    None
}
