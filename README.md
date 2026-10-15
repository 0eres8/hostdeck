# Hostdeck

A modern SSH and RDP connection manager for Linux, written in Rust. Hostdeck keeps your
hosts organized in folders and opens SSH sessions in tabs with a built-in terminal. Remote
desktops open through FreeRDP.

## Install

```bash
sudo apt install openssh-client freerdp3-x11   # connection clients
./install.sh                                   # installs to ~/.local, adds a menu entry
```

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

Uses FreeRDP 3 (`xfreerdp3`); the remote desktop opens in its own window and the tab shows
its status, a clear reason when a connection fails, and the FreeRDP log.

- Domain, fit-to-window (dynamic resolution), fullscreen with multi-monitor, or a fixed size
- Scaling (100/140/180 %), clipboard sharing, home folder as a drive, sound redirection
- Admin console, RD Gateway, certificate policy (trust on first use by default)
- The password is passed to FreeRDP on stdin, never on the command line

Set `HOSTDECK_RDP_CLIENT` to use another FreeRDP binary (for example `sdl-freerdp3`).

## Not included yet

Compared with MobaXterm, this first version does not have: remote desktops embedded inside a
tab, an SFTP file browser, a built-in X server (use X11 forwarding with your desktop's X/XWayland),
Telnet/VNC/serial/FTP sessions, split terminals, multi-execution, macros, or mouse reporting
for terminal apps (mouse clicks in `htop`, `mc` or `tmux` mouse mode). The MobaXterm importer
reads names, folders, hosts, ports, users and key files; it does not read MobaXterm's stored
passwords.
