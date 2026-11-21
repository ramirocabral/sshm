use std::path::{Path, PathBuf};

use crate::store::config::Config;
use crate::store::hosts::Host;

pub const ENV_SOCK: &str = "SSHM_ASKPASS_SOCK";
pub const ENV_TOKEN: &str = "SSHM_ASKPASS_TOKEN";

pub struct AskPassEnv {
    pub socket: PathBuf,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub enum Purpose {
    Shell,
    Sftp,
    /// `ssh -N` that holds a host's tunnels, added and removed through its own control socket.
    Tunnel(PathBuf),
}

/// Options of a tunnel connection that must win over ssh_args and ~/.ssh/config. It is always
/// its own master (never a client of a session's), and forwards from the config are dropped:
/// they would clash with the sessions that already set them up.
fn tunnel_master_opts(socket: &Path) -> [String; 4] {
    ["ControlMaster=yes".into(), format!("ControlPath={}", socket.display()), "ControlPersist=no".into(), "ClearAllForwardings=yes".into()]
}

pub fn build(host: &Host, cfg: &Config, ask: Option<&AskPassEnv>, control_dir: Option<&Path>, purpose: Purpose) -> SshSpec {
    let mut args = Vec::new();
    if let Some(port) = host.port {
        args.push("-p".into());
        args.push(port.to_string());
    }
    if let Some(user) = host.user.as_ref().filter(|u| !u.is_empty()) {
        args.push("-l".into());
        args.push(user.clone());
    }
    if let Some(id) = host.identity_file.as_ref().filter(|i| !i.is_empty()) {
        args.push("-i".into());
        args.push(crate::paths::expand_tilde(id).to_string_lossy().into_owned());
    }
    if let Some(jump) = host.jump.as_ref().filter(|j| !j.is_empty()) {
        args.push("-J".into());
        args.push(jump.clone());
    }
    if let Purpose::Tunnel(socket) = &purpose {
        for o in tunnel_master_opts(socket) {
            args.push("-o".into());
            args.push(o);
        }
    } else if let Some(dir) = control_dir.filter(|_| cfg.control_master) {
        args.push("-o".into());
        args.push("ControlMaster=auto".into());
        args.push("-o".into());
        args.push(format!("ControlPath={}/cm-%C", dir.display()));
        args.push("-o".into());
        args.push(format!("ControlPersist={}", cfg.control_persist));
    }
    args.extend(host.ssh_args.iter().cloned());
    match purpose {
        Purpose::Shell => args.push(host.host.clone()),
        Purpose::Sftp => {
            args.push("-T".into());
            args.push("-s".into());
            args.push(host.host.clone());
            args.push("sftp".into());
        }
        Purpose::Tunnel(_) => {
            // After ssh_args, so a keepalive set there wins; this one notices a dead link in ~45 s.
            for o in ["ServerAliveInterval=15", "ServerAliveCountMax=3"] {
                args.push("-o".into());
                args.push(o.into());
            }
            args.push("-N".into());
            args.push(host.host.clone());
        }
    }

    let mut env = Vec::new();
    if let Some(ask) = ask {
        let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "sshm".into());
        env.push(("SSH_ASKPASS".into(), exe));
        env.push(("SSH_ASKPASS_REQUIRE".into(), "force".into()));
        env.push((ENV_SOCK.into(), ask.socket.to_string_lossy().into_owned()));
        env.push((ENV_TOKEN.into(), ask.token.clone()));
    }
    SshSpec { program: cfg.ssh_command.clone(), args, env }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxOp {
    Forward,
    Cancel,
}

/// `ssh -O forward|cancel` on a tunnel connection's socket. It reads no config (-F /dev/null):
/// ssh would otherwise send the host's forwards from ~/.ssh/config along with ours.
pub fn mux(cfg: &Config, socket: &Path, op: MuxOp, forward: &[String]) -> SshSpec {
    let op = match op {
        MuxOp::Forward => "forward",
        MuxOp::Cancel => "cancel",
    };
    let mut args: Vec<String> = vec!["-F".into(), "/dev/null".into(), "-S".into(), socket.display().to_string(), "-O".into(), op.into()];
    args.extend(forward.iter().cloned());
    args.push("sshm".into());
    SshSpec { program: cfg.ssh_command.clone(), args, env: Vec::new() }
}

/// What ssh asked for through SSH_ASKPASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    Password,
    Passphrase,
    Confirm,
    Other,
}

pub fn classify_prompt(prompt: &str) -> PromptKind {
    let p = prompt.to_lowercase();
    if p.contains("passphrase") {
        PromptKind::Passphrase
    } else if p.contains("(yes/no") || p.contains("continue connecting") || p.contains("are you sure") {
        PromptKind::Confirm
    } else if p.contains("password") || p.contains("contraseña") {
        PromptKind::Password
    } else {
        PromptKind::Other
    }
}

/// Password prompts printed by programs inside a session (sudo, su…).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPrompt {
    Sudo,
    Generic,
}

pub fn detect_session_prompt(line: &str) -> Option<SessionPrompt> {
    let t = line.trim_end();
    if !t.ends_with(':') {
        return None;
    }
    let l = t.to_lowercase();
    if l.contains("[sudo]") || l.starts_with("sudo password") || l.contains("contraseña para") && l.contains("sudo") {
        return Some(SessionPrompt::Sudo);
    }
    if l.ends_with("password:") || l.ends_with("contraseña:") {
        return Some(SessionPrompt::Generic);
    }
    None
}

/// A shell prompt at the end of the cursor line (used to send `on_connect`).
pub fn looks_like_shell_prompt(line: &str) -> bool {
    let t = line.trim_end_matches(' ');
    line.ends_with(' ') && (t.ends_with('$') || t.ends_with('#') || t.ends_with('>') || t.ends_with('%'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_args() {
        let host = Host {
            host: "10.0.0.5".into(),
            port: Some(2222),
            user: Some("admin".into()),
            jump: Some("bastion".into()),
            ssh_args: vec!["-o".into(), "ServerAliveInterval=30".into()],
            ..Host::default()
        };
        let cfg = Config::default();
        let ask = AskPassEnv { socket: "/run/x.sock".into(), token: "tok".into() };
        let spec = build(&host, &cfg, Some(&ask), Some(Path::new("/run/sshm")), Purpose::Shell);
        assert_eq!(spec.program, "ssh");
        assert_eq!(
            spec.args,
            vec![
                "-p",
                "2222",
                "-l",
                "admin",
                "-J",
                "bastion",
                "-o",
                "ControlMaster=auto",
                "-o",
                "ControlPath=/run/sshm/cm-%C",
                "-o",
                "ControlPersist=60",
                "-o",
                "ServerAliveInterval=30",
                "10.0.0.5"
            ]
        );
        assert!(spec.env.iter().any(|(k, v)| k == "SSH_ASKPASS_REQUIRE" && v == "force"));
        assert!(spec.env.iter().any(|(k, v)| k == ENV_TOKEN && v == "tok"));

        let sftp = build(&host, &cfg, None, None, Purpose::Sftp);
        assert_eq!(&sftp.args[sftp.args.len() - 4..], &["-T", "-s", "10.0.0.5", "sftp"]);
        assert!(sftp.env.is_empty());
    }

    #[test]
    fn tunnel_connection_and_mux_args() {
        let host = Host { host: "bastion".into(), ssh_args: vec!["-o".into(), "ServerAliveInterval=5".into()], ..Host::default() };
        let cfg = Config::default();
        let spec = build(&host, &cfg, None, Some(Path::new("/run/sshm")), Purpose::Tunnel("/run/sshm/tun-1-2.sock".into()));
        assert_eq!(
            spec.args,
            vec![
                "-o",
                "ControlMaster=yes",
                "-o",
                "ControlPath=/run/sshm/tun-1-2.sock",
                "-o",
                "ControlPersist=no",
                "-o",
                "ClearAllForwardings=yes",
                "-o",
                "ServerAliveInterval=5",
                "-o",
                "ServerAliveInterval=15",
                "-o",
                "ServerAliveCountMax=3",
                "-N",
                "bastion"
            ],
            "no shared ControlMaster, and the host's own keepalive comes first"
        );

        let fwd = mux(&cfg, Path::new("/run/sshm/tun-1-2.sock"), MuxOp::Forward, &["-L".into(), "5432:db:5432".into()]);
        assert_eq!(fwd.args, ["-F", "/dev/null", "-S", "/run/sshm/tun-1-2.sock", "-O", "forward", "-L", "5432:db:5432", "sshm"]);
        assert_eq!(mux(&cfg, Path::new("/s"), MuxOp::Cancel, &[]).args[5], "cancel");
    }

    #[test]
    fn prompts() {
        assert_eq!(classify_prompt("admin@10.0.0.5's password: "), PromptKind::Password);
        assert_eq!(classify_prompt("(admin@host) Password: "), PromptKind::Password);
        assert_eq!(classify_prompt("Enter passphrase for key '/home/u/.ssh/id_ed25519': "), PromptKind::Passphrase);
        assert_eq!(
            classify_prompt(
                "The authenticity of host 'x' can't be established.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? "
            ),
            PromptKind::Confirm
        );
        assert_eq!(classify_prompt("Verification code: "), PromptKind::Other);

        assert_eq!(detect_session_prompt("[sudo] password for ramiro: "), Some(SessionPrompt::Sudo));
        assert_eq!(detect_session_prompt("[sudo] contraseña para ramiro: "), Some(SessionPrompt::Sudo));
        assert_eq!(detect_session_prompt("Password: "), Some(SessionPrompt::Generic));
        assert_eq!(detect_session_prompt("Contraseña:"), Some(SessionPrompt::Generic));
        assert_eq!(detect_session_prompt("user@host:~$ "), None);
        assert!(looks_like_shell_prompt("user@host:~$ "));
        assert!(looks_like_shell_prompt("[root@db ~]# "));
        assert!(!looks_like_shell_prompt("Password: "));
    }
}
