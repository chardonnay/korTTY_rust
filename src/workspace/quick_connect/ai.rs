use iced::widget::{checkbox, column, pick_list, text};
use iced::{Element, Length};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::connection_store::AiAssignments;
use crate::connection_window::ai_profile_choices;

use super::{Dialog, Message, Protocol, Workspace};

pub(super) fn profile_available(config: &Configuration, reference: &ProfileRef) -> bool {
    config
        .resolve_profile_reference(reference)
        .is_some_and(|id| {
            config
                .interactive_chat_profiles()
                .any(|(candidate, ..)| candidate == id)
        })
}

impl Dialog {
    pub(super) fn reset_ai_draft(&mut self) {
        self.ai = match self.protocol {
            Protocol::Ssh => self
                .selected()
                .map(|profile| AiAssignments::from_terminal(&profile.terminal)),
            Protocol::Local => self
                .selected_local()
                .map(|profile| AiAssignments::from_terminal(&profile.terminal)),
            Protocol::Serial => self
                .selected_serial()
                .map(|profile| profile.terminal_ai.clone()),
            _ => None,
        }
        .unwrap_or_default();
        self.ai_edited = false;
    }

    pub(super) fn ai_view(&self, config: &Configuration) -> Option<Element<'_, Message>> {
        let profiles = config
            .interactive_chat_profiles()
            .filter_map(|(id, name, ..)| {
                Some((
                    config.legacy_policy_alias(id),
                    config.profile_reference(id)?,
                    name.to_owned(),
                ))
            })
            .collect::<Vec<_>>();
        let skills = config.connection_skill_choices();
        if profiles.is_empty() && skills.is_empty() && self.ai.is_empty() {
            return None;
        }
        let (choices, selected) = ai_profile_choices(&profiles, self.ai.profile.as_ref());
        let mut body = column![
            text("AI profile"),
            pick_list(choices, Some(selected), |choice| Message::AiProfile(
                choice.0
            ))
            .width(Length::Fill),
        ]
        .spacing(6);
        if !skills.is_empty() || !self.ai.skills.is_empty() {
            body = body.push(text("Assigned AI skills"));
            for (id, name) in &skills {
                let id = *id;
                body = body.push(
                    checkbox(self.ai.skills.contains(&id))
                        .label(name.clone())
                        .on_toggle(move |enabled| Message::AiSkill(id, enabled)),
                );
            }
            for &id in &self.ai.skills {
                if !skills.iter().any(|(available, _)| *available == id) {
                    body = body.push(
                        checkbox(true)
                            .label(format!("Unavailable skill {id}"))
                            .on_toggle(move |enabled| Message::AiSkill(id, enabled)),
                    );
                }
            }
        }
        Some(body.into())
    }
}

impl Workspace {
    pub(in crate::workspace) fn terminal_ai_assignments<'a>(
        &'a self,
        pane: &'a super::super::PaneSession,
    ) -> (Option<&'a ProfileRef>, &'a [u64]) {
        if let Some(assignment) = pane.quick_ai.as_ref().or_else(|| {
            pane.saved_serial
                .as_ref()
                .map(|profile| &profile.terminal_ai)
        }) {
            return (assignment.profile.as_ref(), &assignment.skills);
        }
        self.terminal_overrides(pane)
            .map_or((None, &[]), |settings| {
                (settings.ai_profile.as_ref(), &settings.ai_skills)
            })
    }
}
