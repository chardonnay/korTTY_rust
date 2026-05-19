# KorTTY - SSH Client (Rust Edition)

A modern SSH terminal client built with Rust (Tauri v2) and React/TypeScript.
This repository tracks the feature migration from [KorTTY JavaFX](https://github.com/chardonnay/korTTY) to the Rust/Tauri desktop app.

Current version: v2.2.0

## Highlights

- Multiple SSH tabs, split terminals, broadcast input, zoom control, dashboard and multi-window workflows
- Connection manager with credentials, SSH keys, GPG keys, custom credential environments and project save/open flows
- Import and export for KorTTY, MobaXterm, MTPuTTY and PuTTY Connection Manager
- Built-in AI workflows: profile manager, saved chats, AI Agent execution, planning mode, inline activity feed and exports
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
- AI Agent execution for connected SSH sessions with inline activity feed, approval prompts, `Allow always`, sudo password entry and per-session sudo password caching
- AI Agent chat tabs with transcript copy and save actions
- AI Agent planning mode with clarifying questions, implementation options, accepted-plan handoff and explicit execution start
- Configurable agent command trio in `Settings -> AI`: `<name>`, `<name>-ask` and `<name>-plan`
- Configurable AI Agent task target in `Settings -> AI`: current terminal window or a dedicated AI Agent chat tab
- Terminal-targeted AI Agent runs show a bottom activity panel with elapsed time, token usage, collapsible details, history navigation, rerun, cancel, approval and masked sudo-password controls
- Current-run and all-runs activity export formats: Markdown, plain text, YAML, XML, JSON, PDF and Asciidoctor; PDF export uses bundled Noto Sans Mono font assets copied from the Java source resources
- Prompt-hook aliases emit hidden `OSC 777;korTTY-agent` markers so terminal-agent shortcuts keep the active remote `pwd -P`; explicit `OSC 7` and typed `cd`/`pushd`/`popd` updates are also tracked
- Agent commands execute with `cd '<tracked cwd>' && ...` only when the tracked cwd is a valid absolute remote path
- Agent command matching is case-sensitive by default; `Settings -> AI` can enable case-insensitive matching, show/hide the run dialog for shortcuts and set saved activity panel height/font size
- Auto title generation for saved chats
- TXT and Markdown transcript export
- Dedicated AI connection test with a minimal request path

### AI Agent shortcuts

KorTTY can intercept agent commands directly from the terminal prompt. By default the shortcut trio is:

- `agent <prompt>` or `agent: <prompt>` for executable AI Agent tasks
- `agent-ask <question>` or `agent-ask: <question>` for pure Q&A without command execution
- `agent-plan <prompt>` or `agent-plan: <prompt>` for planning-only runs that never execute commands directly

The base command name is configurable in `Settings -> AI`. If you rename `agent` to `susi`, the derived shortcuts `susi`, `susi-ask` and `susi-plan` are added; the default `agent`, `agent-ask` and `agent-plan` aliases remain available for compatibility.

When prompt hooks are enabled, KorTTY installs matching shell aliases for the configured shortcut trio and the default `agent` trio. The aliases emit hidden terminal markers containing the mode, `pwd -P` and prompt payload, then the Rust backend uses that cwd for subsequent remote exec calls. `agent-ask` remains non-executing and is routed through the normal AI ask/chat flow.

Typed `agent ...` terminal shortcuts always run in the current terminal window, even when `Settings -> AI -> AI Agent task target` is set to a dedicated chat tab.

During terminal-targeted runs, terminal output and the inline activity panel follow new Agent output automatically; the panel stays open after completion until the user closes it.

### Validation path

```shell
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings
npm run build
```

Manual check: connect to SSH, run `cd /tmp`, then `agent <task>` and verify the activity panel opens at the bottom and commands run relative to `/tmp`. Trigger approval and sudo-password states, confirm masked input and session-local reuse, then export current and all runs in each supported format.

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

- **macOS**: Apple Silicon (aarch64) — DMG + ZIP
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
