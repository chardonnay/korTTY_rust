//! Converts Java's persisted menu action and chord names to native overrides.

use std::collections::{BTreeMap, BTreeSet};

use roxmltree::Node;
use serde::{Deserialize, Serialize};

use super::{Action, Bindings, Chord, MainKey, Override, command};

const MAX_OVERRIDES: usize = 256;

#[derive(Deserialize, Serialize)]
#[serde(transparent)]
pub(in crate::workspace) struct Overrides(BTreeMap<String, Option<Chord>>);

impl Overrides {
    pub(in crate::workspace) fn read(root: Node<'_, '_>, omitted: &mut usize) -> Option<Self> {
        let mut sections = root
            .children()
            .filter(|node| node.has_tag_name("keyBindingOverrides"));
        let section = sections.next()?;
        let rows: Vec<_> = section
            .children()
            .filter(Node::is_element)
            .take(MAX_OVERRIDES + 1)
            .collect();
        if sections.next().is_some()
            || section.attributes().next().is_some()
            || section.children().any(|node| {
                node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty())
            })
            || rows.len() > MAX_OVERRIDES
        {
            *omitted += 1;
            return None;
        }
        let mut result = BTreeMap::new();
        let mut validation = Bindings::default();
        for row in rows {
            let entry = row
                .has_tag_name("binding")
                .then(|| row.text())
                .flatten()
                .filter(|_| row.attributes().next().is_none() && row.children().count() == 1)
                .and_then(|text| text.split_once('='))
                .and_then(|(name, text)| {
                    let entry = command(action(name.trim())?)?;
                    let text = text.trim();
                    let chord = if text.eq_ignore_ascii_case("none") {
                        None
                    } else {
                        Some(parse_chord(text)?)
                    };
                    Some((entry, chord))
                });
            if let Some((entry, chord)) = entry {
                validation
                    .0
                    .insert(entry.id.clone(), Override::Parsed(chord));
                if validation.problem(entry).is_none() {
                    result.insert(entry.id.clone(), chord);
                    continue;
                }
            }
            *omitted += 1;
        }
        (!result.is_empty()).then_some(Self(result))
    }

    pub(in crate::workspace) fn len(&self) -> usize {
        self.0.len()
    }

    pub(in crate::workspace) fn merge(&self, bindings: &mut Bindings) -> bool {
        let previous_conflicts = bindings.conflicts();
        let mut added = BTreeSet::new();
        for entry in super::catalog() {
            if let Some(chord) = self.0.get(&entry.id)
                && !bindings.changed(entry)
            {
                bindings
                    .0
                    .insert(entry.id.clone(), Override::Parsed(*chord));
                if bindings.problem(entry).is_some() {
                    bindings.0.remove(&entry.id);
                } else {
                    added.insert(entry.id.as_str());
                }
            }
        }
        // Consider the imported assignments together so a valid swap can be restored.
        // Removing a conflicting assignment may put its default chord back into use.
        loop {
            let mut removed = false;
            for conflict in bindings.conflicts() {
                if previous_conflicts.contains(&conflict) {
                    continue;
                }
                for action in conflict.1 {
                    if let Some(entry) = command(action)
                        && added.remove(entry.id.as_str())
                    {
                        bindings.0.remove(&entry.id);
                        removed = true;
                    }
                }
            }
            if !removed {
                return !added.is_empty();
            }
        }
    }
}

pub(in crate::workspace) fn action(id: &str) -> Option<Action> {
    use Action::*;

    Some(match id {
        "toolbar.gitBash" => GitBash,
        "menu.file.newTab" => QuickConnect,
        "menu.file.newWindow" => NewWindow,
        "menu.file.closeTab" => CloseTab,
        "menu.file.closeWindow" => CloseWindow,
        "menu.file.closeAllTabs" => CloseAllTabs,
        "menu.file.closeOtherTabs" => CloseOtherTabs,
        "menu.file.closeTabsToRight" => CloseTabsToRight,
        "menu.file.renameTab" => RenameTab,
        "menu.file.reopenClosedTab" => ReopenClosedTab,
        "menu.file.openProject" => OpenProject,
        "menu.file.saveProject" => SaveProject,
        "menu.file.restorePreviousSession" => RestoreSession,
        "menu.file.quit" => Quit,
        "menu.edit.copy" => CopySelection,
        "menu.edit.paste" => Paste,
        "menu.edit.find" => Find,
        "menu.edit.previousPrompt" => PreviousPrompt,
        "menu.edit.nextPrompt" => NextPrompt,
        "menu.edit.selectLastOutput" => SelectLastOutput,
        "menu.edit.copyLastOutput" => CopyLastOutput,
        "menu.edit.quickSelect" => QuickSelect,
        "menu.edit.createBackup" => CreateBackup,
        "menu.edit.importBackup" => ImportBackup,
        "menu.connections.quickConnect" => QuickConnect,
        "menu.connections.manage" => ManageConnections,
        "menu.connections.import" => ImportConnections,
        "menu.connections.export" => ExportConnections,
        "menu.connections.sftpClient" => Sftp,
        "menu.connections.sftpHere" => SftpHere,
        "menu.connections.sendBreak" => SendSerialBreak,
        "menu.security.credentials" => Vault,
        "menu.security.unlockVault" => UnlockVault,
        "menu.security.knownHosts" => KnownHosts,
        "menu.security.sshKeys" => SshKeys,
        "menu.security.gpgKeys" => OpenPgpRecipients,
        "menu.settings.global" => Settings,
        "menu.configuration.preventSleep" => PreventSystemSleep,
        "menu.teamwork.settings" => TeamworkSettings,
        "menu.tools.snippets" => Snippets,
        "menu.tools.snippets.newWindow" => NewSnippetManagerWindow,
        "menu.tools.jobScheduler" => Scheduler,
        "menu.tools.sessionJournals" => SessionJournals,
        "menu.tools.toggleSessionJournal" => ToggleSessionJournal,
        "menu.tools.journalScreenshot" => SessionJournalScreenshot,
        "menu.tools.logfileManager" => LogfileManager,
        "menu.tools.sessionProcesses" => SessionProcesses,
        "menu.tools.aiManager" => AiManager,
        "menu.tools.savedChats" => SavedChats,
        "menu.tools.aiAgent" => TerminalAgent,
        "menu.tools.aiPlanning" => TerminalAgentPlan,
        "menu.tools.aiSwarm" => AgentSwarm,
        "menu.tools.asciiArt" => AsciiArt,
        "menu.tools.toggleRecording" => ToggleRecording,
        "menu.tools.videoManager" => Recordings,
        "menu.view.commandPalette" => CommandPalette,
        "menu.view.snippetPalette" => SnippetPalette,
        "menu.view.menuBar" => ShowMenuBar,
        "menu.view.iconToolbar" => ToggleActionToolbar,
        "menu.view.connectionToolbar" => ToggleConnectionToolbar,
        "menu.view.dashboard" => Dashboard,
        "menu.view.fullscreen" => Fullscreen,
        "menu.view.terminalOnlyFullscreen" => TerminalOnlyFullscreen,
        "menu.view.hideTerminalScrollbarsFullscreen" => HideTerminalScrollbarsFullscreen,
        "menu.view.timestamps" => ShowCommandTimestamps,
        "menu.view.fileBrowser.left" => FilesLeft,
        "menu.view.fileBrowser.right" => FilesRight,
        "menu.view.remoteSidebar.left" => RemoteSidebarLeft,
        "menu.view.remoteSidebar.right" => RemoteSidebarRight,
        "menu.view.remoteSidebar.toggle" => RemoteSidebarToggle,
        "menu.view.journalPanel.left" => JournalDockLeft,
        "menu.view.journalPanel.right" => JournalDockRight,
        "menu.view.journalPanel.toggle" => ToggleJournalDock,
        "menu.view.zoomIn" => ZoomIn,
        "menu.view.zoomOut" => ZoomOut,
        "menu.view.resetZoom" => ResetZoom,
        "menu.codingAgent.panel.left" => CodingAgentsLeft,
        "menu.codingAgent.panel.right" => CodingAgentsRight,
        "menu.codingAgent.panel.toggle" => ToggleCodingAgents,
        "menu.codingAgent.nextBlocked" => NextBlockedCodingAgent,
        "menu.plugins.aiAgentDesign" => AgentDesign,
        "menu.plugins.terminalEffects" => TerminalEffectsManager,
        "menu.help.guide" => Manual,
        "menu.help.about" => About,
        "palette.action.nextTab" => NextTab,
        "palette.action.previousTab" => PreviousTab,
        _ => return None,
    })
}

fn parse_chord(raw: &str) -> Option<Chord> {
    parse_chord_with(raw, false)
}

pub(super) fn parse_completion_chord(raw: &str) -> Option<Chord> {
    parse_chord_with(raw, true)
}

fn parse_chord_with(raw: &str, control_is_shortcut: bool) -> Option<Chord> {
    if raw.len() > 128 {
        return None;
    }
    let (mut shortcut, mut control, mut shift, mut alt) = (false, false, false, false);
    let mut key = None;
    for part in raw.split(['+', '-']) {
        let modifier = match part.trim().to_ascii_lowercase().as_str() {
            "" => continue,
            "shortcut" | "mod" | "ctrlcmd" | "cmd" | "command" | "meta" => &mut shortcut,
            "ctrl" | "control" if control_is_shortcut => &mut shortcut,
            "ctrl" | "control" => &mut control,
            "shift" => &mut shift,
            "alt" | "option" | "opt" => &mut alt,
            part => {
                if key.replace(parse_key(part)?).is_some() {
                    return None;
                }
                continue;
            }
        };
        if std::mem::replace(modifier, true) {
            return None;
        }
    }
    Some(Chord {
        key: key?,
        shortcut,
        control,
        shift,
        alt,
    })
}

fn parse_key(text: &str) -> Option<MainKey> {
    use MainKey::*;

    Some(match text {
        "tab" => Tab,
        "space" => Space,
        "enter" | "return" => Enter,
        "escape" | "esc" => Escape,
        "backspace" => Backspace,
        "delete" | "del" => Delete,
        "insert" | "ins" => Insert,
        "home" => Home,
        "end" => End,
        "pageup" | "pgup" => PageUp,
        "pagedown" | "pgdn" => PageDown,
        "up" | "uparrow" => Up,
        "down" | "downarrow" => Down,
        "left" | "leftarrow" => Left,
        "right" | "rightarrow" => Right,
        "comma" => Character(','),
        "period" => Character('.'),
        "slash" => Character('/'),
        "backslash" => Character('\\'),
        "semicolon" => Character(';'),
        "quote" => Character('\''),
        "bracketleft" => Character('['),
        "bracketright" => Character(']'),
        "minus" => Character('-'),
        "equal" => Character('='),
        "backquote" => Character('`'),
        "plus" => Character('+'),
        _ => match text.as_bytes() {
            [byte] if byte.is_ascii_lowercase() || byte.is_ascii_digit() => {
                Character(char::from(*byte))
            }
            [b'f', b'1'..=b'9'] | [b'f', b'1', b'0'..=b'2'] => Function(text[1..].parse().ok()?),
            _ => return None,
        },
    })
}
