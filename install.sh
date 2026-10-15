#!/usr/bin/env bash
# Install or update Hostdeck from the latest GitHub release:
#   curl -fsSL https://raw.githubusercontent.com/0eres8/hostdeck/main/install.sh | bash
#
# Everything runs inside main(), so a partially downloaded script never executes.

set -euo pipefail

REPO="0eres8/hostdeck"
ASSET="hostdeck_amd64.deb"
BASE_URL="${HOSTDECK_RELEASE_URL:-https://github.com/$REPO/releases/latest/download}"

TMP_DIR=""
cleanup() { [ -z "$TMP_DIR" ] || rm -rf -- "$TMP_DIR"; }
trap cleanup EXIT

say() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mError:\033[0m %s\n' "$*" >&2; exit 1; }

main() {
    [ "$(uname -s)" = "Linux" ] || die "Hostdeck runs on Linux only."
    [ -r /etc/os-release ] || die "Cannot identify this Linux distribution."
    # shellcheck disable=SC1091
    . /etc/os-release
    case " ${ID:-} ${ID_LIKE:-} " in
        *" debian "* | *" ubuntu "*) ;;
        *) die "This installer supports Ubuntu and Debian based systems (found: ${PRETTY_NAME:-unknown})." ;;
    esac
    command -v apt-get >/dev/null || die "apt-get is required."
    command -v dpkg >/dev/null || die "dpkg is required."
    [ "$(dpkg --print-architecture)" = "amd64" ] || die "Only 64-bit x86 (amd64) is supported for now."

    local sudo=""
    if [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null || die "Run as root or install sudo."
        sudo="sudo"
    fi

    local fetch
    if command -v curl >/dev/null; then
        fetch="curl -fsSL --retry 3 -o"
    elif command -v wget >/dev/null; then
        fetch="wget -q -O"
    else
        die "curl or wget is required."
    fi

    TMP_DIR="$(mktemp -d)"
    local tmp="$TMP_DIR"
    # apt reads the package as its unprivileged _apt user.
    chmod 755 "$tmp"

    say "Downloading the latest Hostdeck release"
    $fetch "$tmp/$ASSET" "$BASE_URL/$ASSET" || die "Download failed: $BASE_URL/$ASSET"
    $fetch "$tmp/$ASSET.sha256" "$BASE_URL/$ASSET.sha256" || die "Checksum download failed."

    say "Verifying checksum"
    local expected actual
    expected="$(awk '{print $1}' "$tmp/$ASSET.sha256")"
    actual="$(sha256sum "$tmp/$ASSET" | awk '{print $1}')"
    [ -n "$expected" ] && [ "$expected" = "$actual" ] || die "Checksum mismatch; the download may be corrupted."
    chmod 644 "$tmp/$ASSET"

    local version
    version="$(dpkg-deb -f "$tmp/$ASSET" Version)"

    say "Installing Hostdeck $version and its dependencies (you may be asked for your password)"
    $sudo apt-get update -qq || say "Warning: apt-get update reported problems; trying the installation anyway"
    $sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$tmp/$ASSET" >/dev/null

    command -v hostdeck >/dev/null || die "Installation finished but hostdeck was not found on PATH."
    say "Installed $(hostdeck --version)"
    echo
    echo "Start Hostdeck from your applications menu, or run: hostdeck"
    echo "Update later by running the same command again."
    echo "Uninstall with: sudo apt remove hostdeck"
}

main "$@"
