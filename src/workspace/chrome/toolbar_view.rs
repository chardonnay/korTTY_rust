//! Native toolbar rows and their in-window customization panel.

use std::borrow::Cow;

use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, row, scrollable, slider, space,
    text, text_input, tooltip,
};
use iced::{Alignment, Element, Length, Task, mouse, window};

use super::toolbar_settings::{
    Catalog, Draft, Edit, Group, Item, Layout, Location, MAX_ICON, MIN_ICON, Row,
};
use super::{
    Action, Category, ChromeColors, Icon, Message, Navigation, OpenToolbarOverflow, ToolbarMessage,
    Workspace, WorkspaceMessage, button_style, divider, menu_popup, next_item, popup, toolbar_drag,
    toolbar_strip, wrap,
};

impl Workspace {
    pub(in crate::workspace) fn icon_toolbar(
        &self,
        id: window::Id,
    ) -> Element<'_, WorkspaceMessage> {
        let colors = self.chrome_colors();
        let toolbar = &self.chrome.toolbar;
        let layout = toolbar.layout(id);
        let draft = toolbar.draft(id);
        let mut body = column![].spacing(4);
        let mut has_action_row = false;
        if layout.actions_visible() || draft.is_some() {
            let mut actions = row![].spacing(8);
            for group in Group::ACTIONS {
                if draft.is_some() || (layout.visible(group) && !layout.items(group).is_empty()) {
                    actions = actions.push(self.toolbar_group(id, layout, group, draft));
                    has_action_row = true;
                }
            }
            if has_action_row {
                body = body.push(actions.wrap().vertical_spacing(4));
            } else {
                body = body.push(toolbar_button(
                    "Customize toolbar…",
                    open(id, Catalog::Actions),
                    colors,
                ));
                has_action_row = true;
            }
        }
        let has_connection_row = layout.visible(Group::Connections) || draft.is_some();
        if has_connection_row {
            if has_action_row {
                body = body.push(divider(colors.toolbar_hover));
            }
            body = body.push(self.connection_toolbar(id, layout, draft, colors));
        }
        if let Some(draft) = draft {
            body = body
                .push(divider(colors.toolbar_hover))
                .push(self.toolbar_editor(id, draft));
        }
        if !has_action_row && !has_connection_row && draft.is_none() {
            return space::vertical().height(0).into();
        }
        let content = container(body)
            .padding(iced::Padding {
                top: 6.0,
                right: 10.0,
                bottom: 5.0,
                left: 10.0,
            })
            .width(Length::Fill)
            .style(move |_| container::Style::default().background(colors.toolbar));
        if draft.is_some() {
            toolbar_drag::surface(content.into(), edit(id, Edit::CancelDrag))
        } else {
            let trigger = mouse_area(content)
                .on_right_press(wrap(Message::OpenMenu(id, Category::Toolbar)))
                .into();
            if let Some(menu) = self
                .chrome
                .menu
                .as_ref()
                .filter(|menu| menu.window == id && menu.category == Category::Toolbar)
                && let Some(window) = self.windows.get(&id)
            {
                popup::anchor(
                    trigger,
                    self.menu_items(window, *menu, None),
                    wrap(Message::Dismiss(id)),
                    move |navigation| wrap(Message::Navigate(id, navigation)),
                )
            } else {
                trigger
            }
        }
    }

    fn connection_toolbar<'a>(
        &'a self,
        id: window::Id,
        layout: &'a Layout,
        draft: Option<&Draft>,
        colors: ChromeColors,
    ) -> Element<'a, WorkspaceMessage> {
        let group = Group::Connections;
        let heading = text(group.label()).size(11).color(colors.foreground);
        let mut items = self.toolbar_items(id, layout, group, draft);
        let contents = row![heading].spacing(12).align_y(Alignment::Center);
        if let Some(draft) = draft {
            items.push(drop_end(id, group, draft, colors));
            return items
                .into_iter()
                .fold(contents, |row, item| row.push(item))
                .wrap()
                .into();
        }
        let buttons = if items.is_empty() {
            button(
                row![
                    Icon::Add.widget(true, layout.icon_size(), colors.icon),
                    text("Add connection").size(12)
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            )
            .on_press_maybe(
                self.chrome
                    .toolbar
                    .can_edit(id)
                    .then_some(open(id, Catalog::Connections)),
            )
            .style(move |_, status| button_style(status, colors.toolbar_hover, false, colors))
            .into()
        } else {
            self.toolbar_strip(id, layout, group, items, colors)
        };
        contents.push(buttons).into()
    }

    fn toolbar_group<'a>(
        &'a self,
        id: window::Id,
        layout: &'a Layout,
        group: Group,
        draft: Option<&Draft>,
    ) -> Element<'a, WorkspaceMessage> {
        let colors = self.chrome_colors();
        let mut items = self.toolbar_items(id, layout, group, draft);
        let buttons: Element<'_, WorkspaceMessage> = if let Some(draft) = draft {
            items.push(drop_end(id, group, draft, colors));
            row(items)
                .spacing(6)
                .align_y(Alignment::Center)
                .wrap()
                .into()
        } else {
            self.toolbar_strip(id, layout, group, items, colors)
        };
        let heading_color = if layout.visible(group) {
            colors.foreground
        } else {
            colors.muted
        };
        row![
            column![text(group.label()).size(11).color(heading_color), buttons]
                .spacing(2)
                .padding(iced::padding::left(8)),
            vertical_separator(f32::from(layout.icon_size()) + 27.0, colors),
        ]
        .spacing(8)
        .into()
    }

    fn toolbar_items(
        &self,
        id: window::Id,
        layout: &Layout,
        group: Group,
        draft: Option<&Draft>,
    ) -> Vec<Element<'_, WorkspaceMessage>> {
        layout
            .items(group)
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.toolbar_item(id, layout, draft, Location { group, index }, item)
            })
            .collect()
    }

    fn toolbar_strip<'a>(
        &'a self,
        id: window::Id,
        layout: &'a Layout,
        group: Group,
        items: Vec<Element<'a, WorkspaceMessage>>,
        colors: ChromeColors,
    ) -> Element<'a, WorkspaceMessage> {
        toolbar_strip::strip(items, move |first_hidden| {
            let active = self.chrome.toolbar_overflow.filter(|overflow| {
                overflow.window == id
                    && overflow.group == group
                    && overflow.first_hidden == first_hidden
            });
            let trigger = tooltip(
                button(text("»").size(16))
                    .width(toolbar_strip::TRIGGER_WIDTH)
                    .height(f32::from(layout.icon_size()) + 10.0)
                    .padding(0)
                    .style(move |_, status| {
                        button_style(status, colors.toolbar_hover, active.is_some(), colors)
                    })
                    .on_press(wrap(Message::OpenToolbarOverflow(id, group, first_hidden))),
                container(text(format!("More {} items", group.label())).size(12))
                    .padding(6)
                    .style(move |_| {
                        container::Style::default()
                            .background(colors.menu)
                            .color(colors.foreground)
                    }),
                tooltip::Position::Bottom,
            )
            .into();
            if let Some(overflow) = active {
                popup::anchor(
                    trigger,
                    self.toolbar_overflow_menu(overflow, layout, colors),
                    wrap(Message::Dismiss(id)),
                    move |direction| wrap(Message::NavigateToolbarOverflow(id, group, direction)),
                )
            } else {
                trigger
            }
        })
    }

    fn toolbar_overflow_menu(
        &self,
        overflow: OpenToolbarOverflow,
        layout: &Layout,
        colors: ChromeColors,
    ) -> Element<'_, WorkspaceMessage> {
        let OpenToolbarOverflow {
            window: id,
            group,
            first_hidden,
            selected,
        } = overflow;
        let rows = layout
            .items(group)
            .iter()
            .enumerate()
            .skip(first_hidden)
            .fold(column![].spacing(2), |rows, (index, item)| {
                if matches!(item, Item::Separator) {
                    return rows.push(divider(colors.menu_hover));
                }
                let (icon, label) = self.toolbar_presentation(item);
                let enabled = self.toolbar_item_action(id, item).is_some();
                let (checked, shortcut) = if let Item::Action(action) = item {
                    (
                        self.windows
                            .get(&id)
                            .is_some_and(|window| self.chrome_checked(id, window, *action)),
                        self.window_settings.key_bindings.menu_hint(*action),
                    )
                } else {
                    (false, None)
                };
                rows.push(
                    button(
                        row![
                            text(if checked { "✓" } else { "" }).width(15),
                            icon.widget(enabled, 18, colors.icon),
                            text(label.into_owned()).size(13).width(Length::Fill),
                            shortcut.map(|shortcut| text(shortcut).size(11).color(colors.muted)),
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([5, 6])
                    .style(move |_, status| {
                        button_style(status, colors.menu_hover, selected == Some(index), colors)
                    })
                    .on_press_maybe(
                        enabled.then_some(wrap(Message::InvokeToolbarOverflow(id, group, index))),
                    ),
                )
            });
        menu_popup(rows, colors)
    }

    pub(super) fn open_toolbar_overflow(
        &mut self,
        id: window::Id,
        group: Group,
        first_hidden: usize,
    ) {
        let toolbar = &self.chrome.toolbar;
        let layout = toolbar.layout(id);
        if !self.palette_can_open(id)
            || toolbar.draft(id).is_some()
            || !layout.visible(group)
            || (group != Group::Connections && !layout.actions_visible())
            || first_hidden >= layout.items(group).len()
        {
            return;
        }
        self.dismiss_terminal_context(id);
        self.chrome.menu = None;
        self.chrome.toolbar_overflow = Some(OpenToolbarOverflow {
            window: id,
            group,
            first_hidden,
            selected: None,
        });
    }

    pub(super) fn navigate_toolbar_overflow(
        &mut self,
        id: window::Id,
        group: Group,
        direction: Navigation,
    ) -> Task<WorkspaceMessage> {
        let Some(mut overflow) = self
            .chrome
            .toolbar_overflow
            .filter(|overflow| overflow.window == id && overflow.group == group)
        else {
            return Task::none();
        };
        match direction {
            Navigation::Escape | Navigation::Left => self.chrome.dismiss_toolbar_overflow(id),
            Navigation::Activate => {
                if let Some(index) = overflow.selected {
                    return self.invoke_toolbar_overflow(id, group, index);
                }
            }
            Navigation::Next | Navigation::Previous => {
                let layout = self.chrome.toolbar.layout(id);
                let enabled: Vec<_> = layout
                    .items(group)
                    .iter()
                    .enumerate()
                    .skip(overflow.first_hidden)
                    .filter_map(|(index, item)| self.toolbar_item_action(id, item).map(|_| index))
                    .collect();
                overflow.selected = next_item(
                    &enabled,
                    overflow.selected,
                    matches!(direction, Navigation::Next),
                );
                self.chrome.toolbar_overflow = Some(overflow);
            }
            Navigation::Right => {}
        }
        Task::none()
    }

    pub(super) fn invoke_toolbar_overflow(
        &mut self,
        id: window::Id,
        group: Group,
        index: usize,
    ) -> Task<WorkspaceMessage> {
        if !self.chrome.toolbar_overflow.is_some_and(|overflow| {
            overflow.window == id && overflow.group == group && index >= overflow.first_hidden
        }) || !self.palette_can_open(id)
        {
            return Task::none();
        }
        let action = self
            .chrome
            .toolbar
            .layout(id)
            .items(group)
            .get(index)
            .and_then(|item| self.toolbar_item_action(id, item));
        if let Some(action) = action {
            self.chrome.dismiss_toolbar_overflow(id);
            return self.update(action);
        }
        Task::none()
    }

    fn toolbar_item(
        &self,
        id: window::Id,
        layout: &Layout,
        draft: Option<&Draft>,
        location: Location,
        item: &Item,
    ) -> Element<'_, WorkspaceMessage> {
        let colors = self.chrome_colors();
        if matches!(item, Item::Separator) && draft.is_none() {
            return vertical_separator(f32::from(layout.icon_size()), colors);
        }
        let (icon, label) = self.toolbar_presentation(item);
        let editing = draft.is_some();
        let editable = editing && !self.chrome.toolbar.pending();
        let action = if editing {
            None
        } else {
            self.toolbar_item_action(id, item)
        };
        let checked = if let Some(draft) = draft {
            draft.selected == Some(location)
        } else if let Item::Action(action) = item {
            self.windows
                .get(&id)
                .is_some_and(|window| self.chrome_checked(id, window, *action))
        } else {
            false
        };
        let enabled = editable || action.is_some();
        let glyph = icon.widget(enabled, layout.icon_size(), colors.icon);
        let content: Element<'_, WorkspaceMessage> = if location.group == Group::Connections {
            row![glyph, text(label.clone().into_owned()).size(12)]
                .spacing(6)
                .align_y(Alignment::Center)
                .into()
        } else {
            glyph.into()
        };
        let hint = match item {
            _ if editing => format!(
                "{label}\nDrag to move; click for options. Alt+Left/Right moves; Delete removes."
            ),
            Item::Action(action) if !editing => self
                .window_settings
                .key_bindings
                .menu_hint(*action)
                .map_or_else(
                    || label.to_string(),
                    |shortcut| format!("{label}\n{shortcut}"),
                ),
            _ => label.into_owned(),
        };
        let trigger = tooltip(
            button(content)
                .padding([5, 8])
                .style(move |_, status| {
                    let status = if editable {
                        button::Status::Active
                    } else {
                        status
                    };
                    button_style(status, colors.toolbar_hover, checked, colors)
                })
                .on_press_maybe(action),
            container(text(hint).size(12)).padding(6).style(move |_| {
                container::Style::default()
                    .background(colors.menu)
                    .color(colors.foreground)
            }),
            tooltip::Position::Bottom,
        );
        if let Some(draft) = draft.filter(|_| editable) {
            let target = draft.item_context.target(location);
            let focus = (draft.selected == Some(location)).then(|| draft.item_context.focus_id());
            let drag = mouse_area(trigger)
                .interaction(mouse::Interaction::Grab)
                .on_press(edit(id, Edit::Pick(location)))
                .on_release(edit(id, Edit::Drop(location.group, Some(location.index))))
                .into();
            let trigger = toolbar_drag::item_surface(drag, id, target.clone(), focus);
            draft.item_menu(id, target, trigger, colors)
        } else {
            trigger.into()
        }
    }

    fn toolbar_editor<'a>(
        &'a self,
        id: window::Id,
        draft: &'a Draft,
    ) -> Element<'a, WorkspaceMessage> {
        let colors = self.chrome_colors();
        let toolbar = &self.chrome.toolbar;
        let can_edit = !toolbar.pending();
        let layout = &draft.layout;
        let window_height = self.windows[&id].content_bounds().height;
        let compact = window_height < 530.0;
        let icon_size = layout.icon_size();
        let mut visibility = row![
            visibility_check(
                id,
                "Action toolbar",
                Row::Actions,
                layout.actions_visible(),
                can_edit
            ),
            visibility_check(
                id,
                "Connection toolbar",
                Row::Group(Group::Connections),
                layout.visible(Group::Connections),
                can_edit
            ),
        ]
        .spacing(12);
        for group in Group::ACTIONS {
            visibility = visibility.push(visibility_check(
                id,
                group.label(),
                Row::Group(group),
                layout.visible(group),
                can_edit,
            ));
        }
        let controls = row![
            text("Toolbar icon size").size(12).color(colors.foreground),
            slider(MIN_ICON..=MAX_ICON, layout.icon_size(), move |size| edit(
                id,
                Edit::IconSize(size)
            ))
            .width(170),
            text(format!("{} px", layout.icon_size()))
                .size(12)
                .color(colors.foreground),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut categories = column![].width(145).spacing(2);
        for catalog in Catalog::ALL {
            categories = categories.push(
                button(text(catalog.label()).size(12))
                    .width(Length::Fill)
                    .padding([4, 8])
                    .style(move |_, status| {
                        button_style(
                            status,
                            colors.toolbar_hover,
                            draft.catalog == catalog,
                            colors,
                        )
                    })
                    .on_press_maybe(can_edit.then_some(edit(id, Edit::Catalog(catalog)))),
            );
        }
        let items = self.toolbar_catalog(draft.catalog);
        let query = draft.search.trim().to_lowercase();
        let tile_width = if draft.catalog == Catalog::Connections {
            175.0
        } else {
            150.0
        };
        let can_add = can_edit && layout.has_room();
        let mut found = false;
        let mut results = row![].spacing(8).padding(4);
        for (group, item) in items {
            let (icon, label) = self.toolbar_presentation(&item);
            if !label.to_lowercase().contains(&query) {
                continue;
            }
            found = true;
            let result = column![
                icon.widget(can_add, icon_size, colors.icon),
                text(label.into_owned())
                    .size(12)
                    .width(Length::Fill)
                    .center()
                    .wrapping(text::Wrapping::WordOrGlyph),
            ]
            .spacing(8)
            .align_x(Alignment::Center);
            let result = container(result).width(tile_width).padding(8);
            results = results.push(catalog_source(id, result, group, item, can_add, colors));
        }
        let results: Element<'_, WorkspaceMessage> = if !found {
            text(if draft.catalog == Catalog::Connections {
                "No matching entries. Create servers and groups in the Connection Manager."
            } else {
                "No matching entries."
            })
            .size(12)
            .color(colors.muted)
            .into()
        } else if compact {
            results.wrap().into()
        } else {
            let row_height = (f32::from(icon_size) + 41.0).max(65.0);
            scrollable(results.wrap())
                .id(draft.catalog_scroll.clone())
                .height((2.0 * row_height + 16.0).min(window_height * 0.29))
                .into()
        };
        let catalog = row![
            categories,
            column![
                text_input("Search actions…", &draft.search)
                    .id(draft.search_input.clone())
                    .on_input_maybe(can_edit.then_some(move |query| edit(id, Edit::Search(query)))),
                results,
            ]
            .width(Length::Fill)
            .spacing(8),
        ]
        .spacing(12);
        let catalog_actions = row![
            button(
                row![
                    Icon::Add.widget(can_edit, icon_size, colors.icon),
                    text("Add connection")
                ]
                .spacing(6)
                .align_y(Alignment::Center)
            )
            .style(button::secondary)
            .on_press_maybe(can_edit.then_some(open(id, Catalog::Connections))),
            catalog_source(
                id,
                container(
                    row![
                        Icon::Separator.widget(can_add, icon_size, colors.icon),
                        text("Separator")
                    ]
                    .spacing(6)
                    .align_y(Alignment::Center)
                )
                .padding([5, 8]),
                Group::Tools,
                Item::Separator,
                can_add,
                colors,
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let mut selection = row![].spacing(5).align_y(Alignment::Center);
        if let Some((location, item)) = draft.selected_item() {
            let (_, label) = self.toolbar_presentation(item);
            let targets: Vec<_> = Group::ALL
                .into_iter()
                .filter(|&group| group != location.group && item.accepts(group))
                .collect();
            selection = selection.push(text(label.into_owned()).size(12).color(colors.foreground));
            selection = selection.push(button("Move left").on_press_maybe(
                (can_edit && location.index > 0).then_some(edit(id, Edit::Step(false))),
            ));
            selection = selection.push(
                button("Move right").on_press_maybe(
                    (can_edit && location.index + 1 < layout.items(location.group).len())
                        .then_some(edit(id, Edit::Step(true))),
                ),
            );
            if !targets.is_empty() {
                selection = selection.push(
                    pick_list(targets, None::<Group>, move |group| {
                        edit(id, Edit::Move(group))
                    })
                    .placeholder("Move to section"),
                );
            }
            selection = selection.push(
                button("Remove button").on_press_maybe(can_edit.then_some(edit(id, Edit::Remove))),
            );
        }
        let footer = row![
            button("Default")
                .style(button::secondary)
                .on_press_maybe(can_edit.then_some(edit(id, Edit::Reset))),
            space::horizontal(),
            button("Cancel")
                .style(button::secondary)
                .on_press_maybe(can_edit.then_some(message(ToolbarMessage::Cancel(id)))),
            button(if can_edit { "Finish" } else { "Saving…" })
                .on_press_maybe(can_edit.then_some(message(ToolbarMessage::Finish(id)))),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let mut options = column![
            catalog,
            catalog_actions.wrap(),
            visibility.wrap(),
            controls.wrap()
        ]
        .spacing(8);
        if draft.selected.is_some() {
            options = options.push(selection.wrap());
        }
        if !layout.has_room() {
            options = options.push(
                text("Toolbar item limit reached.")
                    .size(12)
                    .color(colors.muted),
            );
        }
        if let Some(problem) = toolbar.problem() {
            options = options.push(text(problem).size(12).color(colors.foreground));
        }
        let options: Element<'_, WorkspaceMessage> = if compact {
            container(scrollable(options).id(draft.catalog_scroll.clone()))
                .max_height(window_height * 0.32)
                .into()
        } else {
            options.into()
        };
        column![options, footer].padding([4, 0]).spacing(8).into()
    }

    fn toolbar_catalog(&self, catalog: Catalog) -> Vec<(Group, Item)> {
        use Action::*;
        match catalog {
            Catalog::Connections => self
                .chrome
                .toolbar
                .connections()
                .iter()
                .map(|connection| (Group::Connections, Item::Connection(connection.id)))
                .chain(
                    self.chrome
                        .toolbar
                        .folders()
                        .iter()
                        .map(|folder| (Group::Connections, Item::Folder(folder.path.clone()))),
                )
                .collect(),
            Catalog::Settings => [
                Settings,
                FontSettings,
                ColorSettings,
                ThemeSettings,
                AppearanceSettings,
                KeyboardSettings,
                LoggingSettings,
                WindowSettings,
                ShowMenuBar,
                TerminalSettings,
                Clipboard,
                Vault,
                Isolation,
                ControlApi,
            ]
            .into_iter()
            .map(|action| (Group::Settings, Item::Action(action)))
            .collect(),
            Catalog::Actions => {
                let mut actions = vec![(Group::Terminal, Item::Action(LocalShell))];
                for (action, _, _) in Action::shortcut_catalog() {
                    let group = match action {
                        QuickConnect | LocalShell | PowerShell | CommandPrompt | IncognitoShell
                        | Ssh | IncognitoSsh | Serial | Tn3270 | Tn5250 => Group::Terminal,
                        Settings | FontSettings | WindowSettings | TerminalSettings | Clipboard
                        | Vault | Isolation | ControlApi => continue,
                        ShowMenuBar => Group::Settings,
                        _ => Group::Tools,
                    };
                    let item = (group, Item::Action(action));
                    if !actions.contains(&item) {
                        actions.push(item);
                    }
                }
                actions
            }
        }
    }

    fn toolbar_presentation<'a>(&'a self, item: &'a Item) -> (Icon, Cow<'a, str>) {
        use Action::*;
        match item {
            Item::Action(action) => {
                let icon = match action {
                    QuickConnect => Icon::Tab,
                    LocalShell | IncognitoShell | TerminalSettings => Icon::Shell,
                    PowerShell => Icon::PowerShell,
                    CommandPrompt => Icon::CommandPrompt,
                    ManageConnections => Icon::Connections,
                    Sftp | SftpHere => Icon::Sftp,
                    Snippets => Icon::Snippets,
                    SessionJournals | LoggingSettings => Icon::Journal,
                    Scheduler => Icon::Jobs,
                    Recordings | ToggleRecording => Icon::Video,
                    AiManager => Icon::Ai,
                    LogfileManager => Icon::Logfile,
                    ColorSettings | ThemeSettings => Icon::Colors,
                    AppearanceSettings => Icon::Appearance,
                    KeyboardSettings => Icon::Keyboard,
                    WindowSettings | ShowMenuBar => Icon::Window,
                    FontSettings => Icon::Font,
                    Editor => Icon::Editor,
                    UnlockVault => Icon::Security,
                    _ => Icon::Gear,
                };
                let label = match action {
                    LocalShell => "New local shell",
                    Recordings => "Video Manager…",
                    WindowSettings => "Window",
                    TerminalSettings => "Terminal",
                    ShowMenuBar => "Show/hide menu bar",
                    _ => action.title(),
                };
                (icon, Cow::Borrowed(label))
            }
            Item::Reference(reference) => {
                let (icon, title) = reference.presentation();
                (icon, Cow::Borrowed(title))
            }
            Item::Connection(id) => (
                Icon::Server,
                self.chrome
                    .toolbar
                    .connections()
                    .iter()
                    .find(|connection| connection.id == *id)
                    .map_or_else(
                        || Cow::Owned(format!("Connection #{id} (unavailable)")),
                        |connection| Cow::Borrowed(connection.name.as_str()),
                    ),
            ),
            Item::Folder(path) => (Icon::Group, Cow::Borrowed(path)),
            Item::Separator => (Icon::Separator, Cow::Borrowed("Separator")),
        }
    }

    fn toolbar_item_action(&self, id: window::Id, item: &Item) -> Option<WorkspaceMessage> {
        let toolbar = &self.chrome.toolbar;
        match item {
            Item::Reference(super::toolbar_settings::Reference::Ai)
                if self.chrome_enabled(id, Action::AiManager) =>
            {
                Some(wrap(Message::Invoke(id, Action::AiManager)))
            }
            Item::Action(action) if self.chrome_enabled(id, *action) => {
                Some(wrap(Message::Invoke(id, *action)))
            }
            Item::Connection(connection)
                if self.clipboard.policy.problem.is_none()
                    && toolbar.connections().iter().any(|entry| {
                        entry.id == *connection
                            && (!entry.incognito || self.clipboard.policy.incognito_allowed)
                    }) =>
            {
                Some(message(ToolbarMessage::Connect(id, *connection)))
            }
            Item::Folder(path)
                if self.clipboard.policy.problem.is_none()
                    && toolbar
                        .folders()
                        .iter()
                        .any(|folder| folder.path == *path && folder.populated) =>
            {
                Some(message(ToolbarMessage::ConnectFolder(id, path.clone())))
            }
            _ => None,
        }
    }

    pub(super) fn update_toolbar(&mut self, message: ToolbarMessage) -> Task<WorkspaceMessage> {
        if let Some(owner) = message.owner()
            && !self.windows.contains_key(&owner)
        {
            return Task::none();
        }
        match message {
            ToolbarMessage::Edit(id, Edit::ItemContext(_)) if self.owner_dialog(id).is_some() => {
                self.chrome.toolbar.dismiss_item_menu(id);
                Task::none()
            }
            ToolbarMessage::Connect(id, connection) => self.open_recent_connection(id, connection),
            ToolbarMessage::ConnectFolder(id, path) => self.open_saved_group(id, &path),
            ToolbarMessage::Open(id, catalog) => {
                self.chrome.dismiss(id);
                self.chrome
                    .toolbar
                    .update(ToolbarMessage::Open(id, catalog))
            }
            ToolbarMessage::Saved(result) => {
                let was_pending = self.chrome.toolbar.pending();
                let task = self.chrome.toolbar.update(ToolbarMessage::Saved(result));
                if was_pending
                    && !self.chrome.toolbar.pending()
                    && self.should_exit_after_window_close()
                {
                    return Task::batch([task, self.quit()]);
                }
                task
            }
            message => self.chrome.toolbar.update(message),
        }
    }

    pub(in crate::workspace) fn request_toolbar_quit(&mut self) -> Option<Task<WorkspaceMessage>> {
        let pending = self.chrome.toolbar.pending_owner()?;
        super::super::window_lifecycle::reply_to_native_quit(false);
        let owner = self
            .windows
            .contains_key(&pending)
            .then_some(pending)
            .or_else(|| {
                self.windows
                    .iter()
                    .max_by_key(|(_, window)| (window.focused, window.number))
                    .map(|(id, _)| *id)
            });
        Some(owner.map_or_else(Task::none, |id| {
            self.notice(
                id,
                "The toolbar is being saved. Try quitting again shortly.".into(),
            );
            window::gain_focus(id)
        }))
    }
}

fn toolbar_button(
    label: &'static str,
    message: WorkspaceMessage,
    colors: ChromeColors,
) -> Element<'static, WorkspaceMessage> {
    button(text(label).size(12))
        .style(move |_, status| button_style(status, colors.toolbar_hover, false, colors))
        .on_press(message)
        .into()
}

fn vertical_separator(height: f32, colors: ChromeColors) -> Element<'static, WorkspaceMessage> {
    container(space::vertical())
        .width(1)
        .height(height)
        .style(move |_| container::Style::default().background(colors.toolbar_hover))
        .into()
}

fn drop_end(
    id: window::Id,
    group: Group,
    draft: &Draft,
    colors: ChromeColors,
) -> Element<'static, WorkspaceMessage> {
    let accepting = draft.can_drop(group);
    mouse_area(
        container(text("Drop here").size(11).color(colors.muted))
            .width(61)
            .height(u32::from(draft.layout.icon_size()) + 10)
            .padding([2, 4])
            .align_y(Alignment::Center)
            .style(move |_| {
                if accepting {
                    container::Style::default().background(colors.toolbar_hover)
                } else {
                    container::Style::default()
                }
            }),
    )
    .on_release(edit(id, Edit::Drop(group, None)))
    .into()
}

fn visibility_check(
    id: window::Id,
    label: &'static str,
    row: Row,
    selected: bool,
    enabled: bool,
) -> Element<'static, WorkspaceMessage> {
    let field = checkbox(selected).label(label).text_size(12);
    if enabled {
        field
            .on_toggle(move |visible| message(ToolbarMessage::Visible(id, row, visible)))
            .into()
    } else {
        field.into()
    }
}

fn catalog_source<'a>(
    id: window::Id,
    content: iced::widget::Container<'a, WorkspaceMessage>,
    group: Group,
    item: Item,
    enabled: bool,
    colors: ChromeColors,
) -> Element<'a, WorkspaceMessage> {
    let content = content.style(move |_| {
        container::Style::default()
            .background(colors.toolbar_hover)
            .color(if enabled {
                colors.foreground
            } else {
                colors.muted
            })
            .border(iced::Border::default().rounded(3))
    });
    if enabled {
        mouse_area(content)
            .interaction(mouse::Interaction::Grab)
            .on_press(edit(id, Edit::PickCatalog(group, item.clone())))
            .on_release(edit(id, Edit::ReleaseCatalog(group, item)))
            .into()
    } else {
        content.into()
    }
}

fn open(id: window::Id, catalog: Catalog) -> WorkspaceMessage {
    message(ToolbarMessage::Open(id, catalog))
}

fn edit(id: window::Id, edit: Edit) -> WorkspaceMessage {
    message(ToolbarMessage::Edit(id, edit))
}

fn message(message: ToolbarMessage) -> WorkspaceMessage {
    wrap(Message::Toolbar(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appearance_uses_its_distinct_icon_in_the_settings_catalog() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let entry = (Group::Settings, Item::Action(Action::AppearanceSettings));

        assert!(
            workspace
                .toolbar_catalog(Catalog::Settings)
                .contains(&entry)
        );
        let (_, item) = entry;
        assert!(!Layout::default().items(Group::Settings).contains(&item));
        let (icon, label) = workspace.toolbar_presentation(&item);
        assert!(matches!(icon, Icon::Appearance));
        assert_eq!(label, "Appearance");
        assert!(matches!(
            workspace.toolbar_item_action(owner, &item),
            Some(WorkspaceMessage::Chrome(Message::Invoke(id, Action::AppearanceSettings)))
                if id == owner
        ));
    }

    #[test]
    fn logging_is_available_with_the_reference_journal_icon_and_default_placement() {
        let workspace = Workspace::default();
        let item = Item::Action(Action::LoggingSettings);
        assert_eq!(Layout::default().items(Group::Settings)[3], item);
        assert!(
            workspace
                .toolbar_catalog(Catalog::Settings)
                .contains(&(Group::Settings, item.clone()))
        );
        let (icon, label) = workspace.toolbar_presentation(&item);
        assert!(matches!(icon, Icon::Journal));
        assert_eq!(label, "Logging");
    }
}
