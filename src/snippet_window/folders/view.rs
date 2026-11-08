//! Native folder navigation above the snippet list.

use std::collections::HashMap;

use iced::advanced::text::Wrapping;
use iced::widget::{
    button, canvas, checkbox, column, container, responsive, row, scrollable, space, text, tooltip,
};
use iced::{Alignment, Element, Length};

use crate::horizontal_divider::{self, Grow};
use crate::snippet_window::Message as WindowMessage;
use crate::snippets::Library;
use crate::snippets::folders::{Folder, Id};

use super::{MenuTarget, Message, Selection, State, drag};

mod keyboard;

const INITIAL_TREE_RATIO: f32 = 0.3;
const MIN_TREE_HEIGHT: f32 = 82.0;
const MIN_LIST_HEIGHT: f32 = 64.0;
const DISCLOSURE_WIDTH: f32 = 19.0;
const INDENT: f32 = 10.0;

pub(in crate::snippet_window) fn library_split<'a>(
    state: &'a State,
    library: &'a Library,
    zone: &drag::Zone,
    transfer_enabled: bool,
    snippets: impl Fn() -> Element<'a, WindowMessage> + 'a,
) -> Element<'a, WindowMessage> {
    let tree = Tree::new(library);
    let zone = zone.clone();
    responsive(move |size| {
        let content_height = (size.height - horizontal_divider::WIDTH).max(0.0);
        // On a short window, share the available height and keep the folder
        // footer and both scrollable lists reachable.
        let minimum = MIN_TREE_HEIGHT.min(content_height * 0.58);
        let maximum = (content_height - MIN_LIST_HEIGHT.min(content_height * 0.42)).max(minimum);
        let preferred = state.tree_height;
        let height = preferred
            .unwrap_or(size.height * INITIAL_TREE_RATIO)
            .clamp(minimum, maximum);
        let divider = canvas(horizontal_divider::program(
            height,
            Grow::Down,
            move |extent| {
                let requested = extent.clamp(minimum, maximum);
                let extent = if requested == height {
                    preferred.unwrap_or(height)
                } else {
                    requested
                };
                wrap(Message::ResizeTree(extent))
            },
            None,
        ))
        .width(Length::Fill)
        .height(horizontal_divider::WIDTH);
        column![
            container(tree.view(state, library, &zone, transfer_enabled))
                .height(height)
                .width(Length::Fill)
                .clip(true),
            divider,
            container(snippets())
                .height(Length::Fill)
                .width(Length::Fill),
        ]
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    })
    .into()
}

struct Tree<'a> {
    children: HashMap<Option<Id>, Vec<&'a Folder>>,
    counts: HashMap<Id, usize>,
    top_level: usize,
}

impl<'a> Tree<'a> {
    fn new(library: &'a Library) -> Self {
        let by_path: HashMap<_, _> = library
            .folders()
            .iter()
            .map(|folder| (folder.path.as_str(), folder.id))
            .collect();
        let mut children: HashMap<_, Vec<_>> = HashMap::new();
        for folder in library.folders() {
            let parent = parent_path(&folder.path).and_then(|path| by_path.get(path).copied());
            children.entry(parent).or_default().push(folder);
        }

        let mut counts = HashMap::new();
        let mut top_level = 0;
        for entry in library.entries() {
            let Some(folder) = library.folder_id(entry).and_then(|id| library.folder(id)) else {
                top_level += 1;
                continue;
            };
            *counts.entry(folder.id).or_default() += 1;
            let mut ancestor = parent_path(&folder.path);
            while let Some(path) = ancestor {
                if let Some(&id) = by_path.get(path) {
                    *counts.entry(id).or_default() += 1;
                }
                ancestor = parent_path(path);
            }
        }
        Self {
            children,
            counts,
            top_level,
        }
    }

    fn view(
        &self,
        state: &'a State,
        library: &'a Library,
        zone: &drag::Zone,
        transfer_enabled: bool,
    ) -> Element<'a, WindowMessage> {
        let mut rows = column![
            entry(
                state,
                library,
                zone,
                Selection::All,
                transfer_enabled,
                "All snippets",
                library.entries().len(),
            ),
            entry(
                state,
                library,
                zone,
                Selection::TopLevel,
                transfer_enabled,
                "Top level (no folder)",
                self.top_level,
            ),
        ]
        .spacing(1);
        let mut keyboard_rows = vec![
            keyboard::Row::new(Selection::All, None, None),
            keyboard::Row::new(Selection::TopLevel, Some(Selection::All), None),
        ];
        let mut pending = self
            .children
            .get(&None)
            .into_iter()
            .flatten()
            .rev()
            .map(|&folder| (folder, 0_u16, Selection::All))
            .collect::<Vec<_>>();
        while let Some((folder, depth, parent)) = pending.pop() {
            let selection = Selection::Folder(folder.id);
            let expanded = state.expanded.contains(&folder.id);
            let children = self.children.get(&Some(folder.id));
            keyboard_rows.push(keyboard::Row::new(
                selection,
                Some(parent),
                children.map(|_| expanded),
            ));
            let disclosure: Element<'_, WindowMessage> = match children {
                Some(children) => {
                    if expanded {
                        pending.extend(
                            children
                                .iter()
                                .rev()
                                .map(|&child| (child, depth + 1, selection)),
                        );
                    }
                    button(if expanded { "▾" } else { "▸" })
                        .width(DISCLOSURE_WIDTH)
                        .padding(1)
                        .style(button::text)
                        .on_press(wrap(Message::Toggle(folder.id)))
                        .into()
                }
                None => space::horizontal().width(DISCLOSURE_WIDTH).into(),
            };
            let name = folder.path.rsplit('/').next().unwrap_or(&folder.path);
            let count = self.counts.get(&folder.id).copied().unwrap_or(0);
            rows = rows.push(
                row![
                    space::horizontal().width(f32::from(depth.min(10)) * INDENT),
                    disclosure,
                    entry(
                        state,
                        library,
                        zone,
                        selection,
                        transfer_enabled,
                        name,
                        count
                    ),
                ]
                .align_y(Alignment::Center),
            );
        }

        let footer = row![
            tooltip(
                checkbox(state.include_subfolders)
                    .label("Include sub-folders")
                    .size(14)
                    .text_size(12)
                    .on_toggle(|include| wrap(Message::IncludeSubfolders(include))),
                "Include snippets from sub-folders of the selected folder",
                tooltip::Position::Top,
            ),
            space::horizontal(),
            button(text("New folder…").size(12))
                .padding([4, 6])
                .on_press(wrap(Message::NewFolder(state.selection))),
        ]
        .spacing(4)
        .align_y(Alignment::Center);
        column![
            keyboard::tree(
                scrollable(rows)
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .into(),
                keyboard_rows,
                state.selection,
                state.menu_open(),
            ),
            footer
        ]
        .spacing(4)
        .height(Length::Fill)
        .width(Length::Fill)
        .into()
    }
}

fn entry<'a>(
    state: &'a State,
    library: &'a Library,
    zone: &drag::Zone,
    selection: Selection,
    transfer_enabled: bool,
    name: &'a str,
    count: usize,
) -> Element<'a, WindowMessage> {
    let label = button(
        row![
            container(text(name).size(13).wrapping(Wrapping::None))
                .width(Length::Fill)
                .clip(true),
            text(count).size(12),
        ]
        .spacing(4)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([3, 5])
    .clip(true)
    .style(if state.selection == selection {
        button::primary
    } else {
        button::text
    })
    .on_press(wrap(Message::Select(selection)));
    let (source, folder) = match selection {
        Selection::Folder(id) => (Some(drag::Source::Folder(id)), Some(id)),
        Selection::All | Selection::TopLevel => (None, None),
    };
    let label: Element<'a, WindowMessage> = label.into();
    let target = state.context(
        library,
        MenuTarget::Folder(selection),
        transfer_enabled,
        label,
    );
    zone.item(target, source, Some(folder))
}

fn parent_path(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

fn wrap(message: Message) -> WindowMessage {
    WindowMessage::Folders(message)
}
