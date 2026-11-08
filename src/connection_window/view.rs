use std::collections::BTreeMap;

use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, space, stack, text, text_input,
    tooltip,
};
use iced::{Alignment, Color, Element, Length, Theme};

use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::{Auth, Connection, Store};
use crate::ssh_dialog::KeyChoice;
use crate::terminal_effects::Effect;

use super::color::ColorDraft;
use super::selection::{RowKey, VisualRow};
use super::{ConnectionWindow, CredentialChoice, FolderDialog, Message, Scope, os_badge};

mod icons;
use icons::Icon;

const SIDEBAR_WIDTH: u32 = 210;
const INSPECTOR_WIDTH: u32 = 370;

pub(crate) struct ViewChoices<'a> {
    pub(crate) passwords: &'a [CredentialChoice],
    pub(crate) passphrases: &'a [CredentialChoice],
    pub(crate) keys: &'a [KeyChoice],
    pub(crate) incognito_allowed: bool,
    pub(crate) terminal_effects: Option<&'a [Effect]>,
    pub(crate) default_paste_warning: PasteWarningMode,
    pub(crate) minimum_paste_warning: PasteWarningMode,
}

impl ConnectionWindow {
    pub(crate) fn view(&self, choices: ViewChoices<'_>) -> Element<'_, Message> {
        let selected = self.connect_selection().is_some();
        let status = self.notice.as_deref().unwrap_or_default();
        let footer = row![
            button("Reload")
                .style(button::text)
                .on_press_maybe(self.can_browse().then_some(Message::Reload)),
            button("Vault…")
                .style(button::text)
                .on_press(Message::OpenVault),
            text(status).size(13).width(Length::Fill),
            button("Close")
                .style(button::secondary)
                .on_press(Message::Close),
            button("Connect").on_press_maybe(selected.then_some(Message::Connect(false))),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding([8, 10]);
        column![
            self.toolbar(),
            rule::horizontal(1),
            row![
                container(self.sidebar())
                    .width(SIDEBAR_WIDTH)
                    .height(Length::Fill)
                    .style(sidebar_background),
                rule::vertical(4),
                container(self.results())
                    .width(Length::Fill)
                    .height(Length::Fill),
                rule::vertical(4),
                scrollable(self.inspector(&choices))
                    .width(INSPECTOR_WIDTH)
                    .height(Length::Fill),
            ]
            .height(Length::Fill),
            rule::horizontal(1),
            footer,
        ]
        .height(Length::Fill)
        .into()
    }

    fn toolbar(&self) -> Element<'_, Message> {
        let browsing = self.can_browse();
        let selected = self.selected().is_some();
        let group = browsing && self.active_group().is_some();
        let search = text_input(
            "Search by name, host, IP address or tag… (* as wildcard)",
            &self.query,
        )
        .on_input_maybe(browsing.then_some(Message::Search))
        .width(320);
        row![
            tool_group(
                "Connection",
                [
                    tool(
                        Icon::AddServer,
                        "New connection",
                        browsing.then_some(Message::New)
                    ),
                    tool(
                        Icon::Edit,
                        "Edit connection",
                        selected.then_some(Message::EditSelected)
                    ),
                    tool(
                        Icon::Duplicate,
                        "Duplicate connection",
                        selected.then_some(Message::Duplicate)
                    ),
                    tool(
                        Icon::Delete,
                        "Delete connection",
                        (!self.selected_connection_ids().is_empty()).then_some(Message::Delete)
                    ),
                ]
            ),
            container(rule::vertical(1)).height(44),
            tool_group(
                "Folder",
                [
                    tool(
                        Icon::FolderAdd,
                        "New folder",
                        browsing.then_some(Message::NewGroup)
                    ),
                    tool(
                        Icon::Rename,
                        "Rename folder",
                        group.then_some(Message::RenameGroup)
                    ),
                    tool(
                        Icon::Colors,
                        "Tab Color…",
                        group.then_some(Message::EditGroupColor)
                    ),
                    tool(
                        Icon::Delete,
                        "Delete folder",
                        group.then_some(Message::DeleteGroup)
                    ),
                ]
            ),
            container(rule::vertical(1)).height(44),
            tool_group(
                "Exchange",
                [
                    tool(
                        Icon::ImportConnections,
                        "Import connections…",
                        self.can_import_connections()
                            .then_some(Message::ImportConnections),
                    ),
                    tool(
                        Icon::ExportConnections,
                        "Export connections…",
                        self.can_export_connections()
                            .then_some(Message::ExportConnections),
                    ),
                    tool(
                        Icon::ExportOverview,
                        "Export overview…",
                        self.can_export_overview()
                            .then_some(Message::ExportOverview),
                    ),
                ],
            ),
            space::horizontal(),
            Icon::Search.widget(17, browsing),
            search,
        ]
        .padding([7, 10])
        .spacing(8)
        .align_y(Alignment::End)
        .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let profiles = self.store.connections();
        let mut items = column![
            heading("Library"),
            self.scope_item(
                Scope::All,
                Icon::ViewList,
                "All connections",
                profiles.len(),
                0
            ),
            self.scope_item(
                Scope::Favorites,
                Icon::Star,
                "Favorites",
                profiles
                    .iter()
                    .filter(|entry| self.store.is_favorite(entry.id))
                    .count(),
                0
            ),
            self.scope_item(
                Scope::Recent,
                Icon::Clock,
                "Recently used",
                self.store.recent().count(),
                0
            ),
        ]
        .spacing(2);
        if self.open_counts.is_some() {
            items = items.push(
                self.scope_item(
                    Scope::Open,
                    Icon::Tab,
                    "Open now",
                    profiles
                        .iter()
                        .filter(|entry| self.open_count(entry.id) > 0)
                        .count(),
                    0,
                ),
            );
        }
        let counts = group_counts(&self.store, profiles.iter());
        let mut folders: Vec<_> = self.store.groups().collect();
        folders.sort_by_cached_key(|path| path.to_lowercase());
        if !folders.is_empty() {
            items = items.push(heading("Folders"));
            for path in folders {
                items = items.push(self.scope_item(
                    Scope::Group(path.to_owned()),
                    Icon::Folder,
                    basename(path),
                    counts.get(path).copied().unwrap_or(0),
                    depth(path) - 1,
                ));
            }
        }
        let mut tags = BTreeMap::new();
        for entry in profiles.iter().filter(|entry| !entry.tag.is_empty()) {
            tags.entry(entry.tag.to_lowercase())
                .or_insert(entry.tag.as_str());
        }
        if !tags.is_empty() {
            let chips = tags
                .into_iter()
                .fold(row![].spacing(5), |chips, (key, label)| {
                    let scope = Scope::Tag(key);
                    chips.push(
                        button(chip(Icon::Tag, label))
                            .style(selectable(self.scope == scope))
                            .padding([3, 6])
                            .on_press_maybe(self.can_browse().then(|| {
                                Message::Scope(if self.scope == scope {
                                    Scope::All
                                } else {
                                    scope
                                })
                            })),
                    )
                });
            items = items.push(heading("Tags")).push(chips.wrap());
        }
        scrollable(items.padding(8))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn scope_item<'a>(
        &self,
        scope: Scope,
        icon: Icon,
        label: &'a str,
        count: usize,
        indent: usize,
    ) -> Element<'a, Message> {
        button(
            row![
                space::horizontal().width((indent * 14) as u32),
                icon.widget(17, true),
                text(label)
                    .size(14)
                    .width(Length::Fill)
                    .wrapping(text::Wrapping::None),
                text(count.to_string()).size(12).style(text::secondary),
            ]
            .spacing(7)
            .align_y(Alignment::Center),
        )
        .style(selectable(self.scope == scope))
        .width(Length::Fill)
        .padding([5, 7])
        .clip(true)
        .on_press_maybe(self.can_browse().then_some(Message::Scope(scope)))
        .into()
    }

    fn results(&self) -> Element<'_, Message> {
        let visible = self.visible_connections();
        let count = visible.len();
        let scope = match &self.scope {
            Scope::All => "All connections",
            Scope::Favorites => "Favorites",
            Scope::Recent => "Recently used",
            Scope::Open => "Open now",
            Scope::Group(path) => basename(path),
            Scope::Tag(tag) => self
                .store
                .connections()
                .iter()
                .find(|entry| entry.tag.to_lowercase() == *tag)
                .map_or(tag.as_str(), |entry| &entry.tag),
        };
        let header = row![
            text(scope).size(19),
            text(format!(
                "{count} {}",
                if count == 1 {
                    "connection"
                } else {
                    "connections"
                }
            ))
            .size(13)
            .style(text::secondary),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding([10, 12]);
        let rows = self.visual_rows(visible);
        let has_rows = !rows.is_empty();
        let mut list = column![].spacing(1);
        for entry in rows {
            list = list.push(match entry {
                VisualRow::Connection(connection, indent) => {
                    self.connection_row(connection, indent)
                }
                VisualRow::Group {
                    path,
                    indent,
                    count,
                    collapsed,
                } => self.group_row(path, indent, count, collapsed),
            });
        }
        if !has_rows {
            list = list.push(
                container(
                    text(if self.query.trim().is_empty() {
                        "No connections in this view."
                    } else {
                        "No matching connections."
                    })
                    .size(14)
                    .style(text::secondary),
                )
                .padding(12),
            );
        }
        column![
            header,
            self.tree_context_view(
                &RowKey::Empty,
                scrollable(list)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .into()
            ),
        ]
        .height(Length::Fill)
        .into()
    }

    fn group_row<'a>(
        &'a self,
        path: &'a str,
        indent: usize,
        count: usize,
        collapsed: bool,
    ) -> Element<'a, Message> {
        let row_key = RowKey::Group(path.to_owned());
        let base = row![
            space::horizontal().width((10 + indent * 20) as u32),
            button(if collapsed { "▸" } else { "▾" })
                .style(button::text)
                .padding(2)
                .on_press_maybe(
                    self.can_browse()
                        .then(|| Message::ToggleGroup(path.to_owned()))
                ),
            button(
                row![
                    Icon::Folder.widget(17, true),
                    text(basename(path)).size(15),
                    text(count.to_string()).size(12).style(text::secondary),
                    self.store.group_color(path).map(|color| tooltip(
                        text("●").color(color.color()),
                        text(format!("Tab color of this folder · {color}")),
                        tooltip::Position::Bottom,
                    )),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            )
            .style(selectable(self.selection.contains(&row_key)))
            .width(Length::Fill)
            .on_press_maybe(
                self.can_browse()
                    .then(|| Message::SelectGroup(path.to_owned()))
            ),
        ]
        .align_y(Alignment::Center)
        .into();
        self.tree_context_view(&row_key, base)
    }

    fn connection_row<'a>(
        &'a self,
        connection: &'a Connection,
        indent: usize,
    ) -> Element<'a, Message> {
        let mut line = row![
            space::horizontal().width((10 + indent * 20) as u32),
            server_tile(self.connection_badge(connection.id), 20, 29, 15),
            column![
                text(&connection.name)
                    .size(15)
                    .wrapping(text::Wrapping::None),
                text(address(connection))
                    .size(13)
                    .style(text::secondary)
                    .wrapping(text::Wrapping::None),
            ]
            .spacing(1),
        ]
        .spacing(9)
        .align_y(Alignment::Center);
        if !connection.tag.is_empty() {
            line = line.push(chip(Icon::Tag, &connection.tag));
        }
        if connection.jump.is_some() {
            line = line.push(Icon::Jump.widget(16, true));
        }
        if connection.tunnels.iter().any(|tunnel| tunnel.enabled) {
            line = line.push(Icon::Tunnel.widget(16, true));
        }
        if self.store.is_favorite(connection.id) {
            line = line.push(Icon::Star.widget(16, true));
        }
        let open = self.open_count(connection.id);
        if open > 0 {
            line = line.push(text(format!("{open} open")).size(12));
        }
        let base = button(line)
            .width(Length::Fill)
            .padding([5, 8])
            .clip(true)
            .style(selectable(
                self.selection.contains(&RowKey::Connection(connection.id)),
            ))
            .on_press_maybe(self.can_browse().then_some(Message::Select(connection.id)))
            .into();
        self.tree_context_view(&RowKey::Connection(connection.id), base)
    }

    fn inspector(&self, choices: &ViewChoices<'_>) -> Element<'_, Message> {
        let contents = if self.confirming_close {
            column![
                text("Discard unsaved changes?").size(21),
                button("Keep editing")
                    .style(button::secondary)
                    .on_press(Message::KeepEditing),
                button("Discard and close")
                    .style(button::danger)
                    .on_press(Message::DiscardAndClose),
            ]
            .spacing(10)
            .into()
        } else if self.busy {
            text("Updating connection profiles…").into()
        } else if !self.confirming_delete.is_empty() {
            column![
                text(format!(
                    "Delete {} selected connection(s)?",
                    self.confirming_delete.len()
                ))
                .size(21),
                row![
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(Message::ConfirmDelete(false)),
                    button("Delete profile")
                        .style(button::danger)
                        .on_press(Message::ConfirmDelete(true)),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into()
        } else if let Some(prompt) = self.tree_context_prompt() {
            prompt
        } else if let Some(dialog) = &self.folder {
            folder_dialog(dialog, &self.store)
        } else if let Some(draft) = &self.draft {
            let options: Vec<_> = std::iter::once(GroupChoice(None))
                .chain(
                    self.store
                        .groups()
                        .map(|path| GroupChoice(Some(path.to_owned()))),
                )
                .collect();
            let selected = GroupChoice(self.draft_group.clone());
            let form_group = row![
                text("Folder").size(14),
                pick_list(options, Some(selected), |choice: GroupChoice| {
                    Message::DraftGroup(choice.0)
                })
                .width(Length::Fill),
            ]
            .spacing(8)
            .align_y(Alignment::Center);
            draft.view(
                choices,
                &self.custom_badges,
                form_group.into(),
                !self.draft_conflict,
            )
        } else if let Some(path) = self.selection.single_group() {
            self.group_inspector(path)
        } else if let Some(connection) = self.single_connection() {
            self.connection_inspector(connection, choices.keys)
        } else {
            column![
                text("Inspector").size(20),
                text("Select a connection or folder to see its details.")
                    .size(14)
                    .style(text::secondary),
            ]
            .spacing(10)
            .into()
        };
        container(contents).padding(16).width(Length::Fill).into()
    }

    fn group_inspector<'a>(&self, path: &'a str) -> Element<'a, Message> {
        column![
            row![
                tile(Icon::Folder, 28, 50),
                column![
                    text(basename(path)).size(22),
                    text(path).size(13).style(text::secondary),
                ]
                .spacing(4)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            row![
                tool(Icon::FolderAdd, "New subfolder", Some(Message::NewGroup)),
                tool(Icon::Rename, "Rename folder", Some(Message::RenameGroup)),
                tool(Icon::Colors, "Tab Color…", Some(Message::EditGroupColor)),
                tool(Icon::Delete, "Delete folder", Some(Message::DeleteGroup)),
            ]
            .spacing(6),
            heading("FOLDER"),
            self.store
                .inherited_group_color(path)
                .map(|(source, color)| {
                    fact(
                        "Tab color",
                        if source == path {
                            color.to_string()
                        } else {
                            format!("{color} · From {source}")
                        },
                    )
                }),
            fact("Connections", self.store.in_group(path).count().to_string()),
            fact(
                "Subfolders",
                self.store
                    .groups()
                    .filter(|group| group
                        .rsplit_once('/')
                        .is_some_and(|(parent, _)| parent == path))
                    .count()
                    .to_string()
            ),
        ]
        .spacing(12)
        .into()
    }

    fn connection_inspector<'a>(
        &'a self,
        entry: &'a Connection,
        keys: &[crate::ssh_dialog::KeyChoice],
    ) -> Element<'a, Message> {
        let browsing = self.can_browse();
        let id = entry.id;
        let badge = self.connection_badge(id);
        let mut chips = row![].spacing(6);
        if let Some(group) = self.store.group_of(entry.id) {
            chips = chips.push(chip(Icon::Folder, basename(group)));
        }
        if !entry.tag.is_empty() {
            chips = chips.push(chip(Icon::Tag, &entry.tag));
        }
        let sign_in = match &entry.login.auth {
            Auth::Prompt => "Password, asked when connecting",
            Auth::KeyboardInteractive => "Keyboard-interactive",
            Auth::VaultPassword(_) => "Vault password",
            Auth::KeyFile { .. } => "SSH private key",
            Auth::StoredKey(id) => keys
                .iter()
                .find(|key| key.id == *id)
                .map_or("Registered key unavailable", |key| &key.name),
            Auth::Configured {
                method,
                credential,
                key,
            } => match method {
                crate::connection_store::AuthMethod::Password if credential.is_some() => {
                    "Vault password"
                }
                crate::connection_store::AuthMethod::Password => "Password, asked when connecting",
                crate::connection_store::AuthMethod::KeyboardInteractive => "Keyboard-interactive",
                crate::connection_store::AuthMethod::PublicKey => key
                    .and_then(|id| keys.iter().find(|key| key.id == id))
                    .map_or("Choose a registered key when connecting", |key| &key.name),
            },
        }
        .to_owned();
        let favorite = self.store.is_favorite(entry.id);
        let mut route = row![text("This computer").size(13), text("→").size(13)].spacing(7);
        if let Some(jump) = &entry.jump {
            route = route
                .push(Icon::Jump.widget(16, true))
                .push(text(jump.endpoint.label()).size(13))
                .push(text("→").size(13));
        }
        route = route.push(text(&entry.name).size(13));
        let mut body = column![
            row![
                server_tile(badge, 31, 55, 24),
                column![text(&entry.name).size(23), chips.wrap()].spacing(5),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
            row![
                button(
                    row![Icon::Connect.widget(15, browsing), text("Connect").size(14)]
                        .spacing(6)
                        .align_y(Alignment::Center)
                )
                .on_press_maybe(browsing.then_some(Message::Connect(false))),
                tool(
                    Icon::Sftp,
                    "Open SFTP",
                    browsing.then_some(Message::Connect(true))
                ),
                tool(
                    Icon::Edit,
                    "Edit connection",
                    browsing.then_some(Message::EditSelected)
                ),
                tool(
                    Icon::Duplicate,
                    "Duplicate connection",
                    browsing.then_some(Message::Duplicate)
                ),
                tool(
                    Icon::Star,
                    if favorite {
                        "Remove favorite"
                    } else {
                        "Add favorite"
                    },
                    browsing.then_some(Message::ToggleFavorite)
                ),
            ]
            .spacing(5)
            .wrap(),
            heading("CONNECTION"),
            column![
                fact("Host", &entry.login.endpoint.host),
                fact("User", &entry.login.endpoint.username),
                fact("Port", entry.login.endpoint.port.to_string()),
                fact("Protocol", "SSH over TCP/IP"),
                fact("Sign-in", sign_in),
                fact("Terminal", entry.emulation.as_str()),
            ]
            .spacing(4),
            os_badge::picker(
                badge,
                &self.custom_badges,
                self.badge_picker == Some(id),
                browsing.then_some(Message::ToggleOsBadgePicker(id)),
                browsing.then_some(Message::ManageOsBadges),
                move |badge| Message::SetOsBadge(id, badge),
            ),
            heading("ROUTE"),
            route.wrap(),
            heading("SECURITY"),
            fact("Isolation", entry.isolation.to_string()),
            fact("Host key", "Verify before authentication"),
        ]
        .spacing(10);
        if !entry.tunnels.is_empty() {
            body = body.push(heading("SSH TUNNELS"));
            for tunnel in &entry.tunnels {
                body = body.push(fact(
                    if tunnel.enabled {
                        "Enabled"
                    } else {
                        "Disabled"
                    },
                    format!("{} {}", tunnel.rule.label(), tunnel.description),
                ));
            }
        }
        let open = self.open_count(entry.id);
        let recent = self
            .store
            .recent()
            .any(|connection| connection.id == entry.id);
        if self.open_counts.is_some() || recent {
            body = body.push(heading("USAGE"));
            if self.open_counts.is_some() {
                body = body.push(fact("Open tabs", open.to_string()));
            }
            if recent {
                body = body.push(text("Recently used").size(13));
            }
        }
        let name = self
            .quick
            .as_ref()
            .map_or(entry.name.as_str(), |quick| &quick.name);
        let tag = self
            .quick
            .as_ref()
            .map_or(entry.tag.as_str(), |quick| &quick.tag);
        let mut name_field = text_input("Connection name", name).on_input(Message::QuickName);
        let mut tag_field = text_input("Optional tag", tag).on_input(Message::QuickTag);
        if !self.draft_conflict {
            name_field = name_field.on_submit(Message::SaveQuick);
            tag_field = tag_field.on_submit(Message::SaveQuick);
        }
        body = body
            .push(heading("QUICK EDIT (ENTER APPLIES)"))
            .push(row![text("Name").size(13).width(72), name_field].align_y(Alignment::Center))
            .push(row![text("Tag").size(13).width(72), tag_field].align_y(Alignment::Center));
        if self.quick.is_some() {
            body = body.push(
                button("Discard quick edits")
                    .style(button::secondary)
                    .on_press(Message::CancelQuick),
            );
        }
        body.into()
    }
}

fn group_counts<'a>(
    store: &'a Store,
    entries: impl Iterator<Item = &'a Connection>,
) -> BTreeMap<&'a str, usize> {
    let mut counts = BTreeMap::new();
    for entry in entries {
        let mut parent = store.group_of(entry.id);
        while let Some(path) = parent {
            *counts.entry(path).or_default() += 1;
            parent = path.rsplit_once('/').map(|(parent, _)| parent);
        }
    }
    counts
}

#[derive(Clone, PartialEq, Eq)]
struct GroupChoice(Option<String>);

impl std::fmt::Display for GroupChoice {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.write_str(self.0.as_deref().unwrap_or("No folder"))
    }
}

fn folder_dialog<'a>(dialog: &'a FolderDialog, store: &'a Store) -> Element<'a, Message> {
    let (title, hint, name, action) = match dialog {
        FolderDialog::Create { parent, name } => (
            "New folder",
            parent.as_ref().map(|path| format!("Inside {path}")),
            Some(name.as_str()),
            "Create",
        ),
        FolderDialog::Rename { path, name } => (
            "Rename folder",
            Some(path.clone()),
            Some(name.as_str()),
            "Rename",
        ),
        FolderDialog::Delete { path, count } => (
            "Delete folder?",
            Some(format!(
                "Delete {path}, its subfolders, and {count} saved connection(s)?"
            )),
            None,
            "Delete folder",
        ),
        FolderDialog::Color { path, draft, .. } => return folder_color_dialog(path, draft, store),
    };
    let mut contents = column![text(title).size(22)].spacing(10);
    if let Some(hint) = hint {
        contents = contents.push(text(hint).size(14));
    }
    if let Some(name) = name {
        contents = contents.push(
            text_input("Folder name", name)
                .on_input(Message::GroupName)
                .on_submit(Message::CommitGroup),
        );
    }
    contents
        .push(
            row![
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Message::CancelGroup),
                button(action)
                    .style(if name.is_some() {
                        button::primary
                    } else {
                        button::danger
                    })
                    .on_press_maybe(
                        name.is_none_or(|name| !name.trim().is_empty())
                            .then_some(Message::CommitGroup)
                    ),
            ]
            .spacing(8),
        )
        .into()
}

fn folder_color_dialog<'a>(
    path: &str,
    draft: &'a ColorDraft,
    store: &Store,
) -> Element<'a, Message> {
    let inherited = store.ancestor_group_color(path);
    column![
        text("Folder Tab Color").size(22),
        text(format!("Tab color of the folder \"{path}\"")),
        draft.view(
            "Mark the tabs of this folder's connections with a color",
            Message::GroupColorEnabled,
            Message::GroupColorInput,
        ),
        inherited.map(|(source, color)| text(format!(
            "Without a color of its own, this folder inherits {color} from \"{source}\"."
        )).size(13)),
        text("Applies to connections in this folder and its subfolders. A connection's own color takes precedence, followed by the nearest folder color and the credential environment.").size(13),
        row![
            button("Cancel").style(button::secondary).on_press(Message::CancelGroup),
            button("OK").on_press_maybe(draft.value().is_ok().then_some(Message::CommitGroup)),
        ].spacing(8),
    ].spacing(10).into()
}

fn address(connection: &Connection) -> String {
    let entry = &connection.login.endpoint;
    if entry.port == 22 {
        format!("{}@{}", entry.username, entry.host)
    } else {
        entry.label()
    }
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn depth(path: &str) -> usize {
    path.split('/').count()
}

fn tool_group<'a, const N: usize>(
    label: &'a str,
    tools: [Element<'a, Message>; N],
) -> Element<'a, Message> {
    column![
        text(label).size(12).style(text::secondary),
        row(tools).spacing(2)
    ]
    .spacing(3)
    .into()
}

fn tool<'a>(icon: Icon, label: &'a str, action: Option<Message>) -> Element<'a, Message> {
    tooltip(
        button(icon.widget(20, action.is_some()))
            .style(button::text)
            .padding([5, 8])
            .on_press_maybe(action),
        container(text(label).size(13))
            .padding(6)
            .style(container::bordered_box),
        tooltip::Position::Bottom,
    )
    .into()
}

fn heading<'a>(label: &'static str) -> Element<'a, Message> {
    container(text(label).size(12).style(text::secondary))
        .padding(iced::Padding {
            top: 8.0,
            bottom: 2.0,
            ..iced::Padding::ZERO
        })
        .into()
}

fn fact<'a>(label: &'static str, value: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    row![
        text(label).size(13).style(text::secondary).width(78),
        text(value).size(14).width(Length::Fill),
    ]
    .spacing(6)
    .into()
}

fn chip<'a>(icon: Icon, label: &'a str) -> Element<'a, Message> {
    container(
        row![icon.widget(12, true), text(label).size(12)]
            .spacing(4)
            .align_y(Alignment::Center),
    )
    .padding([2, 6])
    .style(container::bordered_box)
    .into()
}

fn tile<'a>(icon: Icon, size: u32, side: u32) -> Element<'a, Message> {
    container(icon.widget(size, true))
        .center(side)
        .style(container::bordered_box)
        .into()
}

fn server_tile<'a>(
    badge: Option<os_badge::Badge<'a>>,
    size: u32,
    side: u32,
    badge_size: u32,
) -> Element<'a, Message> {
    let tile = tile(Icon::Server, size, side);
    if let Some(badge) = badge {
        stack![
            tile,
            container(badge.image(badge_size))
                .width(side)
                .align_bottom(side),
        ]
        .into()
    } else {
        tile
    }
}

fn sidebar_background(theme: &Theme) -> container::Style {
    container::background(theme.extended_palette().background.weakest.color)
}

fn selectable(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let base = button::text(theme, status);
        if active {
            button::Style {
                background: Some(Color::from_rgba8(54, 138, 245, 0.24).into()),
                ..base
            }
        } else {
            base
        }
    }
}
