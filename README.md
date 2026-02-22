# KorTTY - SSH Client (Rust Edition)

A modern SSH terminal client built with Rust (Tauri v2) and React/TypeScript.
Full-featured migration of [KorTTY JavaFX](https://github.com/chardonnay/korTTY).

## Features

- **GUI-based**: Modern dark-themed interface with React + Tailwind CSS
- **Tab Support**: Multiple SSH connections in one window
- **Font Size Adjustment**: Zoom in/out (Ctrl+Plus, Ctrl+Minus, Ctrl+0)
- **Split-Screen with Broadcast**: Split terminal view and broadcast input
- **Multi-Window**: Multiple windows with tab drag-and-drop between them
- **Encrypted Passwords**: AES-256-GCM encryption with master password
- **SSH Key Management**: Centralized management with encrypted passphrases
- **Customizable Display**: Font, colors (global or per connection)
- **Project Management**: Save and load connection sets with history
- **Import/Export**: Import from MTPuTTY, MobaXterm, PuTTY Connection Manager
- **Dashboard**: Overview of all open connections
- **SFTP Manager**: Two-panel file browser with full file operations
- **Snippet Manager**: Create, search, favorite, organize reusable snippets
- **ASCII Art Banner**: FIGlet banner generator with multiple styles
- **Backup & Restore**: Encrypted backups (password or GPG)
- **Multilanguage**: 8 built-in + dynamic translation via APIs
- **Quick Connect**: Fast connection dialog with frequently used connections
- **SSH Tunnels**: Local and remote port forwarding
- **Jump Server**: Bastion host support
- **Terminal Logging**: Automatic compressed session logging
- **GPG Key Management**: For backup encryption

## Requirements

- Rust 1.75+ (with cargo)
- Node.js 18+ (with npm)
- Platform-specific: see [Tauri Prerequisites](https://v2.tauri.app/start/prerequisites/)

## Development

```shell
npm install
npm run tauri dev
```

## Build

```shell
npm run tauri build
```

## Pre-built Binaries

Pre-built packages are available on [GitHub Releases](https://github.com/chardonnay/korTTY_rust/releases):

- **macOS**: Intel (x86_64) + Apple Silicon (aarch64) — DMG + ZIP
- **Windows**: Intel (x86_64) + ARM (aarch64) — MSI + ZIP
- **Linux**: Intel (x86_64) + ARM (aarch64) — DEB + RPM + tar.gz
- **Arch Linux**: x86_64 — pacman `.pkg.tar.zst`

## Keyboard Shortcuts

| Shortcut | Action |
|---|---|
| Ctrl+T | New Tab |
| Ctrl+Shift+N | New Window |
| Ctrl+W | Close Tab |
| Ctrl+Tab | Next Tab |
| Ctrl+Shift+Tab | Previous Tab |
| Ctrl+O | Open Project |
| Ctrl+S | Save Project |
| Ctrl+Shift+D | Toggle Dashboard |
| Ctrl+Plus | Zoom In |
| Ctrl+Minus | Zoom Out |
| Ctrl+0 | Reset Zoom |
| Ctrl+Shift+B | Create Backup |
| Ctrl+K | Quick Connect |
| Ctrl+Q | Quit |
| F11 | Fullscreen |

## Configuration

All configuration is stored under `~/.kortty/`:

```
~/.kortty/
├── connections.json
├── credentials.json
├── ssh-keys.json
├── gpg-keys.json
├── global-settings.json
├── snippets.json
├── master-password-hash
├── kortty.log
├── history/
├── projects/
├── i18n/
└── ssh-keys/
```

## License

MIT License
