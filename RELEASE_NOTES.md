# Release Notes

## Unreleased

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
