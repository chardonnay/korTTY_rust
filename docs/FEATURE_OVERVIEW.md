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

### AI Agent task execution

From a connected terminal session you can start the executable AI Agent in multiple ways:

- Use the terminal pane context menu: `AI -> Agent...`
- Open the dialog from the menu bar
- Type the terminal shortcut directly at the shell prompt: `agent ...` or `agent: ...`

The base command name is configurable in `Settings -> AI`. KorTTY automatically derives the matching `-ask` and `-plan` commands from that custom base name.

The AI Agent workflow currently supports:

- Inspecting the remote server first before asking the AI profile for the next safe step
- Running either in the current terminal session or in a dedicated `AI Agent` tab, configurable via `Settings -> AI -> AI Agent task target`
- Approval prompts for risky commands, including `Allow always` for the current run
- Sudo password prompts inside the terminal overlay or the `AI Agent` tab when privileged commands require authentication
- Temporary reuse of the sudo password for the current SSH session after it was entered once
- Transcript copy and save actions in the dedicated `AI Agent` tab

The `agent-ask` shortcut is the non-executing variant for direct Q&A:

- `agent-ask <question>`
- `agent-ask: <question>`

This mode answers the question without running remote shell commands.

### AI Agent planning mode

Planning mode opens a dedicated `AI Agent Plan` tab and does not execute shell commands directly.

You can start it with:

- `AI -> Planning...` from the terminal pane context menu
- The terminal shortcut `agent-plan <prompt>` or `agent-plan: <prompt>`

The planning flow is:

1. KorTTY probes the connected server
2. The AI asks clarifying questions first
3. You answer the questions or provide your own preferred approach
4. The AI returns one or more implementation options
5. You accept an option explicitly
6. You start execution explicitly via `Start accepted plan`

Each plan option includes:

- Feasibility
- Prerequisites
- Risks
- Alternatives
- Ordered implementation steps

Starting execution from an accepted plan hands the selected plan context over to the normal AI Agent run, which then uses the standard approval and sudo handling.

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
