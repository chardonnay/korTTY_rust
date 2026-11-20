//! Review and progress controls for the signed runtime release channel.

use std::sync::Arc;

use iced::Element;
use iced::widget::{button, column, pick_list, row, text};

use crate::local_ai::runtime_index::PackageBackend;
use crate::local_ai::runtime_package::online::{Catalog, Choice, Pause, Progress};

use super::Message;
use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{Control, Reveal, Setting};

#[derive(Default)]
pub(super) struct State {
    pub(super) catalog: Option<Arc<Catalog>>,
    pub(super) selected: Option<Choice>,
    pub(super) progress: Option<Progress>,
    pub(super) pause: Option<Pause>,
    pub(super) discard_confirmed: bool,
}

impl State {
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
            Control::CheckRuntimeOnline | Control::DiscardRuntimeDownload => true,
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
            action(Control::CheckRuntimeOnline, button("Check signed runtime updates…").on_press_maybe(enabled.then_some(Message::CheckRuntimeOnline))),
            picker.map(|picker| reveal.wrap(Setting::Control(Control::RuntimeOnlineChoice), picker)),
            selected.map(|choice| column![
                text(format!("Download destination · {}", choice.url)).size(13).wrapping(text::Wrapping::WordOrGlyph),
                text(format!("{} bytes. Installation stages the archive and extracted runtime in the profile.", choice.size)).size(13),
                self.catalog.as_ref().filter(|catalog| catalog.pins(choice)).map(|_| {
                    text("Installing this older signed version pins it while the channel offers a compatible package. Installing the newest version follows stable updates again.").size(13).wrapping(text::Wrapping::WordOrGlyph)
                }),
                action(Control::DownloadRuntimeOnline, button("Download or resume selected runtime").on_press_maybe(enabled.then_some(Message::InstallRuntimeOnline))),
            ].spacing(6)),
            reveal.wrap(Setting::Control(Control::DiscardRuntimeDownload), if self.discard_confirmed {
                row![
                    button("Discard saved ZIP").on_press_maybe(can_discard.then_some(Message::DiscardRuntimeDownload)),
                    button("Keep saved ZIP").on_press(Message::ReviewRuntimeDownloadDiscard(false)),
                ].spacing(8)
            } else {
                row![button("Discard saved runtime download…").on_press_maybe(can_discard.then_some(Message::ReviewRuntimeDownloadDiscard(true)))].spacing(8)
            }),
        ].spacing(8).into()
    }
}
