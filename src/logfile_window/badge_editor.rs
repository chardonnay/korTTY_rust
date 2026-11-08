use std::collections::BTreeSet;
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, scrollable, slider, space, text,
    text_input, tooltip,
};
use iced::{Alignment, Border, Element, Length};

use crate::terminal_theme::Rgb;
use crate::workspace::ToolIcon;

use super::Message as Parent;
use super::catalog::badges::{Badge, MAX_BADGES, Prepared, Style};

struct Symbol {
    id: &'static str,
    title: &'static str,
    icon: ToolIcon,
}

impl Symbol {
    const fn new(id: &'static str, title: &'static str, icon: ToolIcon) -> Self {
        Self { id, title, icon }
    }
}

const SYMBOLS: [Symbol; 30] = [
    Symbol::new("security", "Security", ToolIcon::Security),
    Symbol::new("logfile", "Log", ToolIcon::Logfile),
    Symbol::new("jobs", "Schedule", ToolIcon::Jobs),
    Symbol::new("server", "Server", ToolIcon::Server),
    Symbol::new("group", "Group", ToolIcon::Group),
    Symbol::new("terminal", "Terminal", ToolIcon::Shell),
    Symbol::new("sftp", "Transfer", ToolIcon::Sftp),
    Symbol::new("ai", "AI", ToolIcon::Ai),
    Symbol::new("gear", "Settings", ToolIcon::Gear),
    Symbol::new("privacy", "Private", ToolIcon::Privacy),
    Symbol::new("language", "Language", ToolIcon::Language),
    Symbol::new("translation", "Translate", ToolIcon::Translation),
    Symbol::new("themes", "Theme", ToolIcon::Themes),
    Symbol::new("appearance", "Layout", ToolIcon::Appearance),
    Symbol::new("window", "Window", ToolIcon::Window),
    Symbol::new("video", "Video", ToolIcon::Video),
    Symbol::new("backup", "Archive", ToolIcon::Backup),
    Symbol::new("export", "Share", ToolIcon::Export),
    Symbol::new("updates", "Update", ToolIcon::Updates),
    Symbol::new("resources", "Resources", ToolIcon::Resources),
    Symbol::new("editor", "Editor", ToolIcon::Editor),
    Symbol::new("snippetEditor", "Snippet", ToolIcon::Snippets),
    Symbol::new("add", "Add", ToolIcon::Add),
    Symbol::new("pin", "Pin", ToolIcon::Pin),
    Symbol::new("check", "Done", ToolIcon::Check),
    Symbol::new("warning", "Warning", ToolIcon::Warning),
    Symbol::new("star", "Important", ToolIcon::Star),
    Symbol::new("flag", "Flag", ToolIcon::Flag),
    Symbol::new("search", "Review", ToolIcon::Search),
    Symbol::new("clock", "Time", ToolIcon::Clock),
];

#[derive(Clone, Debug)]
pub enum Message {
    Toggle(usize, bool),
    Name(String),
    Style(Style),
    Color(String),
    Component(usize, u8),
    Symbol(usize),
    Create,
}

pub(super) struct Editor {
    pub(super) source: Arc<Prepared>,
    library: Vec<Badge>,
    original_library_len: usize,
    checked: BTreeSet<String>,
    original_checked: BTreeSet<String>,
    name: String,
    style: Style,
    color: String,
    rgb: Rgb,
    symbol: usize,
    problem: Option<String>,
    apply_problem: Option<String>,
}

impl Editor {
    pub(super) fn new(source: Arc<Prepared>) -> Self {
        let mut library = Badge::defaults().to_vec();
        for badge in source.library.iter().chain(&source.first) {
            if let Some(old) = library.iter_mut().find(|old| old.id == badge.id) {
                *old = badge.clone();
            } else {
                library.push(badge.clone());
            }
        }
        let checked: BTreeSet<_> = source.first.iter().map(|badge| badge.id.clone()).collect();
        Self {
            source,
            original_library_len: library.len(),
            library,
            original_checked: checked.clone(),
            checked,
            name: String::new(),
            style: Style::Soft,
            color: "#4488cc".into(),
            rgb: Rgb::from_hex(0x4488cc),
            symbol: 0,
            problem: None,
            apply_problem: None,
        }
    }

    pub(super) fn dirty(&self) -> bool {
        self.checked != self.original_checked
            || self.library.len() != self.original_library_len
            || !self.name.is_empty()
            || self.style != Style::Soft
            || Rgb::parse(&self.color) != Some(Rgb::from_hex(0x4488cc))
            || self.symbol != 0
    }

    fn can_create(&self) -> bool {
        let name = self.name.trim();
        !name.is_empty()
            && Rgb::parse(&self.color).is_some()
            && self.library.len() < MAX_BADGES
            && !self
                .library
                .iter()
                .any(|badge| badge.name.to_lowercase() == name.to_lowercase())
    }

    pub(super) fn can_apply(&self) -> bool {
        self.library.len() <= MAX_BADGES
    }

    pub(super) fn choice(&self) -> (Arc<Prepared>, Vec<Badge>, Vec<Badge>) {
        let selected = self
            .library
            .iter()
            .filter(|badge| self.checked.contains(&badge.id))
            .cloned()
            .collect();
        (Arc::clone(&self.source), self.library.clone(), selected)
    }

    pub(super) fn report_apply_failure(&mut self, problem: String) {
        self.apply_problem = Some(problem);
    }

    fn preview(&self, id: String, name: String) -> Badge {
        Badge {
            id,
            name,
            icon: SYMBOLS[self.symbol].id.into(),
            color: self.rgb.to_string(),
            style: self.style,
        }
    }

    pub(super) fn update(&mut self, message: Message) {
        match message {
            Message::Toggle(index, enabled) => {
                if let Some(badge) = self.library.get(index) {
                    if enabled {
                        self.checked.insert(badge.id.clone());
                    } else {
                        self.checked.remove(&badge.id);
                    }
                }
            }
            Message::Name(name)
                if name.chars().count() <= 40 && !name.chars().any(char::is_control) =>
            {
                self.name = name;
                self.problem = None;
            }
            Message::Style(style) => self.style = style,
            Message::Color(color) if color.len() <= 7 => {
                if let Some(rgb) = Rgb::parse(&color) {
                    self.rgb = rgb;
                }
                self.color = color;
            }
            Message::Component(index, value) => {
                let mut rgb = self.rgb.bytes();
                if let Some(component) = rgb.get_mut(index) {
                    *component = value;
                    self.rgb = Rgb::from_bytes(rgb);
                    self.color = self.rgb.to_string();
                }
            }
            Message::Symbol(index) if index < SYMBOLS.len() => self.symbol = index,
            Message::Create if self.can_create() => match badge_id() {
                Ok(id) => {
                    let badge = self.preview(id, self.name.trim().to_owned());
                    self.checked.insert(badge.id.clone());
                    self.library.push(badge);
                    self.name.clear();
                    self.problem = None;
                }
                Err(error) => self.problem = Some(format!("Could not create the badge: {error}")),
            },
            _ => {}
        }
    }

    pub(super) fn view(&self, foreground: Rgb) -> Element<'_, Parent> {
        let mut library = row![].spacing(8);
        for (index, badge) in self.library.iter().enumerate() {
            let checked = self.checked.contains(&badge.id);
            let toggle = row![
                checkbox(checked)
                    .on_toggle(move |value| Parent::Badge(Message::Toggle(index, value))),
                button(chip(badge, foreground))
                    .style(button::text)
                    .padding(0)
                    .on_press(Parent::Badge(Message::Toggle(index, !checked))),
            ]
            .spacing(4)
            .align_y(Alignment::Center);
            library = library.push(toggle);
        }
        let mut symbols = row![].spacing(4);
        for (index, symbol) in SYMBOLS.iter().enumerate() {
            let button = button(symbol.icon.widget(true, 20, Some(foreground)))
                .style(if self.symbol == index {
                    button::primary
                } else {
                    button::secondary
                })
                .padding(5)
                .on_press(Parent::Badge(Message::Symbol(index)));
            symbols = symbols.push(tooltip(button, symbol.title, tooltip::Position::Top));
        }
        let mut components = row![].spacing(8);
        for (index, (title, value)) in ["Red", "Green", "Blue"]
            .into_iter()
            .zip(self.rgb.bytes())
            .enumerate()
        {
            components = components.push(
                column![
                    text(title).size(12),
                    slider(0..=255, value, move |value| Parent::Badge(
                        Message::Component(index, value)
                    )),
                ]
                .width(Length::Fill)
                .spacing(3),
            );
        }

        let name = if self.name.trim().is_empty() {
            "Preview"
        } else {
            self.name.trim()
        };
        let preview = chip(&self.preview("preview".into(), name.to_owned()), foreground);
        let mut body = column![
            library.wrap(),
            rule::horizontal(1),
            text("Custom badge · 30 symbols"),
            text_input("Badge name (up to 40 characters)", &self.name)
                .on_input(|name| Parent::Badge(Message::Name(name))),
            row![
                pick_list(Style::ALL, Some(self.style), |style| Parent::Badge(
                    Message::Style(style)
                )),
                text_input("#RRGGBB", &self.color)
                    .on_input(|color| Parent::Badge(Message::Color(color)))
                    .width(110),
                swatch(self.rgb),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            components,
            symbols.wrap(),
            preview,
            button("Create and pin")
                .on_press_maybe(self.can_create().then_some(Parent::Badge(Message::Create))),
        ]
        .spacing(10);
        if let Some(problem) = &self.problem {
            body = body.push(text(problem).size(13));
        }
        let mut footer = column![].spacing(8);
        if let Some(problem) = &self.apply_problem {
            footer = footer.push(text(problem).size(13));
        }
        if self.source.held_count != 0 && !self.checked.contains("hold") {
            footer = footer.push(
                text(format!(
                    "Applying will remove Hold from {} selected logs.",
                    self.source.held_count
                ))
                .size(13),
            );
        }
        if !self.can_apply() {
            footer = footer.push(text("The badge library exceeds 256 entries.").size(13));
        }
        footer = footer.push(
            row![
                space::horizontal(),
                button("Cancel").on_press(Parent::Dismiss),
                button("Apply").on_press_maybe(self.can_apply().then_some(Parent::Confirm)),
            ]
            .spacing(8),
        );
        container(
            column![
                text("Badges").size(22),
                text(format!(
                    "Apply these badges to all {} selected logs",
                    self.source.selected_count
                )),
                scrollable(body).height(Length::Fill),
                footer,
            ]
            .spacing(12)
            .padding(16)
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .max_width(560)
        .style(container::bordered_box)
        .into()
    }
}

fn swatch(rgb: Rgb) -> Element<'static, Parent> {
    container(space::horizontal().width(24).height(20))
        .style(move |_| {
            container::Style::default()
                .background(rgb.color())
                .border(iced::border::rounded(4))
        })
        .into()
}

pub(super) fn chip(badge: &Badge, foreground: Rgb) -> Element<'static, Parent> {
    let accent = Rgb::parse(&badge.color)
        .unwrap_or(Rgb::from_hex(0x4488cc))
        .color();
    let color = if badge.style == Style::Solid {
        let linear = |component: f32| {
            if component <= 0.04045 {
                component / 12.92
            } else {
                ((component + 0.055) / 1.055).powf(2.4)
            }
        };
        let luminance =
            0.2126 * linear(accent.r) + 0.7152 * linear(accent.g) + 0.0722 * linear(accent.b);
        if luminance > 0.179 {
            Rgb::from_hex(0x111111)
        } else {
            Rgb::from_hex(0xffffff)
        }
    } else {
        foreground
    };
    let mut content = row![].align_y(Alignment::Center).spacing(5);
    if badge.style == Style::Flag {
        content = content.push(
            container(space::horizontal().width(3).height(18))
                .style(move |_| container::Style::default().background(accent)),
        );
    }
    if let Some(symbol) = SYMBOLS.iter().find(|symbol| symbol.id == badge.icon) {
        content = content.push(symbol.icon.widget(true, 16, Some(color)));
    }
    content = content.push(text(badge.name.clone()).color(color.color()).size(13));
    let style = badge.style;
    container(content)
        .padding([3, if style == Style::Minimal { 2 } else { 7 }])
        .style(move |_| {
            let border = iced::border::rounded(10);
            match style {
                Style::Soft => container::Style::default()
                    .background(accent.scale_alpha(0.20))
                    .border(border),
                Style::Outline => container::Style::default().border(Border {
                    color: accent,
                    width: 1.0,
                    ..border
                }),
                Style::Solid => container::Style::default()
                    .background(accent)
                    .border(border),
                Style::Flag | Style::Minimal => container::Style::default(),
            }
        })
        .into()
}

fn badge_id() -> Result<String, getrandom::Error> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let value = u128::from_be_bytes(bytes);
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        value >> 96,
        (value >> 80) as u16,
        (value >> 64) as u16,
        (value >> 48) as u16,
        value & 0xffff_ffff_ffff
    ))
}
