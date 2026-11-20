# sshm

A terminal SSH session manager in the spirit of MobaXterm or Ásbrú, but fully in the terminal:

- **Host book** with folders, fuzzy search and tags, stored in a plain `hosts.toml`.
- **Saved passwords** in an encrypted vault (age + scrypt). The vault answers ssh's password
  prompt on connect, and it can type the password into `sudo`/`su` prompts later on. A
  password can also come from Bitwarden (`bw`/`rbw`), 1Password (`op`) or any command.
- **Keys just work**: every session is the system `ssh`, so `~/.ssh/config`, ProxyJump,
  ssh-agent, FIDO keys and known_hosts behave exactly as usual.
- **Several sessions at once**: tabs, an automatic grid of panes, and a broadcast mode that
  types into every pane of the tab, like MobaXterm's MultiExec. Each pane can be left out.
- **Built-in SFTP**: a two-panel browser (local | remote) with uploads, downloads, rename,
  mkdir, recursive delete and a transfer queue with progress. It reuses the session's
  connection, so it does not ask for the password again.
- **Import / export**: `~/.ssh/config`, MobaXterm, Ásbrú, Remmina and sshm's own JSON/CSV.
  Passwords come along when the source has them. An export can be an encrypted bundle.

It runs in any terminal emulator on any Linux distribution.

## Install

```sh
git clone <this repo> sshm && cd sshm
./install.sh            # builds and installs ~/.local/bin/sshm
```

`install.sh` never installs packages. It checks what is needed and prints the command for
your distro when something is missing:

| Needed for | Dependency |
|---|---|
| building | Rust 1.89 or newer (`cargo`); [rustup](https://rustup.rs) works everywhere |
| running | OpenSSH client 8.4 or newer (`ssh`) |
| optional: clipboard | `wl-copy`, `xclip` or `xsel`; OSC 52 is always used too |
| optional: external passwords | `bw`, `rbw` or `op` |

Use `PREFIX=/some/dir ./install.sh` to install somewhere else. `./install.sh --uninstall`
removes the binary and keeps your data.

## Quick start

```sh
sshm import ssh-config ~/.ssh/config   # optional: bring your hosts in
sshm                                   # open the TUI
```

In the host list, `n` creates a host and `Enter` connects. `Space` marks several hosts (or `v`
selects a range, like vim's visual mode), and `Enter` then opens them all in a grid. Inside a
session, everything goes to the remote side except the **leader key**, `Ctrl+Space` by
default. `Ctrl+Space ?` shows every binding.

### Key bindings

Inside a session, press the leader key first:

| Keys | Action |
|---|---|
| `Tab` | focus the host list / back |
| `\` | show / hide the host list |
| `1`..`9` (or `Alt+1`..`9`) | go to tab N |
| `H` / `L` | previous / next tab |
| `h j k l` (or `Alt+arrows`) | move between panes |
| `z` | zoom the current pane |
| `d` | duplicate the session into the current tab |
| `b` | broadcast on/off for the tab |
| `x` | leave the current pane out of the broadcast |
| `p` / `P` | type the host password / the root password |
| `s` | SFTP to the current host |
| `[` | copy mode (vi keys: `v`, `V`, `Ctrl+v`, `y`) |
| `r` | reconnect |
| `w` | close the pane |
| `W` | close the whole tab, without asking |
| `t` | local shell |
| `n` / `e` | new host / edit the current host |
| `i` | import |
| `:` | command palette |
| leader twice | send the leader key to the session |
| `q` | quit |

Keys in the host list:

| Keys | Action |
|---|---|
| `j`/`k`, `g`/`G` | move |
| `h`/`l` | collapse / expand a folder |
| `Enter` | connect, or open every marked host in a grid |
| `o` | open in the current tab |
| `Space` | mark a host (on a folder: mark all its hosts) |
| `v` / `V` | visual mode: `j`/`k` extend the range, `Enter` opens it in a grid, `o` in the current tab, `Space` marks it, `m` moves it, `d` deletes it, `Esc` cancels |
| `/` | fuzzy search |
| `n`, `e`, `c`, `d` | new, edit, duplicate, delete |
| `e` on a folder | rename it |
| `m` | move the marked hosts, or the folder or host under the cursor, to another folder (picked from a list, or a new one) |
| `s` | SFTP |
| `y` | copy `user@host` |
| `i` / `x` | import / export |

Mouse: drag to select and copy (hold Shift when the remote app grabs the mouse). Double
click selects a word, triple click a line. The wheel scrolls the scrollback. Click a tab to
switch to it; a middle click closes it.

Everything can be driven with vim keys. Forms (new/edit host, import, export) have vim-like
modes:

- **NORMAL**:
  - `j`/`k` move between fields, and `gg`/`G` jump to the first field or the Save button.
  - `h`/`l` change a choice or flip a switch.
  - `i`/`a`/`I`/`A`/`Enter` edit a text field, and `S` clears it and edits.
  - `D` clears a field; on a password it deletes the saved one.
  - `q`/`Esc` cancel, and ask first if there are unsaved changes.
- **INSERT**: you type, and `Esc` goes back to NORMAL.
- **Both modes**: `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`), the arrows and `Tab` move
  between fields.

`Enter` on a choice opens its list, where `j`/`k` move (`Ctrl+j`/`Ctrl+k` while typing a
filter). Folder, jump host and private key accept free text plus suggestions: your folders,
your hosts and the keys in `~/.ssh`.

Saving works with the **Save** button or `Ctrl+S`. Some terminals grab `Ctrl+S` (kitty
setups often map it), so the button always works. The mouse works too.

In the help (`leader ?`), `/` searches the shortcuts, and `j`/`k`, `g`/`G` and
`Ctrl+d`/`Ctrl+u` scroll. The palette and the host search move with `Ctrl+j`/`Ctrl+k`.
`leader H`/`L` switch tabs.

In the SFTP view: `Tab` switches side, `Enter` opens, `h` goes up, `Space` marks. `F5`/`c`
copies to the other side, `F6`/`m` renames, `F7`/`n` creates a folder, `F8`/`d` deletes,
`/` jumps to a path, `.` toggles hidden files and `r` refreshes.

## Passwords

- **Storage.** Secrets live in `~/.local/share/sshm/vault.age`, a standard
  [age](https://age-encryption.org) file encrypted with your master password (scrypt). You
  can open it without sshm: `age -d vault.age`. The vault is unlocked the first time a
  secret is needed. `vault_timeout` in the config locks it again after some idle time.
- **Connecting.** sshm sets `SSH_ASKPASS` to itself. When ssh asks for a password, the
  request goes over a private Unix socket (mode 0600, inside a 0700 runtime dir), tagged
  with a per-session random token. The stored password is sent once; if the server
  rejects it, sshm asks you instead of retrying, so accounts don't get locked.
  Unknown host keys and OTP prompts show up as dialogs.
- **Inside a session.** sshm watches the line under the cursor for `[sudo] password for`,
  `Password:` and the like. When one shows up, `leader p` types the password (`leader P`
  types the root password). With **Auto sudo** turned on for a host, sudo prompts are
  answered automatically, once per prompt; if sudo asks again right away, sshm stops and
  tells you.
- **Per-host source.** Set the "Password source" field to one of:
  - `vault` (the default);
  - `none`;
  - `bw:<item>` (Bitwarden CLI, needs `BW_SESSION`);
  - `rbw:<item>`;
  - `op://vault/item/field` (1Password CLI);
  - `cmd:<any command>`, whose stdout is the password.

## Import and export

```sh
sshm import ssh-config ~/.ssh/config
sshm import mobaxterm MobaXterm.ini        # asks for the MobaXterm master password
sshm import asbru ~/.config/asbru/asbru.yml
sshm import remmina ~/.local/share/remmina [--remmina-pref ~/.config/remmina/remmina.pref]
sshm import json hosts.json
sshm import csv hosts.csv
sshm import bundle backup.age              # add --conflict skip|overwrite|rename to any import

sshm export --format bundle -o backup.age  # hosts + passwords, encrypted
sshm export --format json -o hosts.json    # plain text (passwords included unless --no-secrets)
sshm export --format csv -o hosts.csv
```

The same options are in the TUI: `i` and `x` in the host list, or the command palette.
A bundle is the easy way to move everything to another machine.

Notes on the third-party formats:

- **MobaXterm.** Passwords are decrypted from `[Passwords]` with the master password, which
  works up to MobaXterm 24. Version 25 changed the scheme, so those passwords are skipped
  with a warning.
- **Ásbrú.** Passwords use Ásbrú's fixed key.
- **Remmina.** Passwords use the `secret` from `remmina.pref`. Passwords kept in the system
  keyring (`password=.`) can't be read.

## Configuration

`~/.config/sshm/config.toml` (`sshm config` prints the defaults):

```toml
leader = "ctrl+space"      # e.g. "ctrl+a", "ctrl+b", "f12"
alt_shortcuts = true       # Alt+1..9 and Alt+arrows without the leader
control_master = true      # one connection per host, shared by panes and SFTP
control_persist = 60
scrollback = 10000
sidebar_width = 32
ssh_command = "ssh"
term = "xterm-256color"    # TERM inside the sessions
vault_timeout = 0          # seconds of inactivity before locking (0 = never)
# clipboard_command = "wl-copy"
```

Files:

| File | Contents |
|---|---|
| `~/.local/share/sshm/hosts.toml` | hosts, no secrets |
| `~/.local/share/sshm/vault.age` | encrypted vault |
| `~/.local/state/sshm/sshm.log` | log |
| `~/.local/state/sshm/state.toml` | which folders were open in the host list |
| `$XDG_RUNTIME_DIR/sshm/` | sockets; falls back to `/tmp/sshm-<uid>` |

All of them follow the XDG variables.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
docker compose -f dev/docker-compose.yml -p sshm-dev up -d   # 3 test servers
```

The test servers listen on `127.0.0.1:2221`–`2223`, with user `tester` and password
`tester-pass`, which also works for sudo.
