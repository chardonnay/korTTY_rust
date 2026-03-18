# Release Notes

## Unreleased

### Added

- AI Manager with profile management, saved chats, usage/quota preview and AI result tabs
- OpenAI-compatible terminal selection actions for summaries, problem analysis and follow-up Q&A
- Auto title generation and transcript export for AI chats
- Project open, preview, save, save-as and settings flows
- Connection export support for KorTTY, MobaXterm, MTPuTTY and PuTTY Connection Manager
- Custom credential environments
- Snippet XML import and export
- Persistent menu bar toggle

### Changed

- README and documentation now reflect the current Rust/Tauri feature set and build outputs
- AI connection testing now uses a dedicated minimal request path with shorter test timeouts

### Fixed

- Creating split terminals no longer leaks the prompt-hook bootstrap command into the terminal view
- CSS build warnings caused by the SFTP timestamp filename sanitizer were removed
- Closing the former primary split pane now promotes another pane instead of shutting down the entire split tab
- AI connection-test response parsing is more tolerant of wrapped or noisy API payloads
