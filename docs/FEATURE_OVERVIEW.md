# KorTTY Feature Overview

This document summarizes the currently implemented desktop workflows in the Rust/Tauri version of KorTTY.

## Terminal workflows

- Open SSH sessions in multiple tabs.
- Split a connected terminal horizontally or vertically.
- Broadcast keyboard input across all panes in the active split tab.
- Zoom globally per tab or override zoom per split pane.
- Reorder split panes and keep the remaining panes alive when the former primary pane is closed.
- Use the workspace dashboard to monitor open windows and active connections.

## AI workflows

### AI Manager

Open `Tools -> AI Manager...` or use `Ctrl/Cmd+Shift+Y`.

The AI Manager supports:

- Creating, editing, testing and deleting AI profiles
- Configuring endpoint URL, model, API key, selection limits and token warning thresholds
- Reviewing saved AI chats
- Reopening saved chats in dedicated AI tabs

### Terminal selection actions

From a connected terminal pane:

1. Select text in the terminal
2. Open the pane context menu
3. Use `AI -> Summarize`, `AI -> Solve Problem` or `AI -> Ask...`

The result opens in a dedicated AI tab where you can:

- Continue the conversation with follow-up prompts
- Switch AI profiles
- Generate a better chat title automatically
- Save the chat
- Export the transcript as TXT or Markdown

## Project workflows

The project flow currently supports:

- Open Project
- Save Project
- Save Project As
- Project preview before opening
- Project settings editing
- Recent project history

Projects store connection references, dashboard state and related metadata, and can reopen the workspace with or without reconnecting sessions automatically.

## Import and export

Connection exchange currently supports:

- KorTTY
- MobaXterm
- MTPuTTY
- PuTTY Connection Manager

Snippets can also be imported from and exported to XML.

## Credentials and environments

Saved credentials are no longer limited to fixed environments only. KorTTY now supports:

- Built-in environments
- Custom user-defined environments
- Protection against deleting environments that are still referenced by saved credentials

## Packaging

Run the following from the repository root:

```bash
npm install
npm run tauri dev
```

For a distributable desktop build:

```bash
npm run tauri build
```

Release artifacts are written to `src-tauri/target/release/bundle/`.
