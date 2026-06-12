# Release Notes

## v2.2.0

### Added — Java v2.2 feature parity port

- Terminal recording: lightweight replay capture per tab or active split (`.korttyrec.jsonl.gz`, Java-compatible format), idle auto-pause with input-activity tracking, snapshot deduplication, optional per-cell color capture, in-app replay viewer with timeline slider, time-jump (`MM:SS`) and 1x–20x playback speed, plus WebM/VP9 and MKV/FFV1 video export with glyph rendering, custom time ranges and live progress/ETA; Video Manager dialog and `Settings -> Video`; `Ctrl/Cmd+Shift+E` toggle
- Monaco-based snippet editor (locally bundled, no CDN) with column ruler and line-length limit (20–240), format-to-width, ten built-in IntelliJ-inspired editor profiles plus custom profiles, snippet history with restore, side-by-side snippet diff, and save-as-new
- Snippet AI workflows: cursor-aware assistant, ghost-text completions with optional session-local auto-completion, error review, improvement themes, alternative solutions (selection-aware), security review with selectable fixes, technical description generator in comment syntax, and AI formatting fallback — all with before/after diff previews and hardened prompts (no hidden reasoning, no invented files/URLs)
- Local PlantUML snippet diagrams stored with the snippet: stale detection by content hash, regeneration with custom instructions, readable activity colors, SVG export and clickable code-reference hotspots
- Shared code formatter service with provider states (built-in, bundled, external fallback, unavailable), ~16 languages, pinned formatter manifest and line-width support for Prettier, Black and Perl::Tidy
- Real SFTP subsystem via `russh-sftp` for all remote file operations (listing, transfers, text editing, delete/rename/mkdir/chmod) with Java-equivalent subsystem failure diagnostics and automatic exec fallback when the server rejects the SFTP subsystem
- SFTP file editing through the snippet editor with overwrite, validated remote "Save As" and save-as-snippet; terminal selection can be opened as a remote file in the snippet editor with binary detection
- Application update checker: GitHub release polling on startup and hourly, semantic version comparison, platform/architecture-aware asset selection (including Linux distro detection), SHA256-verified downloads to the Downloads folder, snooze/skip handling and `Settings -> Updates`
- App-wide designs with previews in `Settings -> Appearance`: Normal, Matrix Terminal, Holographic Interface, Klingon Tactical and Elegant Dark (terminal and editor content colors stay untouched); refreshed master-password login with logo
- Dockable local file browser full feature set: context menu with copy/cut/paste, rename, new file/folder, ZIP/TAR/TAR.GZ archives, owner/permission editor with octal validation and principal lookup, file details, hidden-file toggle and resizable dividers
- Per-connection terminal emulation (TERM value) with searchable selector in Quick Connect and Connection Edit; per-connection and global terminal color disabling via a stateful SGR color-sequence filter
- Drag-and-drop file upload to SSH terminals with `~`-path resolution through the SFTP start directory, progress dialog (target, elapsed time, current file) and cancellation; prompt-heuristic working-directory extraction as cwd-tracking fallback
- AI CLI providers: profiles can run through 17 known local CLI tools (Claude Code, Codex, Gemini, OpenCode, …) with argument templates, placeholder expansion and timeouts; LM Studio auto model selection; reasoning-effort discovery with caching; default AI profile selection
- Per-connection AI profile and connection-scoped AI skills with pinning (assigned skills bypass relevance auto-detection)
- Terminal agent robustness: decision normalization for LLM output variations, mutating-command confirmation gate and a global agent-execution kill switch in `Settings -> AI`
- JobScheduler AI agent jobs now run a full decision loop with command execution, risk-based auto-approval (instead of blanket blocking), sudo handling and journaling; one-time auto-approve default migration for existing jobs
- Configurable log directory with retention (0–3650 days) and automatic gzip compression of old logs in `Settings -> Logging`; settings hot-reload on external file changes
- Tools menu shortcuts (`Ctrl+Shift+J/V`, `Ctrl+Alt+A/P`), `Ctrl+Q` closes secondary windows only, terminal-only fullscreen scrollbar hiding, and complete translations for all eight UI languages (680 keys each)

### Added

- AI Manager with profile management, saved chats, usage/quota preview and AI result tabs
- OpenAI-compatible terminal selection actions for summaries, problem analysis and follow-up Q&A
- Auto title generation and transcript export for AI chats
- AI Agent execution for connected SSH sessions in the current terminal or a dedicated AI Agent tab
- Inline bottom AI Agent activity panel for terminal-targeted runs with status, elapsed time, token usage, collapsible details, run history, rerun, cancel, approval and masked sudo-password controls
- AI Agent activity export for current or all runs as Markdown, plain text, YAML, XML, JSON, PDF and Asciidoctor
- Bundled Noto Sans font assets from the Java source for local terminal-agent PDF export
- AI Agent planning mode with clarifying questions, implementation options and explicit handoff into execution
- Configurable AI agent command names in `Settings -> AI`, including derived `<name>-ask` and `<name>-plan` shortcuts while preserving the default `agent` shortcut trio
- Configurable AI Agent task target in `Settings -> AI`
- AI settings for shortcut run dialog visibility, case-insensitive command matching, saved panel height and saved activity font size
- Prompt-hook shell aliases for `<name>`, `<name>-ask`, `<name>-plan` and the default `agent` trio with hidden `OSC 777;korTTY-agent` cwd/prompt markers
- Remote cwd tracking from `OSC 7`, typed directory changes and terminal-agent shortcut markers
- File-type-count fast path for plain-text versus binary/non-text file count prompts under absolute directories
- Copy and save actions for dedicated AI Agent transcripts
- Terminal context-menu entries for `AI -> Agent...` and `AI -> Planning...`
- Project open, preview, save, save-as and settings flows
- Connection export support for KorTTY, MobaXterm, MTPuTTY and PuTTY Connection Manager
- Custom credential environments
- Snippet XML import and export
- Persistent menu bar toggle

### Changed

- README and documentation now reflect the current Rust/Tauri feature set and build outputs
- AI connection testing now uses a dedicated minimal request path with shorter test timeouts
- AI Agent can prefetch and reuse the sudo password for the current SSH session after the user approved the run
- AI Agent runs can be routed either into the active terminal session or a dedicated AI Agent chat tab
- Terminal-agent JSON prompts now request JSON response format from OpenAI-compatible profiles when supported
- Terminal-agent commands are wrapped with the tracked absolute remote cwd before exec
- Agent command matching is case-sensitive by default; case-insensitive matching is opt-in

### Fixed

- Creating split terminals no longer leaks the prompt-hook bootstrap command into the terminal view
- CSS build warnings caused by the SFTP timestamp filename sanitizer were removed
- Closing the former primary split pane now promotes another pane instead of shutting down the entire split tab
- AI connection-test response parsing is more tolerant of wrapped or noisy API payloads
- AI Agent chat runs no longer start twice or mirror execution back into the terminal when the chat target is selected
- Sudo validation is more robust for quoted command text and no longer misclassifies non-interactive commands such as `systemctl stop ...`
- Terminal-agent JSON prompts retry without `response_format` when an OpenAI-compatible endpoint rejects JSON mode
- Typed `agent ...` terminal shortcuts now always start the Agent in the current terminal window instead of inheriting the dedicated-chat target setting
- Terminal-targeted Agent output now auto-scrolls, and the inline Agent panel remains open after completion until the user closes it
- The inline Agent status bar now reserves terminal space instead of covering the last terminal row
- Terminal-agent text exports are allowed by the Tauri FS ACL and export write failures are shown inline instead of blanking the UI

### Validation

- `cargo fmt --check --manifest-path src-tauri/Cargo.toml`
- `cargo test --manifest-path src-tauri/Cargo.toml`
- `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets -- -D warnings`
- `npm run build`
