# CLAUDE.md

sshm: a full-TUI SSH session manager in Rust (MobaXterm/Ásbrú-like, but in the terminal).
See README.md for the user-facing feature set.

## Hard requirements

- **Everything user-facing is in English**: UI strings, errors, warnings, CLI help, README.
  Code comments too. (The only Spanish strings allowed are the patterns that detect
  Spanish-locale password prompts in `ssh/command.rs`.)
- **Any terminal emulator, any Linux distro.** No kitty/tmux/sway-only features without a
  fallback (keyboard enhancement is enabled only when the terminal supports it; clipboard is
  OSC 52 plus optional wl-copy/xclip/xsel). Dependencies must be pure Rust — no system libs
  (no dbus/keyring, no OpenSSL). The release binary must only link libc/libm/libgcc.
- **`install.sh` is POSIX sh and never installs packages**: it checks dependencies and prints a
  per-distro hint. Keep it passing `sh -n` (and `dash -n`/shellcheck when available).
- **Vim keys everywhere.** Every list/view must be navigable with hjkl/gg/G/Ctrl+d/u; forms use
  NORMAL/INSERT modes (`app/form.rs::Mode`), and text-filtered lists move with Ctrl+j/k
  (also Ctrl+n/p). Never rely on a single Ctrl+letter for an essential action: terminals grab
  some (kitty users often map Ctrl+S) — forms have Save/Cancel buttons for that reason.
- MSRV is set by the dependencies (currently 1.89, `rust-version` in Cargo.toml and
  `MIN_RUST` in install.sh — keep both in sync when bumping crates).

## Commands

```sh
cargo test                                   # unit tests, fixtures in tests/fixtures/
cargo clippy --all-targets -- -D warnings    # must stay clean
cargo fmt --check                            # rustfmt.toml: max_width 140
docker compose -f dev/docker-compose.yml -p sshm-dev up -d   # ssh test servers 2221-2223 (ssh3 refuses port forwarding)
```

Interactive testing without a real terminal: run the binary inside a private tmux server and
drive it with `send-keys` / `capture-pane` (`tmux -L sshmtest -f /dev/null ...`: a user tmux.conf can leave the pane in view-mode and swallow the keys). Point `XDG_CONFIG_HOME`,
`XDG_DATA_HOME` and `XDG_STATE_HOME` at a scratch dir so the real hosts/vault are untouched,
and give test hosts `-o UserKnownHostsFile=<scratch>` in `ssh_args`. Mouse events can be
injected as raw SGR sequences with `send-keys -l`.

## Architecture

- **Transport is the system `ssh`**, never an SSH library. Sessions are `ssh <host>` in a PTY
  (`portable-pty`); SFTP is `ssh -T -s <host> sftp` with piped stdio spoken to by
  `russh-sftp` (`SftpSession::new` over `tokio::io::join(stdout, stdin)`). Both get
  `ControlMaster=auto` + `ControlPath=$runtime/cm-%C` so SFTP and duplicated panes reuse
  the authenticated connection (`ssh/command.rs::build`).
- **Passwords at connect time** go through `SSH_ASKPASS` = our own binary with
  `SSH_ASKPASS_REQUIRE=force`. `main()` checks `ssh::askpass::invoked_as_askpass()` (env
  `SSHM_ASKPASS_SOCK`/`SSHM_ASKPASS_TOKEN`) before clap. The client connects to the TUI's Unix
  socket; the TUI maps the token to an `Owner` (host + attempt counters) in `App::owners`
  and answers via `with_secret` → `fulfill`. A rejected stored password is never retried.
- **Terminal emulation**: `alacritty_terminal::Term` + `vte` parser per pane
  (`term/pane.rs`). A reader thread per PTY parses output under a mutex and sends one
  `PaneNotify::Output` per batch (atomic dirty flag). Input goes through a queue to a writer
  thread on the non-blocking PTY fd, so `TermPane::write` never blocks the UI; terminal replies
  (DSR, DA…) from the `EventListener` (`Event::PtyWrite`) use the same queue. Rendering converts the grid into a
  ratatui `Buffer` (`term/render.rs`); keys are re-encoded as xterm bytes (`term/keys.rs`),
  mouse as SGR/X10 (`term/mouse.rs`).
- **App loop** (`app/mod.rs::run`): one tokio mpsc of `AppEvent` (input, pane notifications,
  askpass requests, SFTP events, `Apply`, ticks); redraws are capped at ~60 fps. Nothing
  blocking runs in `App::handle`: background work goes through `App::spawn_job` and comes back
  as `AppEvent::Apply` (a closure run on the UI thread); vault writes go to the FIFO
  `jobs::VaultWriter`. The terminal must be initialised (`tui::enter`) *before* the crossterm
  `EventStream` task starts, or the stream swallows the cursor-position reply.
- **Keys** (`app/keymap.rs`): every `Action` with its keys and description per context
  (leader, Alt, host list, visual). Dispatch, help, status hints, the welcome screen and the
  palette all read `BINDINGS`; add a binding there, never as a bare match. Key handling is
  in `app/input/`, what actions do in `app/actions.rs` and `app/hosts.rs`. SFTP keys live in
  `app/sftp.rs`.
- **Forms** (`app/form.rs` state + keys, `ui/form.rs` rendering) are generic: fields are
  Text, Secret, Toggle, Select (closed list) or Combo (free text + suggestions), grouped in
  sections, with per-field hints and a popup list. Anything with a fixed set of values must
  be a Select, not free text. Owners (`HostForm`, `ImportForm`, `ExportForm` in
  `app/dialogs.rs`) react to `FormEvent::Changed` to show/hide dependent fields;
  `Dialog::form_mut` gives every form dialog the same key/paste/mouse path.
- **Dialogs** are a queue (`App::dialogs`, front is shown) because several askpass prompts can
  arrive at once (grid of hosts). `dialog_key` pops the front and pushes it back unless
  the key closes it. Once an unlock succeeds, the other queued unlock prompts proceed too.
- **Secrets** (`secrets/`): `Vault` is a JSON map host-id → {password, root_password,
  passphrase} encrypted with age scrypt (work factor 2^17), opened with `Vault::open` →
  `install` and written with `save_job` → `SaveJob::run` → `apply_saved` (the blocking halves
  run off the UI thread; the sync `unlock`/`save` are for the CLI). `SecretSource` per host is a
  string in hosts.toml: `keyring`, `vault`, `none`, `bw:`, `rbw:`, `op://`, `cmd:`. External
  sources run in `spawn_blocking`. `keyring` (`secrets/keyring.rs`) shells out to libsecret's
  `secret-tool` instead of linking D-Bus; `save_secrets` routes each secret to the keyring or
  the vault by the host's source. External tools are killed after a timeout
  (`secrets::run_with_timeout`).
- **Tunnels** (`app/tunnels.rs`, stored in `Host::tunnels`): the running tunnels of a host share
  one `ssh -N` (`Purpose::Tunnel`: its own `ControlMaster=yes` socket `tun-<pid>-<n>.sock`, and
  `ClearAllForwardings` so forwards from ~/.ssh/config do not clash with the sessions'). Each
  tunnel is added/removed on it with `command::mux` (`ssh -F /dev/null -S sock -O forward|cancel`),
  so they toggle one by one with a single authentication. The socket appearing means it
  authenticated. A connection that was up and drops retries with backoff; one that never came up,
  or got "Permission denied", fails its tunnels. Local ports are test-bound first because ssh
  reports success when it could bind only ::1. The ssh gets PR_SET_PDEATHSIG and SIGTERM on quit.
- **Import/export** (`import/`): `import()` → `ImportReport`, `merge()` into the store (does
  not save; callers save hosts.toml and the vault), `export()`. Third-party password
  decryption (MobaXterm AES-CFB8, Ásbrú Blowfish/Crypt::CBC, Remmina 3DES) is verified
  against published vectors in the unit tests.

## Data

hosts.toml (no secrets) and vault.age in `$XDG_DATA_HOME/sshm`, config.toml in
`$XDG_CONFIG_HOME/sshm`, log and state.toml (UI state: open folders) in `$XDG_STATE_HOME/sshm`, sockets in
`$XDG_RUNTIME_DIR/sshm` (fallback `/tmp/sshm-<uid>`, 0700). Writes go through
`paths::write_private` (unique hidden tmp + rename, 0600; only sshm's own dirs are chmod'ed).
Several instances can run at once: hosts.toml and the vault keep the version they last read
or wrote and merge three-way by host id under `.<file>.lock` when the file changed on disk;
the tick reloads hosts.toml when another instance saved it.
