//! Edits the profile-local design catalog and previews an isolated sample run.

use std::borrow::Cow;

use iced::advanced::graphics::text::Paragraph;
use iced::advanced::text::{self as layout_text, Paragraph as _};
use iced::advanced::widget::operation::{focusable, scope};
use iced::widget::{Id, container};
use iced::widget::{
    button, checkbox, column, pick_list, responsive, row, rule, scrollable, slider, text,
    text_input, tooltip,
};
use iced::{Alignment, Element, Font, Length, Pixels, Size, Task, window};

use crate::terminal_agent::design::{
    Approval, Arrangement, Catalog, History, Layout, Preset, Status,
};
use crate::workspace::tab_titles::WindowSettings;
use crate::workspace::terminal_agent::design::{
    AUTO_HIDE_SECONDS, DockPlacement, DockPreferences, SampleLogKey,
};

use super::{Desktop, Message};

mod module_table;
mod preview;

const TABLE_TEXT_SIZE: f32 = 13.0;
const TABLE_HEADER_FONT: Font = Font {
    weight: iced::font::Weight::Bold,
    ..Font::DEFAULT
};

#[derive(Clone, Debug)]
pub(crate) enum Edit {
    Demo(preview::Action),
    NavigatePreview(preview::Token, SampleLogKey),
    NavigateModule(Module),
    FocusPreview(bool),
    Select(String),
    Default(String),
    Duplicate(String),
    BeginRename(String),
    CancelRename,
    Name(String, String),
    Rename(String),
    Remove(String),
    ConfirmRemove(String, bool),
    Module(String, Module),
    Preview(Option<Module>),
    DockPlacement(DockPlacement),
    DockWidth(u16),
    AutoHide(u8),
    AutoHideInput(String),
    ReducedAnimations(bool),
    TogglePreviewOptions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Module {
    History(History),
    Status(Status),
    Approval(Approval),
    Arrangement(Arrangement),
}

impl Module {
    fn apply(self, layout: &mut Layout) {
        match self {
            Self::History(value) => layout.history = value,
            Self::Status(value) => layout.status = value,
            Self::Approval(value) => layout.approval = value,
            Self::Arrangement(value) => layout.arrangement = value,
        }
    }

    fn choices() -> Vec<Self> {
        History::ALL
            .into_iter()
            .map(Self::History)
            .chain(Status::ALL.into_iter().map(Self::Status))
            .chain(Approval::ALL.into_iter().map(Self::Approval))
            .chain(Arrangement::ALL.into_iter().map(Self::Arrangement))
            .collect()
    }

    fn role(self) -> &'static str {
        match self {
            Self::History(_) => "History",
            Self::Status(_) => "Status",
            Self::Approval(_) => "Approval",
            Self::Arrangement(_) => "Arrangement",
        }
    }

    fn number(self) -> &'static str {
        match self {
            Self::History(History::InlineBlocks) => "03",
            Self::History(History::Timeline) => "02",
            Self::History(History::PlanExecution) => "06",
            Self::History(History::CardStack) => "07",
            Self::History(History::Tmux) => "09",
            Self::History(History::Conversation) => "13",
            Self::History(History::LogTable) => "16",
            Self::History(History::Focus) => "17",
            Self::Status(Status::Metro) => "08",
            Self::Status(Status::Powerline) => "01",
            Self::Status(Status::Minimap) => "11",
            Self::Status(Status::TopBar) => "12",
            Self::Status(Status::Neon) => "18",
            Self::Status(Status::Hud) => "04",
            Self::Status(Status::Spotlight) => "05",
            Self::Status(Status::Toasts) => "10",
            Self::Status(Status::Orbit) => "15",
            Self::Approval(Approval::Cursor) => "14",
            Self::Arrangement(Arrangement::PictureInPicture) => "19",
            Self::Arrangement(Arrangement::Adaptive) => "20",
            _ => "",
        }
    }

    fn name(self) -> String {
        match self {
            Self::History(History::Transcript) => "Classic panel".into(),
            Self::History(History::InlineBlocks) => "Blocks in the terminal".into(),
            Self::History(History::Focus) => "Focus mode".into(),
            Self::Status(Status::Hidden) => "No status display".into(),
            Self::Status(Status::Powerline) => "Powerline status line".into(),
            Self::Status(Status::Hud) => "Frosted glass HUD".into(),
            Self::History(value) => value.to_string(),
            Self::Status(value) => value.to_string(),
            Self::Approval(value) => value.to_string(),
            Self::Arrangement(value) => value.to_string(),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::History(value) => match value {
                History::Transcript => "Activity panel below the terminal",
                History::InlineBlocks => "Activity in blocks above the prompt",
                History::Timeline => "Vertical timeline in a side column",
                History::PlanExecution => "Plan and execution in two columns",
                History::CardStack => "Activity steps shown as stacked cards",
                History::Tmux => "A terminal-style pane next to the session",
                History::Conversation => "Agent activity shown as a conversation",
                History::LogTable => "Sortable table of steps and results",
                History::Focus => "Highlights agent lines in the terminal",
                History::StepCards => "Read individual step cards",
            },
            Self::Status(value) => match value {
                Status::Hidden => "Only the history shows the run",
                Status::Metro => "A slim line of step stations",
                Status::Powerline => "A status line with powerline segments",
                Status::Minimap => "A narrow rail at the terminal edge",
                Status::TopBar => "A bar with an expandable step drawer",
                Status::Neon => "An animated departure-board ticker",
                Status::Hud => "A translucent card above the terminal",
                Status::Spotlight => "A focused prompt and status pill",
                Status::Toasts => "Brief notifications for activity",
                Status::Orbit => "A circular indicator with a step list",
                Status::Pill => "A compact terminal status pill",
                Status::Steps => "A compact strip of terminal steps",
            },
            Self::Approval(Approval::Cursor) => "Review the proposal at the cursor",
            Self::Approval(_) => "Review the proposal beside agent history",
            Self::Arrangement(Arrangement::Normal) => "Keep the design in its fixed placement",
            Self::Arrangement(Arrangement::Floating) => {
                "Show activity in a compact floating window"
            }
            Self::Arrangement(Arrangement::PictureInPicture) => {
                "Move a small history panel over the terminal"
            }
            Self::Arrangement(Arrangement::Adaptive) => "Adapt the dock to the terminal size",
        }
    }
}

impl std::fmt::Display for Module {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::History(value) => write!(formatter, "History · {value}"),
            Self::Status(value) => write!(formatter, "Status · {value}"),
            Self::Approval(value) => write!(formatter, "Approval · {value}"),
            Self::Arrangement(value) => write!(formatter, "Arrangement · {value}"),
        }
    }
}

#[derive(Default)]
pub(super) struct Editor {
    selected: Option<String>,
    name: Option<String>,
    removing: Option<String>,
    notice: &'static str,
    demo: preview::Demo,
    preview_module: Option<Module>,
    pub(super) delay_input: Option<String>,
    show_preview_options: bool,
}

impl Editor {
    pub(super) fn preview_log_enabled(&self, preferences: &WindowSettings) -> bool {
        self.preview_layout(&preferences.ai_agent_designs, preferences.ai_agent_design)
            .history
            == History::LogTable
    }

    pub(super) fn preview_keyboard<'a>(
        &'a self,
        content: Element<'a, Desktop>,
        preferences: &WindowSettings,
        id: window::Id,
    ) -> Element<'a, Desktop> {
        let content = if self.preview_log_enabled(preferences) {
            self.demo.log_keyboard(content, id)
        } else {
            content
        };
        let content = container(content)
            .id(preview_focus_scope(id))
            .width(Length::Fill)
            .height(Length::Fill)
            .into();
        use iced::keyboard::{Key, Modifiers, key::Named};
        crate::workspace::dialog_placement::escape::on_key_press(content, move |key, modifiers| {
            (*key == Key::Named(Named::Tab)
                && (modifiers.is_empty() || modifiers == Modifiers::SHIFT))
                .then(|| {
                    Desktop::Settings(Message::AgentDesignCatalog(
                        id,
                        Edit::FocusPreview(modifiers.shift()),
                    ))
                })
        })
    }

    pub(super) fn navigate_preview(
        &mut self,
        token: preview::Token,
        key: SampleLogKey,
        page: Id,
    ) -> Task<Desktop> {
        self.demo.navigate_log(token, key, page)
    }

    fn selected<'a>(&'a self, catalog: &'a Catalog, preset: Preset) -> &'a str {
        self.selected
            .as_deref()
            .filter(|id| catalog.layout(id).is_some())
            .unwrap_or_else(|| catalog.default_id(preset))
    }

    fn preview_layout(&self, catalog: &Catalog, preset: Preset) -> Layout {
        let mut layout = catalog
            .layout(self.selected(catalog, preset))
            .unwrap_or_default();
        if let Some(module) = self.preview_module {
            module.apply(&mut layout);
        }
        layout
    }

    pub(super) fn replay(&mut self, preferences: &WindowSettings) {
        self.demo.layout =
            self.preview_layout(&preferences.ai_agent_designs, preferences.ai_agent_design);
        self.demo.replay();
    }

    pub(super) fn update(&mut self, catalog: &mut Catalog, preset: &mut Preset, edit: Edit) {
        if let Edit::Demo(action) = edit {
            self.demo.layout = self.preview_layout(catalog, *preset);
            self.demo.update(action);
            return;
        }
        let before = (
            self.selected(catalog, *preset).to_owned(),
            self.preview_layout(catalog, *preset),
        );
        match edit {
            Edit::Select(id) if catalog.layout(&id).is_some() => {
                self.selected = Some(id);
                self.name = None;
                self.removing = None;
                self.notice = "";
                self.preview_module = None;
            }
            Edit::Default(id) => {
                if let Some(selected) = Preset::from_id(&id) {
                    *preset = selected;
                    catalog.clear_custom_default();
                } else if catalog.set_custom_default(&id) {
                    *preset = Preset::Standard;
                }
            }
            Edit::Duplicate(id) => {
                if let Some(choice) = catalog.duplicate(&id) {
                    self.selected = Some(choice.id);
                    self.name = None;
                    self.removing = None;
                    self.notice = "";
                    self.preview_module = None;
                }
            }
            Edit::BeginRename(id)
                if self.selected(catalog, *preset) == id && Preset::from_id(&id).is_none() =>
            {
                self.name = catalog
                    .choices()
                    .into_iter()
                    .find(|choice| choice.id == id)
                    .map(|choice| choice.name);
                self.removing = None;
            }
            Edit::CancelRename => self.name = None,
            Edit::Name(id, name) if self.selected(catalog, *preset) == id => {
                self.selected = Some(id);
                self.name = Some(
                    name[..name.floor_char_boundary(crate::terminal_agent::design::MAX_NAME_BYTES)]
                        .to_owned(),
                );
                self.notice = "";
            }
            Edit::Rename(id) if self.selected(catalog, *preset) == id => {
                if let Some(name) = &self.name {
                    if catalog.rename(&id, name) {
                        self.name = None;
                        self.notice = "Design renamed.";
                    } else {
                        self.notice = "Enter a unique design name without control characters.";
                    }
                }
            }
            Edit::Remove(id)
                if self.selected(catalog, *preset) == id && Preset::from_id(&id).is_none() =>
            {
                self.selected = Some(id.clone());
                self.removing = Some(id);
            }
            Edit::ConfirmRemove(id, confirmed) if self.removing.as_deref() == Some(id.as_str()) => {
                self.removing = None;
                if confirmed {
                    catalog.remove(&id);
                    self.selected = None;
                    self.name = None;
                    self.preview_module = None;
                    self.notice = "Design removed. Connections that used it will use the default.";
                }
            }
            Edit::Module(id, change) if self.selected(catalog, *preset) == id => {
                if let Some(mut layout) = catalog.layout(&id) {
                    let before = layout;
                    change.apply(&mut layout);
                    if before != layout {
                        if Preset::from_id(&id).is_some() {
                            if let Some(choice) = catalog.duplicate(&id) {
                                catalog.set_layout(&choice.id, layout);
                                self.selected = Some(choice.id);
                                self.notice = "";
                            } else {
                                self.notice = "The custom design catalog is full.";
                            }
                        } else {
                            catalog.set_layout(&id, layout);
                        }
                        self.preview_module = Some(change);
                    }
                }
            }
            Edit::Preview(module) => self.preview_module = module,
            Edit::NavigateModule(module) => self.preview_module = Some(module),
            Edit::TogglePreviewOptions => self.show_preview_options = !self.show_preview_options,
            _ => {}
        }
        let after = (
            self.selected(catalog, *preset).to_owned(),
            self.preview_layout(catalog, *preset),
        );
        if before != after {
            self.demo.layout = after.1;
            self.demo.replay();
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        preferences: &'a WindowSettings,
        id: window::Id,
        target: Option<&iced::widget::Id>,
    ) -> Element<'a, Desktop> {
        let msg = move |edit| Desktop::Settings(Message::AgentDesignCatalog(id, edit));
        let catalog = &preferences.ai_agent_designs;
        let preset = preferences.ai_agent_design;
        let dock = preferences.ai_agent_panel;
        let mut choices = catalog.choices();
        let selected = self.selected(catalog, preset);
        let layout = catalog.layout(selected).unwrap_or_default();
        let custom = Preset::from_id(selected).is_none();
        let is_default = catalog.default_id(preset) == selected;
        let default_id = catalog.default_id(preset);
        for choice in &mut choices {
            if choice.id == Preset::Standard.id() {
                choice.name = "Standard (blocks + metro line)".into();
            }
            if choice.id == default_id {
                choice.name.insert_str(0, "★ ");
            }
        }
        let display_choice = choices.iter().find(|choice| choice.id == selected).cloned();
        let option = |label, action| {
            crate::workspace::dialog_design::secondary_action(label).on_press_maybe(action)
        };
        let toolbar = row![
            text("Combination:").size(13),
            tooltip(
                pick_list(choices, display_choice, move |choice| msg(Edit::Select(
                    choice.id
                )))
                .width(260)
                .text_size(13),
                text(Preset::from_id(selected).map_or("Custom combination", Preset::description))
                    .size(13),
                tooltip::Position::Bottom,
            ),
            option(
                "New",
                catalog
                    .can_duplicate()
                    .then(|| msg(Edit::Duplicate(Preset::Standard.id().into())))
            ),
            option(
                "Duplicate",
                catalog
                    .can_duplicate()
                    .then(|| msg(Edit::Duplicate(selected.into())))
            ),
            option(
                "Rename",
                custom.then(|| msg(Edit::BeginRename(selected.into())))
            ),
            option("Delete", custom.then(|| msg(Edit::Remove(selected.into())))),
            option(
                "Use as Default",
                (!is_default).then(|| msg(Edit::Default(selected.into())))
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let state = match (is_default, custom) {
            (true, false) => {
                "★ Default for connections without a choice · Changing a built-in creates a copy."
            }
            (true, true) => "★ Default for connections without a choice",
            (false, false) => "Changing a built-in creates a copy.",
            (false, true) => "",
        };
        let rename = self.name.as_deref().filter(|_| custom).map(|name| {
            row![
                crate::settings_search::mark_input(
                    text_input("Design name", name)
                        .on_input(move |name| msg(Edit::Name(selected.into(), name)))
                        .on_submit(msg(Edit::Rename(selected.into()))),
                    target,
                )
                .width(260),
                option("Rename", Some(msg(Edit::Rename(selected.into())))),
                option("Cancel", Some(msg(Edit::CancelRename))),
            ]
            .spacing(8)
            .wrap()
        });
        let remove = self
            .removing
            .as_deref()
            .filter(|id| *id == selected)
            .map(|_| {
                row![
                    text("Delete this design?").size(13),
                    option(
                        "Delete",
                        Some(msg(Edit::ConfirmRemove(selected.into(), true)))
                    ),
                    option(
                        "Cancel",
                        Some(msg(Edit::ConfirmRemove(selected.into(), false)))
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap()
            });

        let mut last_role = "";
        let modules = column(
            Module::choices()
                .into_iter()
                .enumerate()
                .map(|(index, module)| {
                    let mut changed = layout;
                    module.apply(&mut changed);
                    let active = changed == layout;
                    let role = if module.role() == last_role {
                        ""
                    } else {
                        module.role()
                    };
                    last_role = module.role();
                    let choose = checkbox(active).size(17).on_toggle_maybe(
                        (!active && (custom || catalog.can_duplicate()))
                            .then(|| move |_| msg(Edit::Module(selected.into(), module))),
                    );
                    let preview = button(module_row(
                        role,
                        module.number(),
                        module.name(),
                        module.description(),
                        false,
                    ))
                    .padding(0)
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .style(button::text)
                    .on_press(msg(Edit::Preview(Some(module))));
                    let selected = self.preview_module == Some(module);
                    container(
                        row![container(choose).center_x(54), rule::vertical(1), preview]
                            .align_y(Alignment::Center)
                            .height(25),
                    )
                    .id(module_row_id(id, module))
                    .style(move |theme| {
                        crate::workspace::dialog_design::table_row(theme, selected, index % 2 != 0)
                    })
                    .into()
                }),
        );
        let table = column![
            container(
                row![
                    container(text("Active").size(TABLE_TEXT_SIZE).font(TABLE_HEADER_FONT))
                        .center_x(54),
                    rule::vertical(1),
                    module_row("Role", "No.", "Design".into(), "Description", true),
                ]
                .align_y(Alignment::Center)
            )
            .height(26)
            .center_y(26)
            .style(crate::workspace::dialog_design::header),
            container(
                scrollable(modules)
                    .id(module_list_id(id))
                    .spacing(2)
                    .height(Length::Fill)
            )
            .style(crate::workspace::dialog_design::sidebar),
        ]
        .height(420);
        let table = module_table::view(
            table.into(),
            self.preview_module,
            custom || catalog.can_duplicate(),
            selected,
            msg,
        );
        let delay = preferences.agent_status_auto_hide_seconds();
        use crate::workspace::dialog_design::number_step;
        let options = row![
            text("Hide the status bar after a run (seconds, 0 = keep):").size(13),
            text_input(
                "0–60",
                self.delay_input.as_deref().unwrap_or(&delay.to_string())
            )
            .width(52)
            .size(13)
            .on_input(move |input| msg(Edit::AutoHideInput(input))),
            column![
                number_step(
                    "▴",
                    (delay < *AUTO_HIDE_SECONDS.end()).then(|| msg(Edit::AutoHide(delay + 1)))
                ),
                number_step("▾", (delay > 0).then(|| msg(Edit::AutoHide(delay - 1)))),
            ],
            checkbox(
                preferences
                    .ai_agent_design_reduced_animations
                    .unwrap_or_default()
            )
            .size(17)
            .text_size(13)
            .label("Reduce animations")
            .on_toggle(move |reduced| msg(Edit::ReducedAnimations(reduced))),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .wrap();
        let preview = preview::view(
            &self.demo,
            self.preview_layout(catalog, preset),
            dock,
            delay,
            !preferences.agent_animations(),
            self.show_preview_options,
            id,
        );
        let center = row![
            container(table).width(Length::FillPortion(7)),
            container(preview).width(Length::FillPortion(8)),
        ]
        .spacing(12);
        let dock_options = self.show_preview_options.then(|| {
            row![
                text("Agent panel placement").size(13),
                pick_list(
                    DockPlacement::ALL,
                    Some(dock.placement.unwrap_or_default()),
                    move |placement| { msg(Edit::DockPlacement(placement)) }
                ),
                text(format!("Panel width: {} px", dock.width())).size(13),
                slider(
                    DockPreferences::WIDTH_RANGE,
                    dock.width(),
                    move |width| msg(Edit::DockWidth(width))
                )
                .width(180),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .wrap()
        });

        column![
            text("Pick how agent runs are shown in the terminal: one design per role. The preview plays a sample run.").size(13),
            toolbar,
            text(state).size(13).style(text::secondary),
            rename,
            remove,
            (!self.notice.is_empty()).then(|| text(self.notice).size(13)),
            center,
            options,
            text("Each connection can pick its own combination in the Connection Manager.").size(13).style(text::secondary),
            dock_options,
        ].spacing(10).into()
    }
}

fn module_row<'a>(
    role: &'a str,
    number: &'a str,
    name: String,
    description: &'a str,
    header: bool,
) -> Element<'a, Desktop> {
    let cell = |label: String, width: Length| {
        let label = text(label)
            .size(TABLE_TEXT_SIZE)
            .font_maybe(header.then_some(TABLE_HEADER_FONT))
            .wrapping(text::Wrapping::None);
        let align = if header {
            Alignment::Center
        } else {
            Alignment::Start
        };
        container(label)
            .padding([0, 3])
            .width(width)
            .align_x(align)
            .clip(true)
    };
    let description: Element<'_, Desktop> = if header {
        cell(description.to_owned(), Length::Fill).into()
    } else {
        let label = responsive(move |size| {
            text(description_with_ellipsis(description, size.width))
                .size(TABLE_TEXT_SIZE)
                .font(Font::DEFAULT)
                .shaping(text::Shaping::Advanced)
                .wrapping(text::Wrapping::None)
                .into()
        })
        .height(Length::Shrink);
        tooltip(
            container(label).padding([0, 3]).clip(true),
            text(description).size(TABLE_TEXT_SIZE),
            tooltip::Position::Bottom,
        )
        .gap(4)
        .delay(std::time::Duration::from_millis(500))
        .style(crate::workspace::dialog_design::header)
        .into()
    };
    row![
        cell(role.to_owned(), Length::Fixed(89.0)),
        rule::vertical(1),
        cell(number.to_owned(), Length::Fixed(39.0)),
        rule::vertical(1),
        cell(name, Length::Fixed(163.0)),
        rule::vertical(1),
        description,
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .into()
}

fn description_with_ellipsis(description: &str, width: f32) -> Cow<'_, str> {
    if description_width(description) <= width {
        return Cow::Borrowed(description);
    }
    const ELLIPSIS: &str = "…";
    if description_width(ELLIPSIS) > width {
        return Cow::Borrowed("");
    }
    let boundaries: Vec<usize> = description.char_indices().map(|(index, _)| index).collect();
    let mut shortened = String::with_capacity(description.len());
    let count = boundaries.partition_point(|&end| {
        shortened.clear();
        shortened.push_str(&description[..end]);
        shortened.push_str(ELLIPSIS);
        description_width(&shortened) <= width
    });
    shortened.clear();
    let end = boundaries[..count].last().copied().unwrap_or(0);
    shortened.push_str(&description[..end]);
    shortened.push_str(ELLIPSIS);
    Cow::Owned(shortened)
}

fn description_width(description: &str) -> f32 {
    Paragraph::with_text(layout_text::Text {
        content: description,
        bounds: Size::INFINITE,
        size: Pixels(TABLE_TEXT_SIZE),
        line_height: layout_text::LineHeight::default(),
        font: Font::DEFAULT,
        align_x: layout_text::Alignment::Default,
        align_y: iced::alignment::Vertical::Top,
        shaping: layout_text::Shaping::Advanced,
        wrapping: layout_text::Wrapping::None,
    })
    .min_width()
}

fn preview_focus_scope(id: window::Id) -> Id {
    Id::from(format!("agent-design-settings/{id:?}"))
}

fn module_list_id(id: window::Id) -> Id {
    Id::from(format!("agent-design-modules/{id:?}"))
}

fn module_row_id(id: window::Id, module: Module) -> Id {
    Id::from(format!("agent-design-modules/{id:?}/{module:?}"))
}

pub(super) fn reveal_module(id: window::Id, module: Module) -> Task<Desktop> {
    crate::workspace::coding_agents::roster_scroll::reveal_row(
        module_list_id(id),
        module_row_id(id, module),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_description_fits_its_measured_width_and_preserves_utf8() {
        let description = "Ä narrow rail at the edge of the terminal";
        assert_eq!(description_with_ellipsis(description, 0.0), "");
        assert_eq!(
            description_with_ellipsis(description, f32::INFINITY),
            description
        );
        let width = description_width("Ä narrow rail…");
        let clipped = description_with_ellipsis(description, width);
        let prefix = clipped.strip_suffix('…').unwrap();
        assert!(description.starts_with(prefix));
        assert!(!prefix.is_empty());
        assert!(description_width(&clipped) <= width);
    }

    #[test]
    fn activating_a_built_in_module_creates_a_copy_and_keeps_the_default() {
        let mut catalog = Catalog::default();
        let mut preset = Preset::Standard;
        let mut editor = Editor::default();
        let original = preset.layout();
        editor.update(
            &mut catalog,
            &mut preset,
            Edit::Preview(Some(Module::Status(Status::Orbit))),
        );
        assert_eq!(catalog.layout("standard"), Some(original));
        assert!(editor.selected.is_none());
        editor.update(
            &mut catalog,
            &mut preset,
            Edit::Module("standard".into(), Module::Status(Status::Orbit)),
        );
        let selected = editor.selected(&catalog, preset);
        assert_ne!(selected, "standard");
        assert_eq!(
            catalog.layout(selected),
            Some(Layout {
                status: Status::Orbit,
                ..original
            })
        );
        assert_eq!(catalog.layout("standard"), Some(original));
        assert_eq!(catalog.default_id(preset), "standard");
    }

    #[test]
    fn removing_the_custom_default_requires_confirmation_and_restores_the_builtin() {
        let mut catalog = Catalog::default();
        let mut preset = Preset::Standard;
        let mut editor = Editor::default();
        editor.update(&mut catalog, &mut preset, Edit::Duplicate("classic".into()));
        let selected = editor.selected(&catalog, preset).to_owned();
        editor.update(&mut catalog, &mut preset, Edit::Default(selected.clone()));

        editor.update(&mut catalog, &mut preset, Edit::Remove(selected.clone()));
        assert_eq!(catalog.default_id(preset), selected);
        editor.update(
            &mut catalog,
            &mut preset,
            Edit::ConfirmRemove(selected.clone(), false),
        );
        assert_eq!(catalog.default_id(preset), selected);
        assert!(catalog.layout(&selected).is_some());

        editor.update(&mut catalog, &mut preset, Edit::Remove(selected.clone()));
        editor.update(
            &mut catalog,
            &mut preset,
            Edit::ConfirmRemove(selected.clone(), true),
        );
        assert!(catalog.layout(&selected).is_none());
        assert_eq!(catalog.default_id(preset), Preset::Standard.id());
        assert_eq!(editor.selected(&catalog, preset), Preset::Standard.id());
    }
}

pub(super) fn focus_preview(id: window::Id, page: Id, backwards: bool) -> Task<Desktop> {
    use crate::workspace::coding_agents::roster_scroll::reveal_focused;

    let root = preview_focus_scope(id);
    let action = if backwards {
        iced_runtime::Action::widget(scope(root.clone(), focusable::focus_previous()))
    } else {
        iced_runtime::Action::widget(scope(root.clone(), focusable::focus_next()))
    };
    iced_runtime::task::effect(action).chain(reveal_focused(root, page))
}
