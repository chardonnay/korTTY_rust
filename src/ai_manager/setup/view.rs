//! Native review pages for the local AI setup assistant.

use std::fmt;

use iced::widget::{
    button, checkbox, column, container, pick_list, progress_bar, row, scrollable, space, text,
};
use iced::{Element, Length};

use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::local_ai::managed_models::huggingface::GIB;

use super::*;

impl fmt::Display for Access {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anonymous => formatter.write_str("Anonymous"),
            Self::Saved(credential) => formatter.write_str(&credential.label),
        }
    }
}

impl Wizard {
    pub(crate) fn view(
        &self,
        enabled: bool,
        hub_tokens: Vec<CredentialChoice>,
    ) -> Element<'_, Message> {
        let busy = self.busy();
        let body = match self.page {
            Page::Privacy => column![
                text("Model inference and function tests run on this computer."),
                text("Setup contacts the signed llama.cpp release channel when a runtime is needed. It contacts Hugging Face to inspect and download selected model files."),
                text("A selected vault token is sent to Hugging Face to access the model repository."),
            ].spacing(10).into(),
            Page::Hardware => self.hardware_view(),
            Page::Roles => self.roles_view(enabled, hub_tokens),
            Page::License => self.license_view(enabled && !busy),
            Page::Install => self.install_view(),
            Page::Finish => self.finish_view(),
        };
        let back = !busy && matches!(self.page, Page::Hardware | Page::Roles | Page::License);
        let forward = match self.page {
            Page::Privacy | Page::Hardware => Some(("Next", enabled, Message::Next)),
            Page::Roles => Some((
                "Review downloads",
                enabled && self.has_choices(),
                Message::Next,
            )),
            Page::License if self.review_ready() => {
                Some(("Install and test", enabled && self.accepted, Message::Next))
            }
            Page::License | Page::Install if !busy => Some(("Retry", enabled, Message::Retry)),
            Page::Finish => Some(("Finish", true, Message::Finish)),
            _ => None,
        };
        column![
            text("Local AI setup assistant").size(22),
            text(self.page.title()).size(17),
            scrollable(container(body).padding(4)).height(Length::Fill),
            (!self.notice.is_empty()).then(|| text(self.notice.as_str())
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)),
            row![
                (self.page != Page::Finish).then(|| button(if self.stopping {
                    "Stopping…"
                } else if busy {
                    "Stop"
                } else {
                    "Cancel"
                })
                .style(button::secondary)
                .on_press_maybe((self.can_close() && !self.stopping).then_some(Message::Cancel))),
                space::horizontal(),
                (self.page != Page::Privacy && self.page != Page::Finish).then(|| button("Back")
                    .style(button::secondary)
                    .on_press_maybe(back.then_some(Message::Back))),
                forward.map(|(label, allowed, message)| button(label)
                    .on_press_maybe((allowed && !busy).then_some(message))),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(10)
        .into()
    }

    fn hardware_view(&self) -> Element<'_, Message> {
        let memory = self.memory.map_or_else(
            || {
                "System memory could not be detected. The assistant offers baseline models."
                    .to_owned()
            },
            |bytes| {
                format!(
                    "Detected system memory · {:.1} GiB",
                    bytes as f64 / GIB as f64
                )
            },
        );
        let backend = if cfg!(target_os = "macos") {
            "When needed, setup recommends a signed Metal package or a CPU package."
        } else {
            "When needed, setup recommends a signed CPU package or a Vulkan package."
        };
        column![
            text(memory),
            text(backend),
            text(
                "Context size and other running models also affect how much memory inference uses."
            ),
        ]
        .spacing(10)
        .into()
    }

    fn roles_view(
        &self,
        enabled: bool,
        credentials: Vec<CredentialChoice>,
    ) -> Element<'_, Message> {
        let choices = std::iter::once(Access::Anonymous)
            .chain(credentials.into_iter().map(Access::Saved))
            .collect::<Vec<_>>();
        let access: Element<'_, Message> = if enabled {
            pick_list(choices, Some(&self.access), Message::Token).dismiss_on_escape()
        } else {
            text(self.access.to_string()).into()
        };
        let mut roles =
            column![row![text("Hugging Face access"), access].spacing(8).wrap()].spacing(10);
        for (role, selected) in Role::ALL.into_iter().zip(self.choices.iter().cloned()) {
            let enabled_role = checkbox(selected.is_some())
                .label(role.to_string())
                .on_toggle_maybe(enabled.then_some(move |value| Message::EnableRole(role, value)));
            let recommendation = selected.map(|selected| -> Element<'_, Message> {
                if enabled {
                    pick_list(self.recommendations(role), Some(selected), move |choice| {
                        Message::Recommend(role, choice)
                    })
                    .dismiss_on_escape()
                } else {
                    text(selected.to_string()).into()
                }
            });
            roles = roles.push(row![enabled_role, recommendation].spacing(8).wrap());
        }
        roles.into()
    }

    fn license_view(&self, enabled: bool) -> Element<'_, Message> {
        let mut items = column![].spacing(12);
        match &self.runtime {
            Runtime::Unknown => {}
            Runtime::Existing(choice) => {
                items = items.push(text(format!(
                    "Installed llama.cpp runtime · {} · {}",
                    choice.id, choice.backend
                )));
            }
            Runtime::Planned(_, choice) | Runtime::Installed(choice) => {
                items = items.push(
                    column![
                        text(format!(
                            "Signed llama.cpp runtime · {} · {}",
                            choice.id, choice.backend
                        )),
                        text(choice.url.as_str())
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        text(format!(
                            "{} bytes · SHA-256 {}",
                            choice.size,
                            choice.digest()
                        ))
                        .size(13)
                        .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(3),
                );
            }
        }
        let mut bytes = 0_u64;
        for model in &self.models {
            let Some(reviewed) = &model.reviewed else {
                continue;
            };
            let plan = &reviewed.plan;
            bytes += plan.total();
            let roles = Role::ALL
                .into_iter()
                .zip(&self.choices)
                .filter(|(_, choice)| choice.as_ref() == Some(&model.recommendation))
                .map(|(role, _)| role.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let mut files = column![
                text(format!("{} · {roles}", model.recommendation)),
                text(format!("Commit · {}", plan.revision))
                    .size(13)
                    .wrapping(text::Wrapping::WordOrGlyph),
                text(format!(
                    "License · {}",
                    reviewed
                        .license
                        .as_deref()
                        .unwrap_or("Not reported by Hugging Face")
                ))
                .size(13),
            ]
            .spacing(3);
            for file in plan.files() {
                files = files
                    .push(
                        text(plan.destination(file))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    )
                    .push(
                        text(format!("{} bytes · SHA-256 {}", file.size(), file.digest()))
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
            }
            items = items.push(files);
        }
        if self.review_ready() {
            items = items
                .push(text(format!("Total model downloads · {bytes} bytes")))
                .push(
                    checkbox(self.accepted)
                        .label("I reviewed the licenses and approve these downloads")
                        .on_toggle_maybe(enabled.then_some(Message::Accept)),
                );
        }
        items.into()
    }

    fn install_view(&self) -> Element<'_, Message> {
        let tested = self.models.iter().filter(|model| model.tested).count();
        column![
            text(format!("{tested} of {} models tested", self.models.len())),
            self.progress.map(|progress| column![
                progress_bar(
                    0.0..=1.0,
                    progress.received as f32 / progress.total.max(1) as f32
                ),
                text(format!(
                    "{} / {} MiB",
                    progress.received / (1024 * 1024),
                    progress.total.div_ceil(1024 * 1024)
                ))
                .size(13),
            ]
            .spacing(5)),
            self.models
                .iter()
                .find(|model| !model.tested)
                .map(|model| text(model.recommendation.to_string()).size(13)),
        ]
        .spacing(8)
        .into()
    }

    fn finish_view(&self) -> Element<'_, Message> {
        let mut summary = column![text(
            "The selected models passed their local function tests. Role assignments were saved."
        )]
        .spacing(8);
        for (role, recommendation) in Role::ALL.into_iter().zip(&self.choices) {
            if let Some(recommendation) = recommendation {
                summary = summary.push(text(format!("{role} · {recommendation}")));
            }
        }
        for warning in &self.warnings {
            summary = summary.push(text(warning.as_str()).size(13));
        }
        summary.into()
    }
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Privacy => "1. Privacy",
            Self::Hardware => "2. Hardware and runtime",
            Self::Roles => "3. Choose models for roles",
            Self::License => "4. Review licenses and files",
            Self::Install => "5. Install and test",
            Self::Finish => "6. Local AI is ready",
        }
    }
}
