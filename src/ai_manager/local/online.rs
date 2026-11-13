//! Review and progress controls for the signed runtime release channel.

use std::sync::Arc;

use iced::Element;
use iced::widget::{button, column, pick_list, row, text};

use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::online::{Catalog, Choice, Pause, Progress};

use super::Message;
use crate::ai_manager::profiles::picker::DismissOnEscape as _;

#[derive(Default)]
pub(super) struct State {
    pub(super) catalog: Option<Arc<Catalog>>,
    pub(super) selected: Option<Choice>,
    pub(super) progress: Option<Progress>,
    pub(super) pause: Option<Pause>,
    pub(super) discard_confirmed: bool,
}

impl State {
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
    ) -> Element<'_, Message> {
        let selected = self
            .selected
            .as_ref()
            .filter(|choice| required.is_none_or(|backend| backend == choice.backend));
        let choices: Vec<_> = self
            .catalog
            .iter()
            .flat_map(|catalog| catalog.choices())
            .filter(|choice| required.is_none_or(|backend| backend == choice.backend))
            .cloned()
            .collect();
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
            button("Check signed runtime updates…").on_press_maybe(enabled.then_some(Message::CheckRuntimeOnline)),
            picker,
            selected.map(|choice| column![
                text(format!("Download destination · {}", choice.url)).size(13).wrapping(text::Wrapping::WordOrGlyph),
                text(format!("{} bytes. Installation stages the archive and extracted runtime in the profile.", choice.size)).size(13),
                self.catalog.as_ref().filter(|catalog| catalog.pins(choice)).map(|_| {
                    text("Installing this older signed version pins it while the channel offers a compatible package. Installing the newest version follows stable updates again.").size(13).wrapping(text::Wrapping::WordOrGlyph)
                }),
                button("Download or resume selected runtime").on_press_maybe(enabled.then_some(Message::InstallRuntimeOnline)),
            ].spacing(6)),
            if self.discard_confirmed {
                row![
                    button("Discard saved ZIP").on_press_maybe(can_discard.then_some(Message::DiscardRuntimeDownload)),
                    button("Keep saved ZIP").on_press(Message::ReviewRuntimeDownloadDiscard(false)),
                ].spacing(8)
            } else {
                row![button("Discard saved runtime download…").on_press_maybe(can_discard.then_some(Message::ReviewRuntimeDownloadDiscard(true)))].spacing(8)
            },
        ].spacing(8).into()
    }
}
