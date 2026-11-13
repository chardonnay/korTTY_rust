use std::{collections::HashMap, fmt};

use iced::widget::{self, button, column, container, row, space, text, text_input};
use iced::{Element, Length, Size, Task, window};

use crate::session::{CodingAgentStatus, SessionId, Status};
use crate::workspace::{Message as Desktop, Workspace, dialog_placement};

use super::FocusTarget;

const TITLE: &str = "Rename agent";

#[derive(Clone)]
pub enum Message {
    Focus,
    Input(String),
    Submit,
    Cancel,
}

impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Focus => "Focus",
            Self::Input(_) => "Input([redacted])",
            Self::Submit => "Submit",
            Self::Cancel => "Cancel",
        })
    }
}

#[derive(Default)]
pub(in crate::workspace) struct Dialogs(HashMap<window::Id, Draft>);

struct Draft {
    owner: window::Id,
    pane: SessionId,
    agent_name: &'static str,
    alias: String,
    input: widget::Id,
}

fn alias_value(value: &str) -> Option<String> {
    // Java String.strip uses Character.isWhitespace, which excludes no-break spaces.
    let value = value.trim_matches(|ch: char| {
        matches!(ch, '\u{1c}'..='\u{1f}')
            || (ch.is_whitespace() && !matches!(ch, '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}'))
    });
    (!value.is_empty()).then(|| value.to_owned())
}

impl Workspace {
    pub(in crate::workspace) fn show_coding_agent_alias_dialog(
        &mut self,
        owner: window::Id,
        target: FocusTarget,
    ) -> Task<Desktop> {
        if let Some((dialog, focus)) = self.owner_dialog(owner) {
            return Task::batch([window::gain_focus(dialog), Task::done(focus)]);
        }
        if !self.palette_can_open(owner) || self.palette.is_open(owner) {
            return Task::none();
        }
        let pane = target.session;
        let Some(status) = self.live_coding_agent_for_alias(pane) else {
            self.notice(owner, "The coding agent is no longer available.".into());
            return Task::none();
        };
        let alias = self.sessions[&pane]
            .coding_agent
            .alias
            .clone()
            .unwrap_or_default();
        let (dialog, opened) = self.open_owned_placed_dialog(
            owner,
            dialog_placement::Kind::CodingAgentAlias,
            window::Settings {
                size: Size::new(460.0, 190.0),
                min_size: Some(Size::new(360.0, 180.0)),
                position: window::Position::Centered,
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(focus_message),
        );
        self.coding_agent_aliases.0.insert(
            dialog,
            Draft {
                owner,
                pane,
                agent_name: status.name,
                alias,
                input: widget::Id::unique(),
            },
        );
        opened
    }

    fn live_coding_agent_for_alias(&self, id: SessionId) -> Option<CodingAgentStatus> {
        if !self.window_settings.coding_agent_detection_enabled
            || !self
                .windows
                .values()
                .any(|window| window.pane_location(id).is_some())
        {
            return None;
        }
        let frame = self.sessions.get(&id)?.session.frame();
        if frame.status == Status::Running {
            frame.coding_agent()
        } else {
            None
        }
    }

    pub(in crate::workspace) fn rebind_coding_agent_alias(
        &mut self,
        previous: SessionId,
        replacement: SessionId,
    ) {
        if previous == replacement {
            return;
        }
        let alias = self.sessions[&previous].coding_agent.alias.clone();
        self.sessions
            .get_mut(&replacement)
            .expect("replacement pane exists")
            .coding_agent
            .alias = alias;
        for draft in self.coding_agent_aliases.0.values_mut() {
            if draft.pane == previous {
                draft.pane = replacement;
            }
        }
    }

    pub(in crate::workspace) fn update_coding_agent_alias(
        &mut self,
        dialog: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let Some(draft) = self.coding_agent_aliases.0.get_mut(&dialog) else {
            return Task::none();
        };
        match message {
            Message::Focus => Task::batch([
                window::gain_focus(dialog),
                widget::operation::focus(draft.input.clone()),
            ]),
            Message::Input(alias) => {
                draft.alias = alias;
                Task::none()
            }
            Message::Submit | Message::Cancel => {
                let draft = self
                    .coding_agent_aliases
                    .0
                    .remove(&dialog)
                    .expect("open draft");
                if matches!(message, Message::Submit) {
                    if self.live_coding_agent_for_alias(draft.pane).is_some() {
                        self.sessions
                            .get_mut(&draft.pane)
                            .expect("live pane exists")
                            .coding_agent
                            .alias = alias_value(&draft.alias);
                    } else {
                        self.notice(
                            draft.owner,
                            "The coding agent is no longer available.".into(),
                        );
                    }
                }
                if self.windows.contains_key(&draft.owner) {
                    self.close_placed_dialog_and_focus(dialog, draft.owner)
                } else {
                    self.close_placed_dialog(dialog)
                }
            }
        }
    }

    pub(in crate::workspace) fn coding_agent_alias_dialog_title(
        &self,
        dialog: window::Id,
    ) -> Option<&'static str> {
        self.coding_agent_aliases
            .0
            .contains_key(&dialog)
            .then_some(TITLE)
    }

    pub(in crate::workspace) fn coding_agent_alias_dialog_for_owner(
        &self,
        owner: window::Id,
    ) -> Option<(window::Id, Desktop)> {
        self.coding_agent_aliases
            .0
            .iter()
            .find_map(|(&dialog, draft)| {
                (draft.owner == owner).then(|| (dialog, focus_message(dialog)))
            })
    }

    pub(in crate::workspace) fn remove_coding_agent_alias_dialogs(&mut self, id: window::Id) {
        self.coding_agent_aliases
            .0
            .retain(|&dialog, draft| dialog != id && draft.owner != id);
    }

    pub(in crate::workspace) fn close_coding_agent_alias_dialog(
        &mut self,
        dialog: window::Id,
    ) -> Option<Task<Desktop>> {
        self.coding_agent_aliases
            .0
            .contains_key(&dialog)
            .then(|| self.update_coding_agent_alias(dialog, Message::Cancel))
    }

    pub(in crate::workspace) fn coding_agent_alias_dialog_view(
        &self,
        dialog: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let draft = self.coding_agent_aliases.0.get(&dialog)?;
        let send = move |message| Desktop::CodingAgentAlias(dialog, message);
        let view = container(
            column![
                text(draft.agent_name).size(18),
                text("Alias shown for this agent (empty restores the agent name)").size(13),
                text_input("Alias", &draft.alias)
                    .id(draft.input.clone())
                    .on_input(move |alias| send(Message::Input(alias)))
                    .on_submit(send(Message::Submit)),
                space::vertical(),
                row![
                    space::horizontal(),
                    button("Cancel").on_press(send(Message::Cancel)),
                    button("OK").on_press(send(Message::Submit)),
                ]
                .spacing(8),
            ]
            .spacing(10),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(16);
        Some(dialog_placement::escape::on_escape(
            view.into(),
            send(Message::Cancel),
        ))
    }
}

fn focus_message(dialog: window::Id) -> Desktop {
    Desktop::CodingAgentAlias(dialog, Message::Focus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_input_uses_java_strip_and_restores_the_kind_when_blank() {
        assert_eq!(alias_value(" \u{3000}\u{1c}"), None);
        assert_eq!(
            alias_value("\u{2003}Release 🧪 \t"),
            Some("Release 🧪".into())
        );
        assert_eq!(alias_value(" \u{a0} "), Some("\u{a0}".into()));
        assert_eq!(
            format!("{:?}", Message::Input("private alias".into())),
            "Input([redacted])"
        );
    }

    #[test]
    fn reconnect_transfers_only_the_replaced_pane_alias_and_its_draft() {
        use crate::session::Session;
        use crate::workspace::{GridSize, IbmKind};

        let mut workspace = Workspace::default();
        let mut pane = || {
            workspace.store_connected_session(
                Session::disconnected_ibm(IbmKind::Tn3270e),
                GridSize::default(),
                None,
                None,
            )
        };
        let (previous, replacement, unrelated) = (pane(), pane(), pane());
        workspace
            .sessions
            .get_mut(&previous)
            .unwrap()
            .coding_agent
            .alias = Some("Build".into());
        let (owner, other_owner) = (window::Id::unique(), window::Id::unique());
        let (dialog, other_dialog) = (window::Id::unique(), window::Id::unique());
        for (id, owner, pane) in [
            (dialog, owner, previous),
            (other_dialog, other_owner, unrelated),
        ] {
            workspace.coding_agent_aliases.0.insert(
                id,
                Draft {
                    owner,
                    pane,
                    agent_name: "Codex",
                    alias: "Draft".into(),
                    input: widget::Id::unique(),
                },
            );
        }

        workspace.rebind_coding_agent_alias(previous, replacement);

        assert_eq!(
            workspace.sessions[&replacement]
                .coding_agent
                .alias
                .as_deref(),
            Some("Build")
        );
        assert_eq!(workspace.coding_agent_aliases.0[&dialog].pane, replacement);
        assert_eq!(
            workspace.coding_agent_aliases.0[&other_dialog].pane,
            unrelated
        );
        workspace.remove_coding_agent_alias_dialogs(owner);
        assert!(!workspace.coding_agent_aliases.0.contains_key(&dialog));
        assert!(workspace.coding_agent_aliases.0.contains_key(&other_dialog));
    }
}
