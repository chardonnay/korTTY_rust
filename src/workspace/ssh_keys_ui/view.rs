use std::fmt;

use iced::widget::{
    button, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length, Theme};

use super::*;

impl Workspace {
    pub(in crate::workspace) fn ssh_keys_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let manager = self.ssh_keys.native.as_ref()?;
        let vault = self.vault.as_ref();
        let view = if manager.focused_window() == id && manager.discard.is_some() {
            discard_view()
        } else if manager.id == id {
            manager.view()
        } else {
            match &manager.child.as_ref().filter(|child| child.id == id)?.kind {
                ChildKind::Edit(form) => form.view(vault),
                ChildKind::Delete { name, .. } => delete_view(name),
            }
        };
        Some(view.map(move |message| wrap(id, message)))
    }

    pub(in crate::workspace) fn hosted_ssh_keys_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let manager = self.ssh_keys.hosted.get(&id)?;
        if let Some(token) = manager.hosted_close.filter(|_| !manager.busy()) {
            return Some(discard_view().map(move |message| {
                let approve = matches!(message, Message::Discard);
                wrap(id, Message::HostedClose(token, approve))
            }));
        }
        let (surface, view) = if let Some(child) = &manager.child {
            let view = match &child.kind {
                ChildKind::Edit(form) => form.view(self.vault.as_ref()),
                ChildKind::Delete { name, .. } => delete_view(name),
            };
            (child.id, view)
        } else {
            (id, manager.view())
        };
        Some(view.map(move |message| wrap(surface, message)))
    }
}

impl Manager {
    fn view(&self) -> Element<'_, Message> {
        let interactive = self.ready() && self.child.is_none() && self.discard.is_none();
        let selected = self.selected();
        let mut list = column![].spacing(1).width(Length::Fill);
        let count = self.visible_count();
        for key in self
            .entries
            .iter()
            .filter(|key| self.filter.matches(key))
            .skip(self.page * PAGE)
            .take(PAGE)
        {
            let name = if key.description.is_empty() {
                key.name.clone()
            } else {
                format!(
                    "{} - {}",
                    key.name,
                    key.description.replace(['\r', '\n'], " ")
                )
            };
            let location = if key.path.managed() {
                "[Copied]".into()
            } else if matches!(key.path, PathEdit::Copy(_)) {
                "[Copy pending]".into()
            } else {
                key.path
                    .path()
                    .map_or_else(String::new, |path| format!("[{}]", path.display()))
            };
            let checked = self.selected == Some(key.id);
            list = list.push(
                button(text(format!("{name} {location}")).wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .padding([6, 8])
                    .style(move |theme: &Theme, state| {
                        if checked {
                            button::primary(theme, state)
                        } else {
                            button::text(theme, state)
                        }
                    })
                    .on_press_maybe(interactive.then_some(Message::Select(key.id))),
            );
        }
        if count == 0 {
            let message = if self.loading {
                "Loading SSH keys…"
            } else if self.entries.is_empty() {
                "No SSH keys stored."
            } else {
                "No SSH keys match this search."
            };
            list = list.push(container(text(message).size(14)).padding(8));
        }
        let list = container(scrollable(list).width(Length::Fill).height(Length::Fill))
            .style(container::bordered_box)
            .height(Length::Fill)
            .width(Length::Fill);
        let controls = row![
            button("Add").on_press_maybe(interactive.then_some(Message::Add)),
            button("Edit")
                .on_press_maybe((interactive && selected.is_some()).then_some(Message::Edit)),
            button("Delete")
                .on_press_maybe((interactive && selected.is_some()).then_some(Message::Delete)),
            button("Copy to User Directory").on_press_maybe(
                (interactive
                    && selected.is_some_and(|key| !key.path.managed()
                        && !matches!(key.path, PathEdit::Copy(_))
                        && key.path.path().is_some()))
                .then_some(Message::Copy)
            ),
            space::horizontal(),
            (count > PAGE).then(|| pagination(self.page, count, Message::Page)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        column![
            text("Manage your private SSH keys").size(22),
            row![text("Search:"), text_input("Search by name or path… (* as wildcard)", &self.filter.query)
                .on_input_maybe(interactive.then_some(Message::Search))].spacing(10).align_y(Alignment::Center),
            text("Stored SSH Keys:"),
            list,
            controls,
            text("Copy keys to the kortty user directory to take them with you when moving to another computer. Passphrases are stored encrypted in the vault.").size(13),
            self.status.as_ref().map(|status| text(status).size(13)),
            row![space::horizontal(),
                button(if self.saving { "Saving…" } else { "OK" }).on_press_maybe(interactive.then_some(Message::Accept)),
                button("Cancel").on_press_maybe((!self.saving && self.child.is_none()).then_some(Message::Cancel)),
            ].spacing(8),
        ].spacing(12).padding(20).height(Length::Fill).into()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct PassphraseChoice {
    id: Option<String>,
    label: String,
}

impl fmt::Display for PassphraseChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

impl Form {
    fn view(&self, vault: Option<&VaultWindow>) -> Element<'_, Message> {
        if let Some(browser) = &self.browser {
            return self.browser_view(browser);
        }
        let ready = vault.is_some_and(VaultWindow::can_manage_environments);
        let mut choices = vec![PassphraseChoice {
            id: None,
            label: "No stored passphrase".into(),
        }];
        if let Some(vault) = vault {
            choices.extend(
                vault
                    .choices(Kind::KeyPassphrase)
                    .into_iter()
                    .map(|choice| PassphraseChoice {
                        id: Some(choice.id),
                        label: choice.label,
                    }),
            );
        }
        if self.passphrase.is_some() && choices.iter().all(|choice| choice.id != self.passphrase) {
            choices.push(PassphraseChoice {
                id: self.passphrase.clone(),
                label: "Existing vault reference (locked or missing)".into(),
            });
        }
        let selected = choices
            .iter()
            .find(|choice| choice.id == self.passphrase)
            .cloned();
        let passphrase = row![
            pick_list(choices, selected, |choice| Message::Credential(choice.id))
                .width(Length::Fill),
            (!ready).then(|| button("Open Vault…").on_press(Message::OpenVault)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let secret = text_input("Passphrase (optional)", &self.new_passphrase)
            .secure(true)
            .on_input_maybe(ready.then_some(|value| Message::Passphrase(Password::new(value))));
        column![
            text(if self.key.is_some() {
                "Edit SSH Key"
            } else {
                "Add New SSH Key"
            })
            .size(22),
            field(
                "Name:",
                text_input("Name for this key", &self.name).on_input(Message::Name)
            ),
            field(
                "Key Path:",
                row![
                    text_input("Path to private SSH key", &self.path).on_input(Message::Path),
                    button("…").on_press(Message::Browse)
                ]
                .spacing(8)
            ),
            field("Passphrase:", column![passphrase, secret].spacing(7)),
            field(
                "Description:",
                text_editor(&self.description)
                    .placeholder("Description (optional)")
                    .height(90)
                    .on_action(Message::Description)
            ),
            self.problem.as_ref().map(|problem| text(problem).size(13)),
            space::vertical(),
            actions(Message::Accept, Message::Cancel),
        ]
        .spacing(10)
        .padding(20)
        .height(Length::Fill)
        .into()
    }

    fn browser_view<'a>(&self, browser: &'a Browser) -> Element<'a, Message> {
        let mut list = column![].spacing(1).width(Length::Fill);
        if let Some(directory) = &browser.result {
            for (index, entry) in directory
                .entries
                .iter()
                .enumerate()
                .skip(browser.page * PAGE)
                .take(PAGE)
            {
                let title = if entry.kind == EntryKind::Directory {
                    format!("{}/", entry.display)
                } else {
                    entry.display.clone()
                };
                let valid = matches!(entry.kind, EntryKind::Directory | EntryKind::File);
                list = list.push(
                    button(text(title))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press_maybe(
                            valid.then_some(Message::BrowseEntry(self.browse_revision, index)),
                        ),
                );
            }
        } else if browser.loading {
            list = list.push(text("Loading folder…"));
        }
        column![
            text("Select Private SSH Key").size(22),
            row![
                button("Parent folder").on_press_maybe(
                    (!browser.loading && browser.folder.parent().is_some())
                        .then_some(Message::BrowseUp)
                ),
                text(browser.folder.to_string_lossy()).wrapping(text::Wrapping::None)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            scrollable(list).height(Length::Fill),
            self.problem
                .as_ref()
                .map(|problem| text(problem.clone()).size(13)),
            row![
                button("Cancel").on_press(Message::BrowseBack),
                space::horizontal(),
                browser
                    .result
                    .as_ref()
                    .filter(|directory| directory.entries.len() > PAGE)
                    .map(|directory| pagination(
                        browser.page,
                        directory.entries.len(),
                        Message::BrowsePage
                    )),
            ]
            .align_y(Alignment::Center),
        ]
        .spacing(10)
        .padding(20)
        .height(Length::Fill)
        .into()
    }
}

fn delete_view(name: &str) -> Element<'_, Message> {
    column![
        text("Delete SSH Key").size(22),
        text(format!("Do you really want to delete '{name}'?")),
        space::vertical(),
        actions(Message::Accept, Message::Cancel),
    ]
    .spacing(10)
    .padding(20)
    .height(Length::Fill)
    .into()
}

fn discard_view() -> Element<'static, Message> {
    column![
        text("Discard SSH key changes?").size(22),
        space::vertical(),
        row![
            space::horizontal(),
            button("Keep Editing").on_press(Message::KeepEditing),
            button("Discard")
                .style(button::danger)
                .on_press(Message::Discard)
        ]
        .spacing(8),
    ]
    .spacing(10)
    .padding(20)
    .height(Length::Fill)
    .into()
}

fn field<'a>(label: &'static str, input: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).width(110), input.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn actions(accept: Message, cancel: Message) -> Element<'static, Message> {
    row![
        space::horizontal(),
        button("OK").on_press(accept),
        button("Cancel").on_press(cancel)
    ]
    .spacing(8)
    .into()
}

fn pagination<'a>(
    current: usize,
    count: usize,
    message: fn(bool) -> Message,
) -> Element<'a, Message> {
    let pages = count.div_ceil(PAGE);
    row![
        button("‹").on_press_maybe((current > 0).then_some(message(false))),
        text(format!("{} / {pages}", current + 1)).size(13),
        button("›").on_press_maybe((current + 1 < pages).then_some(message(true))),
    ]
    .spacing(5)
    .align_y(Alignment::Center)
    .into()
}
