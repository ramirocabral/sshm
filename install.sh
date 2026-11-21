#!/bin/sh
# Builds sshm from source and installs the binary for the current user.
# It never installs system packages: missing dependencies are reported with hints.
#
#   ./install.sh                  build and install to ~/.local/bin
#   PREFIX=/opt/sshm ./install.sh install to /opt/sshm/bin
#   ./install.sh --uninstall      remove the binary (your hosts and vault are kept)

set -eu

MIN_RUST="1.89"
MIN_OPENSSH="8.4"
PREFIX="${PREFIX:-$HOME/.local}"
BIN_DIR="$PREFIX/bin"
SRC_DIR=$(cd "$(dirname "$0")" && pwd)

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Prints "a" when version $1 >= $2 (both "major.minor").
version_ge() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        split(a, x, "."); split(b, y, ".");
        if (x[1] + 0 != y[1] + 0) exit !(x[1] + 0 > y[1] + 0);
        exit !(x[2] + 0 >= y[2] + 0);
    }'
}

pkg_manager() {
    for pm in pacman apt-get dnf zypper apk xbps-install emerge nix-env brew; do
        if have "$pm"; then
            echo "$pm"
            return
        fi
    done
    echo unknown
}

# Suggests the install command for a package on this distro (never runs it).
hint() {
    pkg="$1"
    case "$(pkg_manager)" in
        pacman) echo "sudo pacman -S $pkg" ;;
        apt-get) echo "sudo apt install $pkg" ;;
        dnf) echo "sudo dnf install $pkg" ;;
        zypper) echo "sudo zypper install $pkg" ;;
        apk) echo "sudo apk add $pkg" ;;
        xbps-install) echo "sudo xbps-install $pkg" ;;
        emerge) echo "sudo emerge $pkg" ;;
        nix-env) echo "nix-env -iA nixpkgs.$pkg" ;;
        *) echo "install '$pkg' with your package manager" ;;
    esac
}

rust_pkg() {
    case "$(pkg_manager)" in
        pacman | emerge) echo rust ;;
        *) echo cargo ;;
    esac
}

libsecret_pkg() {
    case "$(pkg_manager)" in
        apt-get | zypper) echo libsecret-tools ;;
        emerge) echo app-crypt/libsecret ;;
        *) echo libsecret ;;
    esac
}

cc_pkg() {
    case "$(pkg_manager)" in
        apt-get) echo build-essential ;;
        apk) echo build-base ;;
        emerge) echo sys-devel/gcc ;;
        *) echo gcc ;;
    esac
}

openssh_pkg() {
    case "$(pkg_manager)" in
        pacman | apk | xbps-install | zypper) echo openssh ;;
        apt-get) echo openssh-client ;;
        dnf) echo openssh-clients ;;
        emerge) echo net-misc/openssh ;;
        *) echo openssh ;;
    esac
}

check_deps() {
    missing=0

    if have cargo && have rustc; then
        rust=$(rustc --version | awk '{print $2}' | cut -d. -f1,2)
        if ! version_ge "$rust" "$MIN_RUST"; then
            warn "Rust $rust is too old (need $MIN_RUST or newer)."
            say "  Update it with 'rustup update' or install rustup from https://rustup.rs"
            missing=1
        fi
    else
        warn "Rust (cargo) not found: it is needed to build sshm."
        say "  Recommended: rustup from https://rustup.rs (works on every distro)"
        say "  Or: $(hint "$(rust_pkg)")  (the distro package must be $MIN_RUST or newer)"
        missing=1
    fi

    if ! have cc; then
        warn "no C linker (cc) found: Rust needs one to link sshm."
        say "  $(hint "$(cc_pkg)")"
        missing=1
    fi

    if have ssh; then
        ver=$(ssh -V 2>&1 | sed -n 's/.*OpenSSH_\([0-9][0-9]*\.[0-9][0-9]*\).*/\1/p')
        if [ -n "$ver" ] && ! version_ge "$ver" "$MIN_OPENSSH"; then
            warn "OpenSSH $ver is too old (need $MIN_OPENSSH or newer for SSH_ASKPASS_REQUIRE)."
            say "  $(hint "$(openssh_pkg)")"
            missing=1
        fi
    else
        warn "the OpenSSH client (ssh) was not found: sshm runs it for every session."
        say "  $(hint "$(openssh_pkg)")"
        missing=1
    fi

    if ! have wl-copy && ! have xclip && ! have xsel; then
        say "note: no wl-copy, xclip or xsel found. Copying still works through OSC 52 in terminals"
        say "      that support it; for the rest install one of them (optional)."
    fi
    if ! have secret-tool; then
        say "note: secret-tool not found. It lets hosts keep their passwords in the desktop keyring"
        say "      (GNOME Keyring, KWallet…); without it they go to sshm's vault. Optional:"
        say "      $(hint "$(libsecret_pkg)")"
    fi
    say "note: bw, rbw and op are optional; only needed for hosts that take passwords from"
    say "      Bitwarden or 1Password."

    return "$missing"
}

install_sshm() {
    say "Checking dependencies…"
    if ! check_deps; then
        die "install the missing dependencies above and run ./install.sh again."
    fi

    say "Building sshm (release)…"
    (cd "$SRC_DIR" && cargo build --release --locked)

    mkdir -p "$BIN_DIR"
    cp "$SRC_DIR/target/release/sshm" "$BIN_DIR/sshm.new"
    chmod 755 "$BIN_DIR/sshm.new"
    mv -f "$BIN_DIR/sshm.new" "$BIN_DIR/sshm"
    say "Installed $BIN_DIR/sshm"

    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) warn "$BIN_DIR is not in your PATH. Add this to your shell profile:"
           say "  export PATH=\"$BIN_DIR:\$PATH\"" ;;
    esac
    say "Run 'sshm' to start, or 'sshm import ssh-config ~/.ssh/config' to bring your hosts in."
}

uninstall_sshm() {
    if [ -e "$BIN_DIR/sshm" ]; then
        rm -f "$BIN_DIR/sshm"
        say "Removed $BIN_DIR/sshm"
    else
        say "Nothing to remove at $BIN_DIR/sshm"
    fi
    say "Your data was kept. Delete it by hand if you want it gone:"
    say "  ${XDG_CONFIG_HOME:-$HOME/.config}/sshm   (config.toml)"
    say "  ${XDG_DATA_HOME:-$HOME/.local/share}/sshm   (hosts.toml, vault.age)"
    say "  ${XDG_STATE_HOME:-$HOME/.local/state}/sshm   (log)"
}

case "${1:-}" in
    "") install_sshm ;;
    --uninstall | uninstall) uninstall_sshm ;;
    -h | --help) sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//' ;;
    *) die "unknown option: $1 (use --help)" ;;
esac
