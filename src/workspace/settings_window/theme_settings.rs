use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use iced::font::Weight;
use iced::widget::image;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, slider, space, text,
    text_input,
};
use iced::{Alignment, Color, ContentFit, Element, Font, Length, Task};
use kortty_terminal::CursorShape;

use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::terminal_theme::{AgentPanelColors, CursorStyle, FontFamily, Id, Rgb, Scheme, Themes};
use crate::terminal_view::ansi_color;
use crate::terminal_view::background_image::{AssetId, Images, Prepared};

pub(super) mod background;
pub(super) mod exchange;

#[derive(Clone, Copy, Debug)]
pub enum Channel {
    Foreground,
    Background,
    Cursor,
}

impl Channel {
    const ALL: [Self; 3] = [Self::Foreground, Self::Background, Self::Cursor];

    fn index(self) -> usize {
        match self {
            Self::Foreground => 0,
            Self::Background => 1,
            Self::Cursor => 2,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Foreground => "Foreground",
            Self::Background => "Background",
            Self::Cursor => "Cursor color",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Select(Option<Id>),
    ApplyFont(bool),
    New,
    Modify,
    Duplicate,
    Delete,
    ConfirmDelete,
    Cancel,
    Name(String),
    Family(FontFamily),
    FontSize(u8),
    Color(Channel, String),
    Cursor(CursorStyle),
    Apply,
    AgentColor(usize, String),
    Background(background::Action),
    Java(exchange::Action),
}

#[derive(Default)]
pub(super) struct Editor {
    form: Option<Form>,
    deletion: Option<Id>,
    problem: Option<&'static str>,
    exchange: exchange::Editor,
    background_revision: u64,
    staged_backgrounds: BTreeMap<AssetId, Arc<Prepared>>,
}

struct Form {
    id: Option<Id>,
    name: String,
    scheme: Scheme,
    colors: [String; 3],
    agent_colors: [String; 6],
    background: background::Draft,
}

const AGENT_FIELDS: [&str; 6] = [
    "Background",
    "Border",
    "Text",
    "Muted text",
    "Accent",
    "Error",
];

impl Form {
    fn new(id: Option<Id>, name: String, scheme: Scheme) -> Self {
        Self {
            id,
            name,
            scheme,
            colors: [scheme.foreground, scheme.background, scheme.cursor]
                .map(|color| color.to_string()),
            agent_colors: Default::default(),
            background: background::Draft::default(),
        }
    }

    fn from_theme(
        id: Option<Id>,
        name: String,
        theme: crate::terminal_theme::Theme<'_>,
        themes: &Themes,
    ) -> Self {
        let mut form = Self::new(id, name, theme.scheme);
        form.agent_colors = themes
            .agent_panel_colors(theme.id)
            .unwrap_or_default()
            .map(|color| color.map_or_else(String::new, |color| color.to_string()));
        form.background = background::Draft::new(
            themes.background_asset(theme.id),
            id.and_then(|id| themes.java_background_path(id)),
        );
        form
    }

    fn agent_colors(&self) -> Option<AgentPanelColors> {
        let mut colors = [None; 6];
        for (output, input) in colors.iter_mut().zip(&self.agent_colors) {
            if !input.trim().is_empty() {
                *output = Some(Rgb::parse(input)?);
            }
        }
        Some(colors)
    }

    fn parsed(&self) -> Option<Scheme> {
        Some(Scheme {
            foreground: Rgb::parse(&self.colors[0])?,
            background: Rgb::parse(&self.colors[1])?,
            cursor: Rgb::parse(&self.colors[2])?,
            ..self.scheme
        })
    }
}

impl Editor {
    pub(super) fn editing(&self) -> bool {
        self.form.is_some() || self.deletion.is_some() || self.exchange.open()
    }

    pub(super) fn cancel_message(&self) -> Option<Edit> {
        if self.exchange.open() {
            Some(Edit::Java(exchange::Action::Cancel))
        } else {
            self.editing().then_some(Edit::Cancel)
        }
    }

    pub(super) fn exchanging(&self) -> Option<&'static str> {
        self.exchange.working()
    }

    pub(super) fn leave(&mut self) {
        self.exchange.close();
        if let Some(form) = &mut self.form {
            form.background.cancel_read();
        }
    }

    pub(super) fn publish_backgrounds(&self, themes: &Themes, root: &Path) -> std::io::Result<()> {
        for (&id, asset) in &self.staged_backgrounds {
            if themes.background_assets().any(|reference| reference == id) {
                asset.publish(root)?;
            }
        }
        Ok(())
    }

    pub(super) fn has_staged_backgrounds(&self) -> bool {
        !self.staged_backgrounds.is_empty()
    }

    fn background_handle<'a>(
        &'a self,
        images: &'a Images,
        asset: Option<AssetId>,
    ) -> Option<&'a image::Handle> {
        let id = asset?;
        self.staged_backgrounds
            .get(&id)
            .map(|image| image.handle())
            .or_else(|| images.get(id).map(|image| image.handle()))
    }

    pub(super) fn update(&mut self, themes: &mut Themes, message: Edit) -> Task<Edit> {
        if let Edit::Java(action) = message {
            return if self.form.is_none() && self.deletion.is_none() {
                self.exchange.update(themes, action).map(Edit::Java)
            } else {
                Task::none()
            };
        }
        if self.exchange.open() {
            return Task::none();
        }
        self.problem = None;
        if let Some(form) = &mut self.form {
            match message {
                Edit::Name(mut name) => {
                    name.truncate(name.floor_char_boundary(72));
                    form.name = name;
                }
                Edit::Family(family) => form.scheme.family = family,
                Edit::FontSize(size) => {
                    form.scheme.font_size = size.clamp(MIN_TERMINAL_FONT, MAX_TERMINAL_FONT)
                }
                Edit::Color(channel, mut color) => {
                    color.truncate(color.floor_char_boundary(12));
                    form.colors[channel.index()] = color;
                }
                Edit::Cursor(style) => form.scheme.cursor_style = style,
                Edit::AgentColor(index, mut color) => {
                    color.truncate(color.floor_char_boundary(12));
                    if let Some(value) = form.agent_colors.get_mut(index) {
                        *value = color;
                    }
                }
                Edit::Background(action) => {
                    return form
                        .background
                        .update(action, &mut self.background_revision)
                        .map(Edit::Background);
                }
                Edit::Cancel => self.form = None,
                Edit::Apply if !form.background.working() => {
                    let result = form
                        .parsed()
                        .ok_or("Enter all three colors as #RRGGBB.")
                        .and_then(|scheme| {
                            let colors = form.agent_colors().ok_or(
                                "Enter Agent panel colors as #RRGGBB or leave them blank.",
                            )?;
                            themes
                                .store(form.id, &form.name, scheme)
                                .map(|id| (id, colors))
                        });
                    match result {
                        Ok((id, colors)) => {
                            themes.set_background_asset(id, form.background.asset);
                            themes.set_agent_panel_colors(id, colors);
                            if let Some(prepared) = form.background.prepared.take() {
                                self.staged_backgrounds.insert(prepared.id(), prepared);
                            }
                            self.staged_backgrounds.retain(|id, _| {
                                themes.background_assets().any(|current| current == *id)
                            });
                            self.form = None;
                        }
                        Err(problem) => self.problem = Some(problem),
                    }
                }
                _ => {}
            }
            return Task::none();
        }
        if let Some(id) = self.deletion {
            match message {
                Edit::ConfirmDelete => {
                    themes.remove(id);
                    self.staged_backgrounds
                        .retain(|id, _| themes.background_assets().any(|current| current == *id));
                    self.deletion = None;
                }
                Edit::Cancel => self.deletion = None,
                _ => {}
            }
            return Task::none();
        }
        let selected = themes.selected().and_then(|id| themes.find(id));
        match message {
            Edit::Select(id) => themes.select(id),
            Edit::ApplyFont(value) => themes.apply_font = value,
            Edit::New if themes.can_add() => {
                self.form = Some(Form::new(None, String::new(), Scheme::default()))
            }
            Edit::Modify => {
                if let Some(theme) = selected {
                    self.form = Some(Form::from_theme(
                        Some(theme.id),
                        theme.name.into(),
                        theme,
                        themes,
                    ));
                }
            }
            Edit::Duplicate if themes.can_add() => {
                if let Some(theme) = selected {
                    let prefix = &theme.name[..theme.name.floor_char_boundary(67)];
                    self.form = Some(Form::from_theme(
                        None,
                        format!("{prefix} Copy"),
                        theme,
                        themes,
                    ));
                }
            }
            Edit::Delete => {
                self.deletion = selected
                    .filter(|theme| !theme.id.built_in())
                    .map(|theme| theme.id)
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn view<'a>(
        &'a self,
        preferences: &'a super::WindowSettings,
        images: &'a Images,
        target: Option<&iced::widget::Id>,
    ) -> Element<'a, Edit> {
        let themes = &preferences.themes;
        if let Some(page) = self.exchange.view(themes, target) {
            return page.map(Edit::Java);
        }
        if let Some(form) = &self.form {
            return self.form_view(
                form,
                self.background_handle(images, form.background.asset),
                target,
            );
        }
        if let Some(theme) = self.deletion.and_then(|id| themes.find(id)) {
            return column![
                text("Delete Theme").size(19),
                text(format!("Delete theme ‘{}’?", theme.name)),
                row![
                    button("Cancel").on_press(Edit::Cancel),
                    button("Delete")
                        .style(button::danger)
                        .on_press(Edit::ConfirmDelete)
                ]
                .spacing(8),
            ]
            .spacing(10)
            .into();
        }
        let selected = themes.selected().and_then(|id| themes.find(id));
        let mut choices = column![choice("kortty defaults", None, themes.selected())].spacing(2);
        for theme in themes.iter() {
            let label = if theme.id.built_in() {
                format!("{} (built-in)", theme.name)
            } else {
                theme.name.into()
            };
            choices = choices.push(choice(label, Some(theme.id), themes.selected()));
        }
        let list = column![
            container(scrollable(choices).height(260)).style(container::bordered_box),
            button("Add")
                .width(Length::Fill)
                .on_press_maybe(themes.can_add().then_some(Edit::New)),
            button("Edit")
                .width(Length::Fill)
                .on_press_maybe(selected.is_some().then_some(Edit::Modify)),
            button("Duplicate").width(Length::Fill).on_press_maybe(
                (selected.is_some() && themes.can_add()).then_some(Edit::Duplicate)
            ),
            button("Delete").width(Length::Fill).on_press_maybe(
                selected
                    .is_some_and(|theme| !theme.id.built_in())
                    .then_some(Edit::Delete)
            ),
        ]
        .width(235)
        .spacing(5);
        let (name, scheme) = selected.map_or_else(
            || {
                (
                    "kortty",
                    Scheme {
                        foreground: Rgb::from_hex(0xDBE3ED),
                        background: Rgb::from_hex(0x0E1014),
                        cursor: Rgb::from_hex(0xDBE3ED),
                        ..Scheme::default()
                    },
                )
            },
            |theme| (theme.name, theme.scheme),
        );
        let size = if themes.apply_font && selected.is_some() {
            scheme.font_size
        } else {
            preferences.terminal_font
        };
        let font = preferences.terminal_font_face(selected.map(|theme| theme.scheme));
        let mut preview_pane = column![
            text("Theme Preview").size(16),
            preview(name, scheme, font, size)
        ]
        .spacing(8)
        .width(Length::Fill);
        let asset = selected.and_then(|theme| themes.background_asset(theme.id));
        if let Some(handle) = self.background_handle(images, asset) {
            preview_pane = preview_pane.push(
                image(handle.clone())
                    .width(180)
                    .height(100)
                    .content_fit(ContentFit::Contain),
            );
        } else if asset.is_some() {
            preview_pane = preview_pane.push(text("Background image unavailable.").size(12));
        }
        let mut page = column![
            text("Terminal Themes").size(20),
            text("Create and manage color themes for the terminal and application.").size(13),
            checkbox(themes.apply_font)
                .label("When enabled, applying a theme also changes font family and size.")
                .on_toggle(Edit::ApplyFont),
            row![list, preview_pane].spacing(16),
            row![
                button("Import Java Themes…")
                    .on_press(Edit::Java(exchange::Action::Open(exchange::Mode::Import))),
                button("Export Java Themes…").on_press_maybe(
                    themes
                        .iter()
                        .any(|theme| !theme.id.built_in())
                        .then_some(Edit::Java(exchange::Action::Open(exchange::Mode::Export)))
                ),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(12);
        if let Some(notice) = self.exchange.notice() {
            page = page.push(text(notice).size(13));
        }
        page.into()
    }

    fn form_view<'a>(
        &'a self,
        form: &'a Form,
        background: Option<&image::Handle>,
        target: Option<&iced::widget::Id>,
    ) -> Element<'a, Edit> {
        let mut controls = column![
            text(if form.id.is_some() {
                "Edit Theme"
            } else {
                "New Theme"
            })
            .size(20),
            field(
                "Name",
                crate::settings_search::mark_input(
                    text_input("Theme name", &form.name).on_input(Edit::Name),
                    target,
                ),
            ),
            field(
                "Font family",
                pick_list(FontFamily::ALL, Some(form.scheme.family), Edit::Family)
            ),
            text("Unavailable families use the system monospace font.").size(12),
            field(
                "Font size",
                row![
                    slider(
                        MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT,
                        form.scheme.font_size,
                        Edit::FontSize
                    )
                    .width(165),
                    text(format!("{} pt", form.scheme.font_size)),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            ),
        ]
        .spacing(8);
        for channel in Channel::ALL {
            let input = &form.colors[channel.index()];
            controls = controls.push(field(
                channel.label(),
                row![
                    text_input("#RRGGBB", input)
                        .width(110)
                        .on_input(move |value| Edit::Color(channel, value)),
                    swatch(
                        Rgb::parse(input).map_or(Color::TRANSPARENT, Rgb::color),
                        24.0,
                        20.0
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ));
        }
        controls = controls.push(field(
            "Cursor style",
            pick_list(
                CursorStyle::ALL,
                Some(form.scheme.cursor_style),
                Edit::Cursor,
            ),
        ));
        controls = controls
            .push(form.background.view(background).map(Edit::Background))
            .push(text("Agent panel colors").size(16));
        let scheme = form.parsed().unwrap_or(form.scheme);
        let derived = form
            .agent_colors()
            .filter(|colors| colors.iter().any(Option::is_some))
            .map(|colors| {
                crate::workspace::terminal_agent::palette::resolved_panel_colors(scheme, colors)
            });
        for (index, (label, input)) in AGENT_FIELDS.into_iter().zip(&form.agent_colors).enumerate()
        {
            let color = Rgb::parse(input).or_else(|| derived.map(|colors| colors[index]));
            controls = controls.push(field(
                label,
                row![
                    text_input("Default", input)
                        .width(110)
                        .on_input(move |value| Edit::AgentColor(index, value)),
                    swatch(color.map_or(Color::TRANSPARENT, Rgb::color), 24.0, 20.0),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ));
        }
        if let Some(problem) = self.problem {
            controls = controls.push(text(problem).style(text::danger).size(13));
        }
        controls = controls.push(
            row![
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Edit::Cancel),
                button("Save theme").on_press_maybe(
                    (!form.name.trim().is_empty() && !form.background.working())
                        .then_some(Edit::Apply),
                ),
            ]
            .spacing(8),
        );
        row![
            controls,
            preview(&form.name, scheme, scheme.family.font(), scheme.font_size)
        ]
        .spacing(14)
        .into()
    }
}

fn field<'a>(label: &'static str, widget: impl Into<Element<'a, Edit>>) -> Element<'a, Edit> {
    row![text(label).width(115), widget.into()]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
}

fn choice<'a>(
    label: impl iced::widget::text::IntoFragment<'a>,
    id: Option<Id>,
    selected: Option<Id>,
) -> Element<'a, Edit> {
    button(text(label).size(13))
        .width(Length::Fill)
        .style(if id == selected {
            button::primary
        } else {
            button::text
        })
        .on_press(Edit::Select(id))
        .into()
}

fn swatch(color: Color, width: f32, height: f32) -> Element<'static, Edit> {
    container(space::horizontal())
        .width(width)
        .height(height)
        .style(move |_| container::Style::default().background(color))
        .into()
}

fn preview(name: &str, scheme: Scheme, font: Font, size: u8) -> Element<'static, Edit> {
    let bright = Font {
        weight: Weight::Bold,
        ..font
    };
    let size = f32::from(size);
    let fg = scheme.foreground.color();
    let bg = scheme.background.color();
    let run = |value: String, color, bold| {
        text(value)
            .size(size)
            .color(color)
            .font(if bold { bright } else { font })
    };
    let host = preview_host(name);
    let prompt = |command: &str| {
        row![
            run(format!("user@{host}"), ansi_color(10), true),
            run(":".into(), fg, false),
            run("~/projects".into(), ansi_color(12), true),
            run(format!("$ {command}"), fg, false),
        ]
        .align_y(Alignment::Center)
    };
    let file = |attributes: &str, name: &str, color| {
        row![
            run(attributes.into(), fg, false),
            run(name.into(), color, true)
        ]
    };
    let colors = (0..16).fold(row![].spacing(1), |row, index| {
        row.push(swatch(ansi_color(index), size * 1.22, size))
    });
    let (width, height) = match scheme.cursor_style.behavior().0 {
        CursorShape::Block => (size * 0.61, size),
        CursorShape::Underline => (size * 0.61, 2.0),
        CursorShape::Bar => (2.0, size),
    };
    let cursor = container(swatch(scheme.cursor.color(), width, height))
        .height(size)
        .align_y(Alignment::End);
    let sample = column![
        prompt("ls --color -l"),
        file(
            "drwxr-xr-x  5 user staff   160 Oct 17 09:12 ",
            "docs/",
            ansi_color(12)
        ),
        file(
            "drwxr-xr-x 12 user staff   384 Oct 17 09:12 ",
            "src/",
            ansi_color(12)
        ),
        file(
            "-rwxr-xr-x  1 user staff  2048 Oct 16 18:40 ",
            "build.sh",
            ansi_color(10)
        ),
        row![
            run(
                "lrwxr-xr-x  1 user staff    11 Oct 15 11:03 ".into(),
                fg,
                false
            ),
            run("current".into(), ansi_color(14), true),
            run(" -> ".into(), fg, false),
            run("release-2/".into(), ansi_color(12), true)
        ],
        file(
            "-rw-r--r--  1 user staff 51200 Oct 14 07:55 ",
            "backup.tar.gz",
            ansi_color(1)
        ),
        run(
            "-rw-r--r--  1 user staff  1337 Oct 13 22:10 README.md".into(),
            fg,
            false
        ),
        prompt("git status --short"),
        row![
            run(" M ".into(), ansi_color(1), false),
            run("src/main.c".into(), fg, false)
        ],
        row![
            run("A  ".into(), ansi_color(2), false),
            run("src/net/tunnel.c".into(), fg, false)
        ],
        row![
            run("?? ".into(), ansi_color(1), false),
            run("notes.txt".into(), fg, false)
        ],
        prompt("make test"),
        row![
            run("warning:".into(), ansi_color(3), true),
            run(" tunnel.c:42: unused variable 'retries'".into(), fg, false)
        ],
        row![
            run("error:".into(), ansi_color(9), true),
            run(
                " main.c:108: expected ';' before 'return'".into(),
                fg,
                false
            )
        ],
        row![
            run("PASS".into(), ansi_color(2), true),
            run(" 24  ".into(), fg, false),
            run("FAIL".into(), ansi_color(1), true),
            run(" 1  ".into(), fg, false),
            run("SKIP".into(), ansi_color(5), true),
            run(" 3".into(), fg, false)
        ],
        colors,
        prompt("").push(cursor),
    ]
    .spacing(2)
    .padding([12, 14]);
    let terminal = scrollable(sample)
        .direction(scrollable::Direction::Both {
            horizontal: scrollable::Scrollbar::default(),
            vertical: scrollable::Scrollbar::default(),
        })
        .height(360)
        .width(Length::Fill);
    column![
        container(terminal).style(move |_| container::Style::default().background(bg)).width(Length::Fill),
        text("Sample session showing the theme’s colors, font and cursor with the terminal’s 16 ANSI colors.").size(12),
    ].spacing(6).width(Length::Fill).into()
}

fn preview_host(name: &str) -> String {
    let mut host = String::new();
    for segment in name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|segment| !segment.is_empty())
    {
        if !host.is_empty() {
            host.push('-');
        }
        host.push_str(&segment.to_ascii_lowercase());
    }
    if host.is_empty() {
        "theme".into()
    } else {
        host
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{StreamExt, executor::block_on};
    use iced_runtime::{Action, task};

    #[test]
    fn theme_form_stages_image_and_panel_color_until_settings_save() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let source = root.join("wallpaper.png");
        ::image::RgbaImage::from_pixel(3, 2, ::image::Rgba([80, 96, 112, 255]))
            .save(&source)
            .unwrap();
        let mut editor = Editor::default();
        let mut themes = Themes::default();
        let load = |editor: &mut Editor, themes: &mut Themes| {
            let _ = editor.update(
                themes,
                Edit::Background(background::Action::Path(source.to_str().unwrap().into())),
            );
            let task = editor.update(themes, Edit::Background(background::Action::Preview));
            let mut results = task::into_stream(task).unwrap();
            let Some(Action::Output(result)) = block_on(results.next()) else {
                panic!("expected the prepared image");
            };
            result
        };
        let _ = editor.update(&mut themes, Edit::New);
        let obsolete = load(&mut editor, &mut themes);
        let _ = editor.update(&mut themes, Edit::Cancel);
        let _ = editor.update(&mut themes, Edit::New);
        let _ = editor.update(&mut themes, obsolete);
        assert!(editor.form.as_ref().unwrap().background.asset.is_none());
        let loaded = load(&mut editor, &mut themes);
        let _ = editor.update(&mut themes, loaded);
        let _ = editor.update(&mut themes, Edit::Name("Preview".into()));
        let _ = editor.update(&mut themes, Edit::AgentColor(4, "#AA88CC".into()));
        let _ = editor.update(&mut themes, Edit::Apply);
        let id = themes.selected().unwrap();
        let asset = themes.background_asset(id).unwrap();
        assert_eq!(
            themes.agent_panel_colors(id).unwrap()[4],
            Some(Rgb::from_hex(0xAA88CC))
        );
        assert!(!root.join(asset.relative_path()).exists());
        editor.publish_backgrounds(&themes, &root).unwrap();
        assert!(root.join(asset.relative_path()).is_file());
    }
}
