mod app;
mod clipboard;
mod import;
mod paths;
mod secrets;
mod shell;
mod ssh;
mod store;
mod term;
mod tui;
mod ui;

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::secrets::Vault;
use crate::store::config::Config;
use crate::store::hosts::HostStore;

#[derive(Parser)]
#[command(name = "sshm", version, about = "SSH session manager for the terminal")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List saved hosts
    List,
    /// Import hosts (and passwords, when the format has them)
    Import {
        #[arg(value_enum)]
        source: import::Source,
        /// File (or directory, for Remmina)
        file: PathBuf,
        /// What to do when a host with the same folder and name already exists
        #[arg(long, value_enum, default_value = "skip")]
        conflict: import::Conflict,
        /// remmina.pref holding the key that decrypts Remmina passwords
        #[arg(long)]
        remmina_pref: Option<PathBuf>,
    },
    /// Export hosts
    Export {
        #[arg(long, value_enum, default_value = "bundle")]
        format: import::ExportFormat,
        /// Output file ("-" = stdout)
        #[arg(short, long)]
        output: PathBuf,
        /// Leave passwords out
        #[arg(long)]
        no_secrets: bool,
    },
    /// Show file locations and a sample config
    Config,
}

fn main() {
    if ssh::askpass::invoked_as_askpass() {
        std::process::exit(ssh::askpass::run_client());
    }
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("sshm: {e:#}");
        std::process::exit(1);
    }
}

fn init_logging() {
    let dir = paths::state_dir();
    if paths::ensure_private_dir(&dir).is_err() {
        return;
    }
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("sshm.log")) {
        let _ = tracing_subscriber::fmt().with_writer(std::sync::Mutex::new(file)).with_ansi(false).try_init();
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        None => {
            init_logging();
            let cfg = Config::load()?;
            check_ssh(&cfg)?;
            let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
            rt.block_on(app::run(cfg))
        }
        Some(Cmd::List) => {
            let store = HostStore::load_default()?;
            let mut out = std::io::stdout().lock();
            for h in &store.hosts {
                if writeln!(out, "{:<40} {}", h.path(), h.display_target()).is_err() {
                    break;
                }
            }
            Ok(())
        }
        Some(Cmd::Config) => {
            println!("config:  {}", Config::path().display());
            println!("hosts:   {}", HostStore::default_path().display());
            println!("vault:   {}", Vault::default_path().display());
            println!("log:     {}", paths::state_dir().join("sshm.log").display());
            println!("\n# sample config.toml (defaults)\n{}", Config::sample());
            Ok(())
        }
        Some(Cmd::Import { source, file, conflict, remmina_pref }) => {
            let needs_pass = matches!(source, import::Source::Bundle | import::Source::Mobaxterm);
            let passphrase = if needs_pass {
                let label = if source == import::Source::Bundle {
                    "Bundle password: "
                } else {
                    "MobaXterm master password (Enter if none): "
                };
                Some(prompt_hidden(label)?).filter(|p| !p.is_empty())
            } else {
                None
            };
            let opts = import::ImportOptions { passphrase, remmina_pref };
            let report = import::import(source, &paths::expand_tilde(&file.to_string_lossy()), &opts)?;
            for w in &report.warnings {
                eprintln!("warning: {w}");
            }
            let has_secrets = report.hosts.iter().any(|h| h.has_secrets());
            let mut store = HostStore::load_default()?;
            let mut vault = Vault::new(Vault::default_path());
            if has_secrets {
                let label = if vault.exists() { "Vault master password: " } else { "New vault master password: " };
                vault.unlock(&prompt_hidden(label)?)?;
            }
            let sum = import::merge(&mut store, has_secrets.then_some(&mut vault), report, conflict)?;
            store.save()?;
            if sum.secrets > 0 {
                vault.save()?;
            }
            println!(
                "added {}, updated {}, renamed {}, skipped {}, passwords {}",
                sum.added, sum.updated, sum.renamed, sum.skipped, sum.secrets
            );
            Ok(())
        }
        Some(Cmd::Export { format, output, no_secrets }) => {
            let store = HostStore::load_default()?;
            let mut vault = Vault::new(Vault::default_path());
            let with_secrets = !no_secrets && vault.exists();
            if with_secrets {
                vault.unlock(&prompt_hidden("Vault master password: ")?)?;
            }
            let pass = if format == import::ExportFormat::Bundle {
                let p = prompt_hidden("Password to encrypt the bundle: ")?;
                if p != prompt_hidden("Repeat it: ")? {
                    bail!("the passwords do not match");
                }
                Some(p)
            } else {
                if with_secrets {
                    eprintln!("warning: the file will contain passwords in plain text");
                }
                None
            };
            let data = import::export(&store, with_secrets.then_some(&vault), format, with_secrets, pass.as_deref())?;
            if output.as_os_str() == "-" {
                std::io::stdout().write_all(&data)?;
            } else {
                let path = paths::expand_tilde(&output.to_string_lossy());
                paths::write_private(&path, &data)?;
                eprintln!("{} hosts exported to {}", store.hosts.len(), path.display());
            }
            Ok(())
        }
    }
}

fn check_ssh(cfg: &Config) -> Result<()> {
    let out = std::process::Command::new(&cfg.ssh_command)
        .arg("-V")
        .output()
        .with_context(|| format!("`{}` not found: install the OpenSSH client", cfg.ssh_command))?;
    let version = String::from_utf8_lossy(&out.stderr).to_string() + &String::from_utf8_lossy(&out.stdout);
    if let Some(v) = parse_openssh_version(&version)
        && v < (8, 4) {
            bail!("OpenSSH 8.4 or newer is required (found {}.{}): sshm relies on SSH_ASKPASS_REQUIRE", v.0, v.1);
        }
    Ok(())
}

fn parse_openssh_version(s: &str) -> Option<(u32, u32)> {
    let rest = s.split("OpenSSH_").nth(1)?;
    let mut nums = rest.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    Some((nums.next()?.parse().ok()?, nums.next()?.parse().ok()?))
}

/// Reads a line from the terminal without echo.
fn prompt_hidden(label: &str) -> Result<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    eprint!("{label}");
    std::io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    let mut s = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break Err(anyhow::anyhow!("cancelled")),
                KeyCode::Backspace => {
                    s.pop();
                }
                KeyCode::Char(c) => s.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => s.push_str(&p),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    crossterm::terminal::disable_raw_mode()?;
    eprintln!();
    result.map(|_| s)
}

#[cfg(test)]
mod tests {
    #[test]
    fn openssh_version() {
        assert_eq!(super::parse_openssh_version("OpenSSH_10.6p2, OpenSSL 4.1.0"), Some((10, 6)));
        assert_eq!(super::parse_openssh_version("OpenSSH_8.4p1 Debian"), Some((8, 4)));
        assert_eq!(super::parse_openssh_version("garbage"), None);
    }
}
