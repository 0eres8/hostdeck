#!/usr/bin/env bash
# Install Hostdeck for the current user (no root needed for Hostdeck itself).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
bin="$here/hostdeck"
[ -f "$bin" ] || bin="$here/target/release/hostdeck"
assets="$here/assets"
[ -d "$assets" ] || assets="$here"

if [ ! -f "$bin" ]; then
    echo "hostdeck binary not found. Build it first with: cargo build --release" >&2
    exit 1
fi

install -Dm755 "$bin" "$HOME/.local/bin/hostdeck"
install -Dm644 "$assets/hostdeck.png" "$HOME/.local/share/icons/hicolor/256x256/apps/hostdeck.png"
install -Dm644 "$assets/hostdeck.desktop" "$HOME/.local/share/applications/hostdeck.desktop"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -q "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

echo "Installed Hostdeck to ~/.local/bin/hostdeck and added it to your applications menu."

missing=()
command -v ssh >/dev/null || missing+=(openssh-client)
command -v xfreerdp3 >/dev/null || command -v xfreerdp >/dev/null || missing+=(freerdp3-x11)
if [ ${#missing[@]} -gt 0 ]; then
    echo
    echo "Hostdeck needs these packages for connections:"
    echo "  sudo apt install ${missing[*]}"
fi
case ":$PATH:" in
    *":$HOME/.local/bin:"*) ;;
    *) echo; echo "Note: ~/.local/bin is not on your PATH. Start Hostdeck from the applications menu, or add it to PATH." ;;
esac
