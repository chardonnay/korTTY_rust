# KorTTY - SSH Client (Rust Edition)

A modern SSH terminal client built with Rust (Tauri v2) and React/TypeScript.
This repository tracks the feature migration from [KorTTY JavaFX](https://github.com/chardonnay/korTTY) to the Rust/Tauri desktop app.

## Highlights

- Multiple SSH tabs, split terminals, broadcast input, zoom control, dashboard and multi-window workflows
- Connection manager with credentials, SSH keys, GPG keys, custom credential environments and project save/open flows
- Import and export for KorTTY, MobaXterm, MTPuTTY and PuTTY Connection Manager
- Built-in AI workflows: profile manager, connection test, terminal selection actions, saved chats and transcript export
- SFTP file browser, snippet manager with XML import/export, ASCII art banner, backups and theme editors

## Feature Overview

### Terminal and session workflows

- Tabbed SSH sessions
- Horizontal and vertical split terminals
- Broadcast input to all splits in a tab
- Multi-window workspace dashboard
- Session zoom per tab or per split pane
- SFTP manager
- SSH tunnels and jump-host aware connection settings

### Productivity and data management

- Quick Connect
- Project open, preview, save and save-as flows
- Connection import and export
- Snippet manager with XML import/export
- Backup and restore
- Terminal and GUI theme editors

### AI features

- AI Manager for profile CRUD, usage/quota preview and saved chats
- OpenAI-compatible chat completion integration
- Terminal selection actions: `Summarize`, `Solve Problem`, `Ask...`
- AI result/chat tabs with follow-up prompts
- Auto title generation for saved chats
- TXT and Markdown transcript export
- Dedicated AI connection test with a minimal request path

### Security and customization

- AES-256-GCM encrypted passwords with master password support
- SSH key management with encrypted passphrases
- GPG key management for backup encryption
- Per-connection terminal theme assignment
- Toggleable menu bar and global terminal defaults

## Documentation

- Feature and workflow overview: [docs/FEATURE_OVERVIEW.md](docs/FEATURE_OVERVIEW.md)
- Latest branch release notes: [RELEASE_NOTES.md](RELEASE_NOTES.md)

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

### Build output

After `npm run tauri build`, the distributable artifacts are written below `src-tauri/target/release/bundle/`.
On macOS the primary output is typically `src-tauri/target/release/bundle/macos/KorTTY.app`, plus a DMG in `src-tauri/target/release/bundle/dmg/`.

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
| Ctrl+Shift+Y | Open AI Manager |
| Ctrl+Shift+L | Toggle Menu Bar |
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
├── environments.json
├── ssh-keys.json
├── gpg-keys.json
├── global-settings.json
├── snippets.json
├── ai-profiles.json
├── ai-chats.json
├── recent-projects.json
├── master-password-hash
├── kortty.log
├── history/
├── projects/
├── i18n/
└── ssh-keys/
```

## License

MIT License
