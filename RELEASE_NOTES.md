# Release Notes

## Unreleased

### Added

- AI Manager with profile management, saved chats, usage/quota preview and AI result tabs
- OpenAI-compatible terminal selection actions for summaries, problem analysis and follow-up Q&A
- Auto title generation and transcript export for AI chats
- AI Agent execution for connected SSH sessions in the current terminal or a dedicated AI Agent tab
- AI Agent planning mode with clarifying questions, implementation options and explicit handoff into execution
- Configurable AI agent command names in `Settings -> AI`, including derived `<name>-ask` and `<name>-plan` shortcuts
- Configurable AI Agent task target in `Settings -> AI`
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

### Fixed

- Creating split terminals no longer leaks the prompt-hook bootstrap command into the terminal view
- CSS build warnings caused by the SFTP timestamp filename sanitizer were removed
- Closing the former primary split pane now promotes another pane instead of shutting down the entire split tab
- AI connection-test response parsing is more tolerant of wrapped or noisy API payloads
- AI Agent chat runs no longer start twice or mirror execution back into the terminal when the chat target is selected
- Sudo validation is more robust for quoted command text and no longer misclassifies non-interactive commands such as `systemctl stop ...`
