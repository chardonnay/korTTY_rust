use std::collections::BTreeMap;
use std::fmt;

use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, space, stack, text, text_input,
    tooltip,
};
use iced::{Alignment, Color, Element, Length, Theme};

use crate::ai_manager::ProfileRef;
use crate::clipboard_policy::PasteWarningMode;
use crate::connection_store::teamwork;
use crate::connection_store::{Auth, Connection, LocalProfile, SerialProfile, Store};
use crate::ssh_dialog::KeyChoice;
use crate::terminal_agent::design::Choice as AgentDesignChoice;
use crate::terminal_effects::Effect;
use crate::terminal_theme::Themes;

use super::color::ColorDraft;
use super::selection::{RowKey, VisualRow};
use super::{
    ConnectionWindow, CredentialChoice, FolderDialog, Message, Scope, TeamworkAuthDraft, os_badge,
};

pub(super) mod icons;
use icons::Icon;

const SIDEBAR_WIDTH: u32 = 210;
const INSPECTOR_WIDTH: u32 = 370;

pub(crate) struct ViewChoices<'a> {
    pub(crate) passwords: &'a [CredentialChoice],
    pub(crate) passphrases: &'a [CredentialChoice],
    pub(crate) keys: &'a [KeyChoice],
    pub(crate) ai_profiles: &'a [(Option<u64>, ProfileRef, String)],
    pub(crate) ai_skills: &'a [(u64, String)],
    pub(crate) ai_agent_designs: &'a [AgentDesignChoice],
    pub(crate) highlighting: HighlightChoices<'a>,
    pub(crate) themes: &'a Themes,
    pub(crate) journal_max_parts: Option<u16>,
    pub(crate) incognito_allowed: bool,
    pub(crate) serial_allowed: bool,
    pub(crate) terminal_effects: Option<&'a [Effect]>,
    pub(crate) default_paste_warning: PasteWarningMode,
    pub(crate) minimum_paste_warning: PasteWarningMode,
}

#[derive(Default)]
pub(crate) struct HighlightChoices<'a> {
    pub(crate) sets: &'a [(String, String)],
    pub(crate) default_set: Option<&'a str>,
    pub(crate) enabled: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct TeamworkAuthChoice {
    value: teamwork::DefaultAuth,
    label: String,
}

impl fmt::Display for TeamworkAuthChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn teamwork_auth_choices(
    draft: &teamwork::DefaultAuth,
    choices: &ViewChoices<'_>,
) -> (Vec<TeamworkAuthChoice>, TeamworkAuthChoice, bool) {
    use teamwork::DefaultAuth;

    let username = match draft {
        DefaultAuth::Key { username, .. } | DefaultAuth::TemporaryKey { username } => username,
        _ => "",
    };
    let mut options = vec![TeamworkAuthChoice {
        value: DefaultAuth::Prompt,
        label: "Ask when connecting".into(),
    }];
    options.extend(choices.passwords.iter().map(|choice| TeamworkAuthChoice {
        value: DefaultAuth::Password {
            credential: choice.id.clone(),
        },
        label: format!("Vault password · {}", choice.label),
    }));
    options.extend(choices.keys.iter().map(|choice| TeamworkAuthChoice {
        value: DefaultAuth::Key {
            id: choice.id,
            username: username.to_owned(),
        },
        label: format!("SSH key · {}", choice.name),
    }));
    options.push(TeamworkAuthChoice {
        value: DefaultAuth::TemporaryKey {
            username: username.to_owned(),
        },
        label: "Temporary SSH key".into(),
    });

    if let Some(selected) = options
        .iter()
        .find(|option| option.value == *draft)
        .cloned()
    {
        return (options, selected, true);
    }
    let label = if matches!(draft, DefaultAuth::Password { .. }) {
        "Saved vault password unavailable"
    } else {
        "Saved SSH key unavailable"
    };
    let selected = TeamworkAuthChoice {
        value: draft.clone(),
        label: label.into(),
    };
    (options, selected, false)
}

fn discard_confirmation() -> Element<'static, Message> {
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
}

impl ConnectionWindow {
    pub(crate) fn editor_title(&self) -> Option<&'static str> {
        if let Some(draft) = &self.local_draft {
            Some(if draft.id == 0 {
                "New local shell profile"
            } else {
                "Edit local shell profile"
            })
        } else if let Some(draft) = &self.serial_draft {
            Some(if draft.id == 0 {
                "New serial connection"
            } else {
                "Edit serial connection"
            })
        } else {
            self.draft.as_ref().map(|draft| {
                if draft.id() == 0 {
                    "New connection"
                } else {
                    "Edit connection"
                }
            })
        }
    }

    pub(crate) fn editor_view(&self, choices: ViewChoices<'_>) -> Option<Element<'_, Message>> {
        let title = self.editor_title()?;
        let contents = if self.confirming_close {
            discard_confirmation()
        } else if self.busy {
            text("Updating connection profiles…").into()
        } else if let Some(draft) = &self.local_draft {
            draft.view(title, &choices).map(Message::Local)
        } else if let Some(draft) = &self.serial_draft {
            column![
                text(title).size(21),
                text_input("Name (defaults to port and speed)", &draft.name)
                    .on_input(Message::SerialName),
                text_input("Tag", &draft.tag).on_input(Message::SerialTag),
                self.editor_group_picker(),
                draft.form.editor_fields().map(Message::Serial),
                row![
                    text("Paste line delay (ms)"),
                    text_input("Inherit", &draft.paste_line_delay)
                        .on_input(Message::SerialPasteLineDelay),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                row![
                    text("Strict terminal mode"),
                    super::terminal_overrides::strict_terminal_mode(
                        draft.strict_terminal_mode,
                        Message::SerialStrictTerminalMode
                    ),
                ]
                .spacing(8)
                .align_y(iced::Alignment::Center),
                draft
                    .journal
                    .view(choices.journal_max_parts)
                    .map(Message::SerialJournal),
            ]
            .spacing(10)
            .into()
        } else {
            let draft = self.draft.as_ref()?;
            draft.view(&choices, &self.custom_badges, self.editor_group_picker())
        };
        let form =
            scrollable(container(contents).padding(16).width(Length::Fill)).height(Length::Fill);
        let mut body = column![form].height(Length::Fill);
        if !self.confirming_close && !self.busy {
            let valid = if let Some(draft) = &self.local_draft {
                draft.profile().is_ok()
            } else if let Some(draft) = &self.serial_draft {
                draft.profile().is_ok()
            } else {
                self.draft
                    .as_ref()
                    .is_some_and(|draft| draft.connection().is_ok())
            };
            body = body.push(rule::horizontal(1)).push(
                row![
                    space::horizontal(),
                    button(if self.draft.as_ref().is_some_and(|draft| draft.changed) {
                        "Discard draft"
                    } else {
                        "Cancel"
                    })
                    .style(button::secondary)
                    .on_press(Message::CancelEdit),
                    button("Save")
                        .on_press_maybe((!self.draft_conflict && valid).then_some(Message::Save)),
                ]
                .spacing(8)
                .padding([8, 16]),
            );
        }
        if let Some(notice) = &self.notice {
            body = body
                .push(rule::horizontal(1))
                .push(container(text(notice).size(13)).padding([8, 16]));
        }
        Some(body.into())
    }

    pub(crate) fn view(&self, choices: ViewChoices<'_>) -> Element<'_, Message> {
        let selected = self.connect_selection().is_some()
            || self.selected_serial().is_some()
            || self.selected_local().is_some()
            || self.selected_shared().is_some();
        let status = [self.notice.as_deref(), self.teamwork_notice.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        let teamwork_auth = self
            .teamwork_auth
            .as_ref()
            .filter(|_| self.scope == Scope::Teamwork)
            .map(|draft| self.teamwork_auth_bar(draft, &choices));
        let footer = row![
            button("Reload")
                .style(button::text)
                .on_press_maybe(self.can_browse().then_some(Message::Reload)),
            button("Vault…")
                .style(button::text)
                .on_press(Message::OpenVault),
            text(status)
                .size(13)
                .width(Length::Fill)
                .wrapping(text::Wrapping::WordOrGlyph),
            button("Close")
                .style(button::secondary)
                .on_press(Message::Close),
            button("Connect").on_press_maybe(selected.then_some(Message::Connect(false))),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding([8, 10]);
        column![
            self.toolbar(choices.serial_allowed),
            rule::horizontal(1),
            teamwork_auth,
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

    fn teamwork_auth_bar<'a>(
        &'a self,
        auth: &'a TeamworkAuthDraft,
        choices: &ViewChoices<'_>,
    ) -> Element<'a, Message> {
        use teamwork::DefaultAuth;

        let editable = self.can_edit_teamwork_auth();
        let (options, selected, available) = teamwork_auth_choices(&auth.draft, choices);
        let picker: Element<'_, Message> = if editable {
            pick_list(options, Some(selected), |choice: TeamworkAuthChoice| {
                Message::TeamworkAuthEdit(choice.value)
            })
            .width(242)
            .into()
        } else {
            text(selected.label).size(14).into()
        };
        let username = match &auth.draft {
            DefaultAuth::Key { id, username } => {
                let id = *id;
                Some(
                    text_input("SSH username (optional)", username).on_input_maybe(
                        editable.then_some(move |username| {
                            Message::TeamworkAuthEdit(DefaultAuth::Key { id, username })
                        }),
                    ),
                )
            }
            DefaultAuth::TemporaryKey { username } => Some(
                text_input("SSH username (optional)", username).on_input_maybe(editable.then_some(
                    |username| Message::TeamworkAuthEdit(DefaultAuth::TemporaryKey { username }),
                )),
            ),
            _ => None,
        };
        let username = username.map(|input| input.width(180));
        let mut body = column![
            row![
                text("Default Teamwork authentication").size(14),
                picker,
                username,
                button("Apply").on_press_maybe(
                    (available && self.teamwork_auth_change().is_some())
                        .then_some(Message::SaveTeamworkAuth)
                ),
                button("Discard changes")
                    .style(button::text)
                    .on_press_maybe(
                        (editable && auth.dirty()).then_some(Message::ResetTeamworkAuth)
                    ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap()
        ]
        .spacing(4);
        let issue = if self.teamwork_auth_saving {
            Some("Saving Teamwork authentication…".into())
        } else if auth.conflict {
            Some("The saved default changed in another window. Discard this draft to use the current setting.".into())
        } else if !available {
            Some("Choose an available credential or SSH key before applying changes.".into())
        } else {
            auth.draft.validate().err().map(|error| error.to_string())
        };
        if let Some(issue) = issue {
            body = body.push(
                text(issue)
                    .size(13)
                    .style(text::secondary)
                    .wrapping(text::Wrapping::WordOrGlyph),
            );
        }
        container(body)
            .padding([7, 10])
            .width(Length::Fill)
            .style(sidebar_background)
            .into()
    }

    fn toolbar(&self, serial_allowed: bool) -> Element<'_, Message> {
        let browsing = self.can_browse();
        let local = browsing && self.scope != Scope::Teamwork;
        let direct_selected = self.selected_serial().is_some() || self.selected_local().is_some();
        let selected = self.selected().is_some() || direct_selected;
        let group = browsing && self.active_group().is_some();
        let delete_local = direct_selected || !self.selected_profile_ids().is_empty();
        let hide_shared = browsing && self.selected_shared_entries().next().is_some();
        let delete_label = match (hide_shared, delete_local) {
            (true, true) => "Hide shared and delete local connections",
            (true, false) => "Hide Teamwork connection",
            (false, _) => "Delete connection",
        };
        let search = text_input("Search profiles… (* as wildcard)", &self.query)
            .on_input_maybe(browsing.then_some(Message::Search))
            .width(Length::Fill);
        row![
            tool_group(
                "Connection",
                [
                    tool(
                        Icon::AddServer,
                        "New connection",
                        local.then_some(Message::New)
                    ),
                    tool(
                        Icon::Serial,
                        "New serial connection",
                        (local && serial_allowed).then_some(Message::NewSerial)
                    ),
                    tool(
                        Icon::Tab,
                        "New local shell profile",
                        local.then_some(Message::NewLocal)
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
                        delete_label,
                        (hide_shared || delete_local).then_some(Message::Delete)
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
                        local.then_some(Message::NewGroup)
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
            container(rule::vertical(1)).height(44),
            tool_group(
                "Teamwork",
                [
                    tool(
                        Icon::Sync,
                        "Refresh Teamwork connections",
                        self.can_restore_teamwork()
                            .then_some(Message::RefreshTeamwork),
                    ),
                    tool(
                        Icon::Restore,
                        "Restore hidden Teamwork connections…",
                        self.can_restore_teamwork()
                            .then_some(Message::RestoreTeamwork),
                    ),
                ],
            ),
            space::horizontal(),
            Icon::Search.widget(17, browsing),
            container(search)
                .width(Length::FillPortion(4))
                .max_width(320),
        ]
        .padding([7, 10])
        .spacing(8)
        .align_y(Alignment::End)
        .into()
    }

    fn sidebar(&self) -> Element<'_, Message> {
        let profiles = self.store.connections();
        let shared_count = self.shared_entries().count();
        let mut items = column![
            heading("Library"),
            self.scope_item(
                Scope::All,
                Icon::ViewList,
                "All connections",
                profiles.len()
                    + self.store.serial_profiles().len()
                    + self.store.local_profiles().len()
                    + shared_count,
                0
            ),
            self.scope_item(
                Scope::Favorites,
                Icon::Star,
                "Favorites",
                self.favorite_count(),
                0
            ),
            self.scope_item(
                Scope::Recent,
                Icon::Clock,
                "Recently used",
                self.recent_entries().len(),
                0
            ),
            self.scope_item(
                Scope::Teamwork,
                Icon::Teamwork,
                "Teamwork connections",
                shared_count,
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
                        .count()
                        + self
                            .store
                            .serial_profiles()
                            .iter()
                            .filter(|entry| self.open_count(entry.id) > 0)
                            .count()
                        + self
                            .store
                            .local_profiles()
                            .iter()
                            .filter(|entry| self.open_count(entry.id) > 0)
                            .count(),
                    0,
                ),
            );
        }
        let counts = group_counts(
            &self.store,
            profiles
                .iter()
                .map(|entry| entry.id)
                .chain(self.store.serial_profiles().iter().map(|entry| entry.id)),
        );
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
        for tag in self.library_tags() {
            tags.entry(tag.to_lowercase()).or_insert(tag);
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
        let entries = self.visible_entries();
        let count = entries.len();
        let scope = match &self.scope {
            Scope::All => "All connections",
            Scope::Favorites => "Favorites",
            Scope::Recent => "Recently used",
            Scope::Open => "Open now",
            Scope::Teamwork => "Teamwork connections",
            Scope::Group(path) => basename(path),
            Scope::Tag(tag) => self
                .library_tags()
                .find(|entry| entry.to_lowercase() == *tag)
                .unwrap_or(tag.as_str()),
        };
        let summary = row![
            container(text(scope).size(19).wrapping(text::Wrapping::None)).clip(true),
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
        .wrap()
        .vertical_spacing(2);
        let header = row![
            container(summary).width(Length::Fill),
            row![
                button("List")
                    .style(selectable(!self.network_map.visible))
                    .on_press_maybe(self.can_browse().then_some(Message::MapView(false))),
                button("Map")
                    .style(selectable(self.network_map.visible))
                    .on_press_maybe(self.can_browse().then_some(Message::MapView(true))),
            ]
            .spacing(3),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .padding([10, 12]);
        if self.network_map.visible {
            return column![header, self.network_map_view(count)]
                .height(Length::Fill)
                .into();
        }
        let rows = self.visible_rows(entries);
        let has_rows = !rows.is_empty();
        let mut list = column![].spacing(1);
        let mut shared_group = None;
        for entry in rows {
            let row = match entry {
                VisualRow::Connection(connection, indent) => {
                    self.connection_row(connection, indent)
                }
                VisualRow::Serial(profile, indent) => self.serial_row(profile, indent),
                VisualRow::Local(profile) => self.local_row(profile),
                VisualRow::Shared(entry) => {
                    if self.scope != Scope::Recent && shared_group != Some(&entry.group) {
                        list = list.push(heading(teamwork::group_title(&entry.group)));
                        shared_group = Some(&entry.group);
                    }
                    self.shared_row(entry)
                }
                VisualRow::Group {
                    path,
                    indent,
                    count,
                    collapsed,
                } => self.group_row(path, indent, count, collapsed),
            };
            list = list.push(row);
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
        let list = scrollable(list)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        let list = if self.scope == Scope::Teamwork {
            list
        } else {
            self.tree_context_view(&RowKey::Empty, list)
        };
        column![header, list].height(Length::Fill).into()
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
        let blocked = connection.blocked_server(&self.server_access);
        let name = text(&connection.name)
            .size(15)
            .wrapping(text::Wrapping::None);
        let name = if blocked.is_some() {
            name.style(text::secondary)
        } else {
            name
        };
        let mut line = row![
            space::horizontal().width((10 + indent * 20) as u32),
            server_tile(self.connection_badge(connection.id), 20, 29, 15),
            column![
                name,
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
        if let Some((host, port)) = blocked {
            line = line.push(blocked_server_badge(host, port));
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
        let contents = if self.editor_title().is_some() {
            text("Connection editor is open.")
                .style(text::secondary)
                .into()
        } else if self.confirming_close {
            discard_confirmation()
        } else if self.busy {
            text("Updating connection profiles…").into()
        } else if !self.confirming_teamwork_hide.is_empty() {
            let count = self.confirming_teamwork_hide.len();
            let title = if count == 1 {
                "Hide this Teamwork connection?".into()
            } else {
                format!("Hide {count} Teamwork connections?")
            };
            column![
                text(title).size(21),
                text("You can restore hidden connections from the Teamwork toolbar.").size(14),
                row![
                    button("Cancel")
                        .style(button::secondary)
                        .on_press(Message::ConfirmTeamworkHide(false)),
                    button("Hide locally").on_press(Message::ConfirmTeamworkHide(true)),
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into()
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
        } else if let Some(path) = self.selection.single_group() {
            self.group_inspector(path)
        } else if let Some(connection) = self.single_connection() {
            self.connection_inspector(connection, choices.keys)
        } else if let Some(entry) = self.selected_shared() {
            let endpoint = &entry.connection.login.endpoint;
            let blocked = entry.connection.blocked_server(&self.server_access);
            column![
                text(&entry.connection.name).size(21),
                row![Icon::Teamwork.widget(17, true), text("Shared (Teamwork)")].spacing(6),
                text(format!(
                    "{}@{}:{}",
                    endpoint.username, endpoint.host, endpoint.port
                )),
                text(format!("Source  {}", entry.source())).size(13),
                blocked.map(|(host, port)| text(server_policy_note(host, port)).size(13)),
                entry.last_used_at.map(|millis| {
                    let when = jiff::Timestamp::from_millisecond(millis as i64).map_or_else(
                        |_| format!("{millis} ms since Unix epoch"),
                        |time| {
                            time.to_zoned(jiff::tz::TimeZone::system())
                                .strftime("%Y-%m-%d %H:%M")
                                .to_string()
                        },
                    );
                    fact("Source last used", when)
                }),
                row![
                    button("Connect")
                        .on_press_maybe(blocked.is_none().then_some(Message::Connect(false))),
                    favorite_tool(self.store.is_teamwork_favorite(entry.identity()), true),
                    tool(
                        Icon::Delete,
                        "Hide Teamwork connection",
                        Some(Message::Delete),
                    ),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            ]
            .spacing(10)
            .into()
        } else if let Some(profile) = self.single_local() {
            let program = profile
                .command
                .as_ref()
                .and_then(|argv| argv.first().map(String::as_str))
                .or_else(|| profile.shell.preset_title())
                .unwrap_or("System default");
            column![
                row![tile(Icon::Tab, 28, 50), text(&profile.name).size(21)]
                    .spacing(10)
                    .align_y(Alignment::Center),
                fact("Program", program.to_owned()),
                profile
                    .working_directory
                    .as_ref()
                    .map(|directory| fact("Starting directory", directory.display().to_string())),
                fact(
                    "Emulation",
                    profile.profile().unwrap_or_default().to_string()
                ),
            ]
            .spacing(10)
            .into()
        } else if let Some(profile) = self.single_serial() {
            column![
                row![
                    tile(Icon::Serial, 28, 50),
                    text(profile.display_name()).size(21)
                ]
                .spacing(10)
                .align_y(Alignment::Center),
                text(format!(
                    "Serial port  {}",
                    profile.connection.device.display()
                ))
                .size(14),
                text(format!("Line  {}", profile.line())).size(14),
                self.store
                    .group_of(profile.id)
                    .map(|group| fact("Folder", group.to_owned())),
                (!profile.tag.is_empty()).then(|| fact("Tag", profile.tag.clone())),
                text(format!(
                    "Emulation  {}",
                    profile.profile().unwrap_or_default()
                ))
                .size(14),
                row![
                    favorite_tool(self.store.is_favorite(profile.id), self.can_browse()),
                    button(if profile.pinned { "Unpin" } else { "Pin" })
                        .on_press_maybe(self.can_browse().then_some(Message::ToggleSerialPin)),
                ]
                .spacing(6),
            ]
            .spacing(10)
            .into()
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

    fn shared_row<'a>(&'a self, entry: &'a teamwork::Entry) -> Element<'a, Message> {
        let endpoint = &entry.connection.login.endpoint;
        let key = RowKey::Shared(entry.identity.clone());
        let blocked = entry.connection.blocked_server(&self.server_access);
        let name = text(&entry.connection.name)
            .size(15)
            .wrapping(text::Wrapping::None);
        let name = if blocked.is_some() {
            name.style(text::secondary)
        } else {
            name
        };
        let base = button(
            row![
                tile(Icon::Teamwork, 20, 29),
                column![
                    name,
                    text(format!("{}@{}", endpoint.username, endpoint.host))
                        .size(13)
                        .style(text::secondary)
                        .wrapping(text::Wrapping::None),
                ]
                .spacing(1),
                self.store
                    .is_teamwork_favorite(entry.identity())
                    .then(|| Icon::Star.widget(16, true)),
                blocked.map(|(host, port)| blocked_server_badge(host, port)),
            ]
            .align_y(Alignment::Center)
            .spacing(9),
        )
        .width(Length::Fill)
        .padding([5, 18])
        .clip(true)
        .style(selectable(self.selection.contains(&key)))
        .on_press_maybe(
            self.can_browse()
                .then(|| Message::SelectShared(entry.identity.clone())),
        )
        .into();
        self.tree_context_view(&key, base)
    }

    fn editor_group_picker(&self) -> Element<'_, Message> {
        let options: Vec<_> = std::iter::once(GroupChoice(None))
            .chain(
                self.store
                    .groups()
                    .map(|path| GroupChoice(Some(path.to_owned()))),
            )
            .collect();
        let selected = GroupChoice(self.draft_group.clone());
        row![
            text("Folder").size(14),
            pick_list(options, Some(selected), |choice: GroupChoice| {
                Message::DraftGroup(choice.0)
            })
            .width(Length::Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }

    fn serial_row<'a>(&'a self, profile: &'a SerialProfile, indent: usize) -> Element<'a, Message> {
        let key = RowKey::Serial(profile.id);
        let mut line = row![
            space::horizontal().width((10 + indent * 20) as u32),
            tile(Icon::Serial, 20, 29),
            column![
                text(profile.display_name())
                    .size(15)
                    .wrapping(text::Wrapping::None),
                text(format!(
                    "{} · {}",
                    profile.connection.device.display(),
                    profile.line()
                ))
                .size(13)
                .style(text::secondary)
                .wrapping(text::Wrapping::None),
            ]
            .spacing(1),
        ]
        .spacing(9)
        .align_y(Alignment::Center);
        if !profile.tag.is_empty() {
            line = line.push(chip(Icon::Tag, &profile.tag));
        }
        if self.store.is_favorite(profile.id) {
            line = line.push(Icon::Star.widget(16, true));
        }
        if profile.pinned {
            line = line.push(text("Pinned").size(12));
        }
        let open = self.open_count(profile.id);
        if open > 0 {
            line = line.push(text(format!("{open} open")).size(12));
        }
        let base = button(line)
            .width(Length::Fill)
            .padding([5, 8])
            .clip(true)
            .style(selectable(self.selection.contains(&key)))
            .on_press_maybe(
                self.can_browse()
                    .then_some(Message::SelectSerial(profile.id)),
            )
            .into();
        self.tree_context_view(&key, base)
    }

    fn local_row<'a>(&'a self, profile: &'a LocalProfile) -> Element<'a, Message> {
        let key = RowKey::Local(profile.id);
        let open = self.open_count(profile.id);
        let line = row![
            space::horizontal().width(10),
            tile(Icon::Tab, 20, 29),
            column![
                text(&profile.name).size(15).wrapping(text::Wrapping::None),
                text("Local shell").size(13).style(text::secondary),
            ]
            .spacing(1),
            (open > 0).then(|| text(format!("{open} open")).size(12)),
        ]
        .spacing(9)
        .align_y(Alignment::Center);
        let base = button(line)
            .width(Length::Fill)
            .padding([5, 8])
            .clip(true)
            .style(selectable(self.selection.contains(&key)))
            .on_press_maybe(
                self.can_browse()
                    .then_some(Message::SelectLocal(profile.id)),
            )
            .into();
        self.tree_context_view(&key, base)
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
            fact("Connections", self.folder_profile_count(path).to_string()),
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
        let blocked = entry.blocked_server(&self.server_access);
        let connect = browsing && blocked.is_none();
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
                    row![Icon::Connect.widget(15, connect), text("Connect").size(14)]
                        .spacing(6)
                        .align_y(Alignment::Center)
                )
                .on_press_maybe(connect.then_some(Message::Connect(false))),
                tool(
                    Icon::Sftp,
                    "Open SFTP",
                    connect.then_some(Message::Connect(true))
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
                favorite_tool(favorite, browsing),
            ]
            .spacing(5)
            .wrap(),
            button("Connect in New Window")
                .style(button::secondary)
                .on_press_maybe(connect.then_some(Message::ConnectInNewWindow)),
            blocked.map(|(host, port)| text(server_policy_note(host, port)).size(13)),
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

fn group_counts(store: &Store, ids: impl Iterator<Item = u64>) -> BTreeMap<&str, usize> {
    let mut counts = BTreeMap::new();
    for id in ids {
        let mut parent = store.group_of(id);
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

fn favorite_tool(favorite: bool, enabled: bool) -> Element<'static, Message> {
    tool(
        Icon::Star,
        if favorite {
            "Remove favorite"
        } else {
            "Add favorite"
        },
        enabled.then_some(Message::ToggleFavorite),
    )
}

fn heading<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
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

fn server_policy_note(host: &str, port: u16) -> String {
    format!("Managed server policy blocks {host}:{port}.")
}

fn blocked_server_badge(host: &str, port: u16) -> Element<'_, Message> {
    tooltip(
        text("Blocked").size(12).style(text::secondary),
        text(server_policy_note(host, port)),
        tooltip::Position::Bottom,
    )
    .into()
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

#[cfg(test)]
#[test]
fn teamwork_auth_picker_displays_missing_references_without_offering_them() {
    use teamwork::DefaultAuth;

    let passwords = [CredentialChoice {
        id: "password-id".into(),
        label: "Operations".into(),
        username: "operator".into(),
    }];
    let keys = [KeyChoice {
        id: 4,
        name: "Team key".into(),
    }];
    let choices = ViewChoices {
        passwords: &passwords,
        passphrases: &[],
        keys: &keys,
        ai_profiles: &[],
        ai_skills: &[],
        ai_agent_designs: &[],
        highlighting: HighlightChoices::default(),
        themes: &Themes::default(),
        journal_max_parts: None,
        incognito_allowed: true,
        serial_allowed: true,
        terminal_effects: None,
        default_paste_warning: PasteWarningMode::default(),
        minimum_paste_warning: PasteWarningMode::Off,
    };
    for draft in [
        DefaultAuth::Password {
            credential: "missing".into(),
        },
        DefaultAuth::Key {
            id: 99,
            username: "operator".into(),
        },
    ] {
        let (options, selected, available) = teamwork_auth_choices(&draft, &choices);
        assert!(!available);
        assert_eq!(selected.value, draft);
        assert!(selected.label.contains("unavailable"));
        assert!(options.iter().all(|option| option.value != draft));
    }
    let key = DefaultAuth::Key {
        id: 4,
        username: "operator".into(),
    };
    let (options, selected, available) = teamwork_auth_choices(&key, &choices);
    assert!(available);
    assert_eq!(selected.value, key);
    assert!(options.iter().any(|option| {
        option.value
            == DefaultAuth::TemporaryKey {
                username: "operator".into(),
            }
    }));
}
