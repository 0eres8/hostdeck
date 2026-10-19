# Hostdeck

A modern SSH and RDP connection manager for Linux, written in Rust. Hostdeck keeps your
hosts organized in folders and opens SSH sessions and remote desktops in tabs, with a
built-in terminal and a built-in RDP client.

## Install

On Ubuntu 24.04 or newer (and Debian-based systems with FreeRDP 3), one command installs
Hostdeck with everything it needs:

```bash
curl -fsSL https://raw.githubusercontent.com/0eres8/hostdeck/main/install.sh | bash
```

No curl? Use `wget -qO- https://raw.githubusercontent.com/0eres8/hostdeck/main/install.sh | bash`.

The script downloads the latest release package, verifies its checksum and installs it with
apt, which also installs OpenSSH, and FreeRDP for sessions that open in a separate window.
Run the same command again to update. Uninstall with `sudo apt remove hostdeck`.

Then start **Hostdeck** from the applications menu, or run `hostdeck`.

Passwords are stored in the desktop keyring (GNOME Keyring on Ubuntu), never in the
session file. Sessions live in `~/.config/hostdeck/hostdeck.toml` (owner-only permissions).

## Building from source

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev \
    libxcursor-dev libxrandr-dev libxi-dev libgl1-mesa-dev libdbus-1-dev
cargo build --release
cargo test
```

## Releasing

Bump `version` in `Cargo.toml`, commit, then tag and push:

```bash
git tag v0.2.0 && git push origin main v0.2.0
```

GitHub Actions builds the `.deb` on Ubuntu 24.04, smoke-tests it on 24.04 and 26.04, and
publishes the release that the install command downloads.

## Host management

- Folder tree with nested folders, item counts, collapse state that is remembered
- Drag and drop sessions and folders between folders
- New, edit, duplicate, rename, move and delete sessions; open all sessions in a folder
- Search across names, hosts, users, folders and notes
- Favorites and recently used sessions on the home screen
- Color per session, shown in the tree and on the tab
- Notes per session
- Shared credentials: named logins reused across sessions
- Quick connect: `user@host`, `host:2222`, `ssh://host`, `rdp://user@host`
- Import from MobaXterm (`.mxtsessions`), from `~/.ssh/config`, and from Hostdeck exports
- Export sessions and folders (without passwords)

## SSH

Runs your system OpenSSH client in a terminal tab, so `~/.ssh/config`, ssh-agent, known_hosts,
host-key prompts, OTP and keyboard-interactive logins behave exactly like in a normal terminal.

- Private key, jump hosts (`-J`), X11 forwarding, agent forwarding, compression, keepalive
- Local, remote and dynamic (SOCKS) port forwarding per session
- Remote command and extra ssh arguments
- A saved password is typed automatically at the first password prompt
- Terminal: 256 colors and truecolor, bold/underline/strikethrough, wide characters,
  10,000 lines of scrollback (configurable), copy on select, right-click paste

Keys: **Ctrl+C** interrupts, **Ctrl+Shift+C** copies, **Ctrl+V** / **Ctrl+Shift+V** pastes,
**Shift+PageUp/PageDown** scrolls, **R** reconnects a closed session.
Tabs: **Ctrl+Tab** / **Ctrl+Shift+Tab** switch, **Ctrl+Shift+D** duplicates, **Ctrl+Shift+W** closes.

## RDP

Remote desktops open in a tab, just like SSH sessions, with Hostdeck's built-in RDP client
(based on [IronRDP](https://github.com/Devolutions/IronRDP)). The remote desktop resizes with
the Hostdeck window.

- Network Level Authentication with local and domain accounts: fill in the domain on the Login
  tab, or type `DOMAIN\user` as the username
- Keyboard and mouse pass-through, including the wheel; **Ctrl+Alt+Del** from the toolbar
- Text copied on either side can be pasted on the other
- Scaling (100/140/180 %) on top of the display scale of this computer
- Server certificates are trusted on first use and checked before the password is sent; a
  changed certificate is shown with its fingerprint and only accepted when you choose to
- A clear reason when a connection fails, and a connection log

A session can also open in a separate FreeRDP 3 window (`xfreerdp3`) instead, set under
**RDP options → Open in**. Use that for sound, sharing your home folder as a drive, an RD
Gateway, multiple monitors or extra FreeRDP arguments.

- Fit-to-window (dynamic resolution), fullscreen with multi-monitor, or a fixed size
- Fullscreen sessions show a bar at the top edge to minimize, leave fullscreen or disconnect;
  **Ctrl+Alt+Enter** also toggles fullscreen
- The password is passed to FreeRDP on stdin, never on the command line

Set `HOSTDECK_RDP_CLIENT` to use another FreeRDP binary (for example `sdl-freerdp3`).

## Not included yet

Compared with MobaXterm, this version does not have: an SFTP file browser, a built-in X server (use X11 forwarding with your desktop's X/XWayland),
Telnet/VNC/serial/FTP sessions, split terminals, multi-execution, macros, or mouse reporting
for terminal apps (mouse clicks in `htop`, `mc` or `tmux` mouse mode). The MobaXterm importer
reads names, folders, hosts, ports, users and key files; it does not read MobaXterm's stored
passwords.
