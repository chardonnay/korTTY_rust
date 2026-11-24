//! Review and progress controls for the signed runtime release channel.

use std::sync::Arc;

use iced::widget::{button, column, container, pick_list, row, scrollable, space, text};
use iced::{Element, Length};

use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::online::{Catalog, Choice, Pause, Progress};

use super::{Message, managed::ReviewedModel};
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{Control, Reveal, Setting};
use crate::workspace::dialog_design;

pub(super) enum Review {
    Stable {
        upgrade: bool,
        required: Option<PackageBackend>,
        start: Vec<ReviewedModel>,
        approved: bool,
    },
    Versions,
}

impl Review {
    pub(super) fn title(&self) -> &'static str {
        match self {
            Self::Stable { upgrade: true, .. } => "Update llama.cpp runtime",
            Self::Stable { .. } => "Install llama.cpp runtime",
            Self::Versions => "Choose a llama.cpp version",
        }
    }

    pub(super) fn required(&self) -> Option<PackageBackend> {
        match self {
            Self::Stable { required, .. } => *required,
            Self::Versions => None,
        }
    }

    pub(super) fn height(&self) -> f32 {
        match self {
            Self::Stable { .. } => 212.0,
            Self::Versions => 378.0,
        }
    }
}

#[derive(Default)]
pub(super) struct State {
    pub(super) review: Option<Review>,
    pub(super) catalog: Option<Arc<Catalog>>,
    pub(super) selected: Option<Choice>,
    pub(super) progress: Option<Progress>,
    pub(super) pause: Option<Pause>,
    pub(super) discard_confirmed: bool,
    pub(super) pin_cleared: bool,
}

pub(super) fn check_button<'a>(enabled: bool, reveal: &Reveal) -> Element<'a, Message> {
    reveal.action(
        Setting::Control(Control::CheckRuntimeOnline),
        crate::workspace::dialog_design::secondary_action("Check signed runtime updates…")
            .on_press_maybe(enabled.then_some(Message::CheckRuntimeOnline)),
    )
}

impl State {
    pub(super) fn reset_catalog(&mut self) {
        self.catalog = None;
        self.selected = None;
        self.progress = None;
        self.pause = None;
        self.discard_confirmed = false;
        self.pin_cleared = false;
    }

    pub(super) fn has_pin(&self) -> bool {
        !self.pin_cleared
            && self
                .catalog
                .as_ref()
                .is_some_and(|catalog| catalog.pinned_installation().is_some())
    }

    pub(super) fn review_view<'a>(
        &'a self,
        status: Option<&'a str>,
        can_request: bool,
        downloads_allowed: bool,
        busy: bool,
        active: Option<&'a str>,
    ) -> Option<Element<'a, Message>> {
        let review = self.review.as_ref()?;
        let status = status
            .filter(|status| !status.is_empty())
            .map(|status| text(status).size(13).wrapping(text::Wrapping::WordOrGlyph));
        let progress = self.progress.map(|progress| {
            text(format!(
                "{} of {} MiB",
                progress.received.div_ceil(1_048_576),
                progress.total.div_ceil(1_048_576)
            ))
            .size(13)
        });
        let close = dialog_design::secondary_action("Cancel").on_press(Message::CloseRuntimeReview);
        let permission = (!downloads_allowed).then(|| {
            text(crate::clipboard_policy::AI_RUNTIME_DOWNLOADS_DISABLED)
                .size(13)
                .wrapping(text::Wrapping::WordOrGlyph)
        });
        let content: Element<'_, Message> = match review {
            Review::Stable {
                upgrade,
                required,
                start,
                approved,
            } => {
                let gguf = start
                    .iter()
                    .filter(|model| matches!(model, ReviewedModel::Gguf(_)))
                    .collect::<Vec<_>>();
                let description = if !gguf.is_empty() {
                    let models = match gguf.as_slice() {
                        [model] => format!("{} needs", model.name()),
                        models if models.len() == start.len() => {
                            format!("The {} selected models need", models.len())
                        }
                        models => format!("The {} selected GGUF models need", models.len()),
                    };
                    format!(
                        "{models} a compatible llama.cpp runtime. Install a signed package and continue?"
                    )
                } else if *upgrade {
                    "Check the stable channel for an update and install the newest compatible signed package?".into()
                } else {
                    "Install the newest compatible signed llama.cpp runtime?".into()
                };
                let (label, event) = if self.selected.is_some() {
                    ("Resume download", Message::InstallRuntimeOnline)
                } else {
                    (
                        if *approved {
                            "Check again"
                        } else {
                            "Install runtime"
                        },
                        Message::ConfirmStableRuntime,
                    )
                };
                column![
                    text(description).wrapping(text::Wrapping::WordOrGlyph),
                    required.map(|backend| text(format!("Required backend: {backend}")).size(13)),
                    permission,
                    status,
                    progress,
                    space::vertical().height(Length::Fill),
                    row![
                        dialog_design::secondary_action("Use signed files…").on_press_maybe(
                            (!busy && can_request).then_some(Message::OpenOfflineRuntimeFromReview)
                        ),
                        space::horizontal().width(Length::Fill),
                        self.can_pause()
                            .then(|| dialog_design::secondary_action("Pause")
                                .on_press(Message::PauseRuntimeDownload)),
                        button(label).on_press_maybe(
                            (!busy && can_request && downloads_allowed).then_some(event)
                        ),
                        close,
                    ]
                    .spacing(8)
                    .wrap(),
                ]
                .spacing(10)
                .into()
            }
            Review::Versions => {
                let rows = self.choices(None).map(|choice| {
                    let current = active == Some(choice.id.as_str());
                    let label = if current {
                        format!("{choice} · Active")
                    } else {
                        choice.to_string()
                    };
                    let selected = self.selected.as_ref() == Some(choice);
                    button(text(label).size(13))
                        .style(move |theme, state| dialog_design::tab(theme, state, selected))
                        .width(Length::Fill)
                        .on_press_maybe(
                            (!busy && can_request && downloads_allowed)
                                .then(|| Message::ChooseRuntimeOnline(choice.clone())),
                        )
                        .into()
                });
                column![
                    text("Choose a compatible version from the signed release channel. An older version stays pinned while it remains available.")
                        .wrapping(text::Wrapping::WordOrGlyph),
                    permission,
                    status,
                    progress,
                    container(scrollable(column(rows)).height(Length::Fill))
                        .style(dialog_design::sidebar).height(Length::Fill),
                    row![
                        dialog_design::secondary_action("Follow stable updates")
                            .on_press_maybe((!busy && can_request && downloads_allowed && self.has_pin()).then_some(Message::UnpinRuntime)),
                        space::horizontal().width(Length::Fill),
                        dialog_design::secondary_action("Pause")
                            .on_press_maybe(self.can_pause().then_some(Message::PauseRuntimeDownload)),
                        button("Install selected version")
                            .on_press_maybe((!busy && can_request && downloads_allowed && self.selected.is_some()).then_some(Message::InstallRuntimeOnline)),
                        close,
                    ].spacing(8).wrap(),
                ].spacing(10).into()
            }
        };
        Some(
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(14)
                .style(dialog_design::page)
                .into(),
        )
    }

    pub(super) fn can_pause(&self) -> bool {
        self.pause.is_some() && self.progress.is_some_and(|progress| progress.resumable)
    }

    fn selection(&self, required: Option<PackageBackend>) -> Option<&Choice> {
        self.selected
            .as_ref()
            .filter(|choice| required.is_none_or(|backend| backend == choice.backend))
    }

    fn choices(&self, required: Option<PackageBackend>) -> impl Iterator<Item = &Choice> {
        self.catalog
            .iter()
            .flat_map(|catalog| catalog.choices())
            .filter(move |choice| required.is_none_or(|backend| backend == choice.backend))
    }

    pub(super) fn has_search_control(
        &self,
        control: Control,
        enabled: bool,
        required: Option<PackageBackend>,
    ) -> bool {
        match control {
            Control::RuntimeOnlineChoice => {
                self.selection(required).is_some()
                    || enabled && self.choices(required).next().is_some()
            }
            Control::DownloadRuntimeOnline => self.selection(required).is_some(),
            Control::KeepRuntimeDownload => self.discard_confirmed,
            Control::DiscardRuntimeDownload => true,
            _ => false,
        }
    }

    pub(super) fn select(&mut self, choice: Choice, required: Option<PackageBackend>) {
        if self
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.choices().contains(&choice))
            && required.is_none_or(|backend| backend == choice.backend)
        {
            self.selected = Some(choice);
        }
    }

    pub(super) fn view(
        &self,
        enabled: bool,
        can_discard: bool,
        required: Option<PackageBackend>,
        reveal: &Reveal,
    ) -> Element<'_, Message> {
        let selected = self.selection(required);
        let choices: Vec<_> = self.choices(required).cloned().collect();
        let action = |control, button| reveal.action(Setting::Control(control), button);
        let picker: Option<Element<'_, Message>> = if enabled && !choices.is_empty() {
            Some(
                pick_list(choices, selected, Message::ChooseRuntimeOnline)
                    .placeholder("Choose a verified package")
                    .dismiss_on_escape(),
            )
        } else {
            selected.map(|choice| text(choice.to_string()).into())
        };
        column![
            text("Signed llama.cpp Release Channel").size(17),
            text("Check the stable korTTY runtime releases on GitHub and review a compatible package before downloading.").size(13),
            required.map(|_| check_button(enabled, reveal)),
            picker.map(|picker| reveal.wrap(Setting::Control(Control::RuntimeOnlineChoice), picker)),
            selected.map(|choice| column![
                text(format!("Download destination · {}", choice.url)).size(13).wrapping(text::Wrapping::WordOrGlyph),
                text(format!("{} bytes. Installation stages the archive and extracted runtime in the profile.", choice.size)).size(13),
                self.catalog.as_ref().filter(|catalog| catalog.pins(choice)).map(|_| {
                    text("Installing this older signed version pins it while the channel offers a compatible package. Installing the newest version follows stable updates again.").size(13).wrapping(text::Wrapping::WordOrGlyph)
                }),
                action(Control::DownloadRuntimeOnline, button("Download or resume selected runtime").on_press_maybe(enabled.then_some(Message::InstallRuntimeOnline))),
            ].spacing(6)),
            if self.discard_confirmed {
                row![
                    action(Control::DiscardRuntimeDownload, button("Discard saved ZIP").on_press_maybe(can_discard.then_some(Message::DiscardRuntimeDownload))),
                    action(Control::KeepRuntimeDownload, button("Keep saved ZIP").on_press(Message::ReviewRuntimeDownloadDiscard(false))),
                ].spacing(8)
            } else {
                row![action(Control::DiscardRuntimeDownload, button("Discard saved runtime download…").on_press_maybe(can_discard.then_some(Message::ReviewRuntimeDownloadDiscard(true))))].spacing(8)
            },
        ].spacing(8).into()
    }
}
