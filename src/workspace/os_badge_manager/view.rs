use iced::widget::{
    button, column, container, image, row, rule, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length};

use crate::sftp_window::{EntryKind, Location};
use crate::workspace::ToolIcon;

use super::{Browser, CatalogState, Dialog, Event, Mode, NAME_INPUT, PAGE_SIZE, Target};

impl Dialog {
    pub(super) fn view(&self) -> Element<'_, Event> {
        let content = match &self.mode {
            Mode::List => self.list_view(),
            Mode::Browse(browser) => self.browser_view(browser),
            Mode::Name { target, value } => self.name_view(target, value),
            Mode::ConfirmDelete { name, .. } => self.delete_view(name),
        };
        let mut body = column![
            container(text("Your own symbols, in addition to the built-in logos").size(19))
                .width(Length::Fill)
                .padding([10, 14]),
            rule::horizontal(1),
            container(content)
                .width(Length::Fill)
                .height(Length::Fill)
                .padding([10, 14]),
        ];
        if self.writing {
            body = body.push(container(text("Saving…").size(13)).padding([4, 14]));
        } else if self.catalog_state == CatalogState::Loading {
            body = body.push(container(text("Loading symbols…").size(13)).padding([4, 14]));
        } else if let Some(problem) = &self.problem {
            body = body.push(container(text(problem).size(13)).padding([4, 14]));
        }
        let content = body
            .push(rule::horizontal(1))
            .push(
                row![
                    space::horizontal(),
                    button("Close").on_press_maybe((!self.writing).then_some(Event::Close)),
                ]
                .padding([8, 14]),
            )
            .height(Length::Fill);
        container(content)
            .id(self.focus_scope.clone())
            .height(Length::Fill)
            .into()
    }

    fn list_view(&self) -> Element<'_, Event> {
        let ready = self.catalog_state == CatalogState::Ready;
        let mut items = column![].spacing(2);
        for badge in self.catalog.entries() {
            let selected = self.selected.as_deref() == Some(badge.reference());
            let symbol = container(
                badge
                    .image_handle()
                    .map(|handle| image(handle).width(28).height(28)),
            )
            .center_x(32)
            .center_y(32);
            items = items.push(
                button(
                    row![symbol, text(badge.name())]
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .style(if selected {
                    button::secondary
                } else {
                    button::text
                })
                .on_press_maybe(ready.then(|| Event::Select(badge.reference().into()))),
            );
        }
        if self.catalog.entries().is_empty() {
            items = items.push(container(text("No symbols of your own yet.")).padding(8));
        }
        let selected = ready && self.selected().is_some();
        column![
            container(scrollable(items).height(Length::Fill))
                .height(Length::Fill)
                .style(container::bordered_box),
            row![
                button(label(ToolIcon::Add, "Add…", ready)).on_press_maybe(ready.then_some(Event::Add)),
                button(label(ToolIcon::Editor, "Rename…", selected))
                    .on_press_maybe(selected.then_some(Event::Rename)),
                button("Delete").on_press_maybe(selected.then_some(Event::Delete)),
            ]
            .spacing(8),
            text("PNG, JPG, GIF or SVG. Square images look best. Connections whose symbol was deleted show none.")
                .size(13),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn browser_view(&self, browser: &Browser) -> Element<'_, Event> {
        let mut rows = column![].spacing(2);
        if let Some(directory) = &browser.directory {
            for (index, entry) in directory
                .entries
                .iter()
                .enumerate()
                .skip(browser.page * PAGE_SIZE)
                .take(PAGE_SIZE)
            {
                let title = if entry.kind == EntryKind::Directory {
                    format!("{}/", entry.display)
                } else {
                    entry.display.clone()
                };
                rows = rows.push(
                    button(text(title))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press(Event::Pick(self.generation, index)),
                );
            }
            if directory.entries.is_empty() {
                rows = rows.push(text("No supported image files or folders."));
            }
        } else if browser.loading {
            rows = rows.push(text("Loading folder…"));
        }
        let can_open = browser.absolute_path().is_some();
        column![
            text("Choose an image").size(18),
            row![
                button("Parent folder").on_press_maybe(
                    (!browser.loading && browser.folder.parent().is_some())
                        .then_some(Event::Parent)
                ),
                text(Location::Local(browser.folder.clone()).display())
                    .size(13)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::Glyph),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            scrollable(rows).height(Length::Fill),
            row![
                button("Previous").on_press_maybe((browser.page > 0).then_some(Event::Page(false))),
                button("Next").on_press_maybe(
                    (browser.page < browser.max_page()).then_some(Event::Page(true))
                ),
                space::horizontal(),
                button("Cancel").on_press(Event::Dismiss),
            ]
            .spacing(8),
            row![
                text_input("Absolute image path", &browser.path)
                    .on_input(Event::BrowsePath)
                    .on_submit_maybe(can_open.then_some(Event::OpenPath)),
                button("Open").on_press_maybe(can_open.then_some(Event::OpenPath)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .height(Length::Fill)
        .into()
    }

    fn name_view(&self, target: &Target, value: &str) -> Element<'_, Event> {
        let (title, accept) = match target {
            Target::Add(_) => ("Add OS symbol", "Add"),
            Target::Rename { .. } => ("Rename OS symbol", "Rename"),
        };
        let save = (!self.writing && !value.trim().is_empty()).then_some(Event::SaveName);
        column![
            text(title).size(18),
            text("Name"),
            text_input("OS symbol name", value)
                .id(NAME_INPUT)
                .on_input_maybe((!self.writing).then_some(Event::Name))
                .on_submit_maybe(save.clone()),
            row![
                button("Cancel").on_press_maybe((!self.writing).then_some(Event::Dismiss)),
                button(accept).on_press_maybe(save),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }

    fn delete_view(&self, name: &str) -> Element<'_, Event> {
        column![
            text(format!("Delete the symbol “{name}”?")).size(18),
            text("Connections that used this symbol will show none."),
            row![
                button("Cancel").on_press_maybe((!self.writing).then_some(Event::Dismiss)),
                button("Delete")
                    .style(button::danger)
                    .on_press_maybe((!self.writing).then_some(Event::ConfirmDelete)),
            ]
            .spacing(8),
        ]
        .spacing(8)
        .into()
    }
}

fn label(icon: ToolIcon, name: &'static str, enabled: bool) -> Element<'static, Event> {
    row![icon.widget(enabled, 16, None), text(name)]
        .spacing(5)
        .align_y(Alignment::Center)
        .into()
}
