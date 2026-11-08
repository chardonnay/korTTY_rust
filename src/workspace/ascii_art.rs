use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    button, checkbox, column, container, mouse_area, pick_list, row, rule, scrollable, slider,
    space, text, text_editor, tooltip,
};
use iced::{Alignment, Element, Font, Length, Size, Task, window};
use serde::{Deserialize, Serialize};

use crate::profile::{self, SavedPosition, WindowPlacement};
use crate::sftp_window::{Directory, EntryKind, list_local};

use super::{Message as Desktop, Workspace, dialog_placement::Kind as DialogKind};

mod ai;
mod browser;
mod copy;
mod figlet;
mod picture;

use browser::Browser;
use copy::{Comment, Format, Output};
use figlet::Style;
use picture::{Adjustments, Grid, Picture};

pub(super) const TITLE: &str = "ASCII Art";
const MINIMUM: Size = Size::new(690.0, 440.0);
const DEFAULT_PLACEMENT: WindowPlacement = WindowPlacement {
    width: 780.0,
    height: 620.0,
    position: None,
};
const FONT_SIZE: u8 = 12;
const FONT_MIN: u8 = 6;
const FONT_MAX: u8 = 40;
const MAX_SOURCE_BYTES: usize = 4096;
const MAX_SOURCE_LINES: usize = 100;
const MAX_PREFERENCES: u64 = 4096;

#[derive(Clone, Debug)]
pub enum Message {
    Focus,
    Close,
    Escape,
    Tab(Tab),
    Ai(ai::Message),
    Style(Style),
    StepStyle(bool),
    Edit(text_editor::Action),
    Browse,
    Browser(browser::Message),
    ImageListed(u64, Result<Arc<Directory>, String>),
    ImageLoaded(u64, PathBuf, Result<Arc<Picture>, String>),
    PictureRendered(u64, Result<RenderedPicture, String>),
    ImageSize(picture::Size),
    Brightness(i16),
    Contrast(i16),
    Invert(bool),
    ResetAdjustments,
    HorizontalScroll,
    Zoom(i8),
    Comment(Comment),
    Output(Output),
    Gap(u8),
    Copy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tab {
    Banner,
    Ai,
    Image,
}

#[derive(Clone, Debug)]
pub struct RenderedPicture {
    image: Arc<Picture>,
    grid: Arc<Grid>,
    text: String,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(default)]
struct Preferences {
    // Older profiles stored this dialog's geometry alongside its display preferences.
    placement: WindowPlacement,
    zoom: u8,
    format: Format,
    ai_size: picture::Size,
    image_size: picture::Size,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            placement: DEFAULT_PLACEMENT,
            zoom: FONT_SIZE,
            format: Format::default(),
            ai_size: picture::Size::default(),
            image_size: picture::Size::default(),
        }
    }
}

impl Preferences {
    fn path() -> io::Result<PathBuf> {
        Ok(profile::root()?.join("ascii-art.json"))
    }

    fn load() -> Self {
        let Ok(file) = Self::path().and_then(|path| profile::open_preference(&path)) else {
            return Self::default();
        };
        if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
            return Self::default();
        }
        let mut bytes = Vec::new();
        if file
            .take(MAX_PREFERENCES + 1)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 > MAX_PREFERENCES
        {
            return Self::default();
        }
        let mut prefs = serde_json::from_slice::<Self>(&bytes).unwrap_or_default();
        if !WindowPlacement::valid_size(prefs.placement.width, prefs.placement.height) {
            prefs.placement = DEFAULT_PLACEMENT;
        }
        if let Some(position) = prefs.placement.position
            && (SavedPosition::new(position.x, position.y).is_none()
                || position
                    .scale_factor
                    .is_some_and(|scale| position.physical_pixels(scale).is_none()))
        {
            prefs.placement.position = None;
        }
        prefs.zoom = prefs.zoom.clamp(FONT_MIN, FONT_MAX);
        prefs.format.gap = prefs.format.gap.min(20);
        if prefs.format.output != Output::None {
            prefs.format.comment = Comment::None;
        }
        prefs
    }

    fn save(self) -> io::Result<()> {
        profile::save_atomic(&Self::path()?, &serde_json::to_vec(&self)?)
    }
}

pub(super) struct Dialog {
    owner: window::Id,
    tab: Tab,
    style: Style,
    source: text_editor::Content,
    banner: String,
    ai: ai::State,
    preferences: Preferences,
    adjustments: Adjustments,
    image: Option<Arc<Picture>>,
    image_name: Option<String>,
    image_dir: Option<PathBuf>,
    grid: Option<Arc<Grid>>,
    picture: String,
    picture_revision: u64,
    rendering: Option<u64>,
    browser: Option<Browser>,
    loading: Option<u64>,
    status: Option<String>,
    modifiers: Modifiers,
}

impl Dialog {
    pub(super) fn ai_active(&self) -> bool {
        self.ai.busy()
    }

    fn new(owner: window::Id, preferences: Preferences) -> Self {
        Self {
            owner,
            tab: Tab::Banner,
            style: Style::default(),
            source: text_editor::Content::new(),
            banner: String::new(),
            ai: ai::State::new(preferences.ai_size),
            preferences,
            adjustments: Adjustments::default(),
            image: None,
            image_name: None,
            image_dir: None,
            grid: None,
            picture: String::new(),
            picture_revision: 0,
            rendering: None,
            browser: None,
            loading: None,
            status: None,
            modifiers: Modifiers::empty(),
        }
    }

    fn preview(&self) -> &str {
        match self.tab {
            Tab::Banner => &self.banner,
            Tab::Ai => self.ai.text(),
            Tab::Image => &self.picture,
        }
    }

    fn can_copy(&self, ai_allowed: bool) -> bool {
        let available = match self.tab {
            Tab::Banner => true,
            Tab::Ai => ai_allowed && !self.ai.busy(),
            Tab::Image => self.rendering.is_none() && self.loading.is_none(),
        };
        available && !self.preview().trim().is_empty()
    }

    fn edit(&mut self, action: text_editor::Action) {
        if !action.is_edit() {
            self.source.perform(action);
            return;
        }
        let old = self.source.text();
        if !matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > MAX_SOURCE_BYTES)
        {
            self.source.perform(action);
            let source = self.source.text();
            if source.trim_end_matches('\n').len() <= MAX_SOURCE_BYTES
                && self.source.line_count() <= MAX_SOURCE_LINES
            {
                self.banner = figlet::render(self.style, &source);
                self.status = None;
                return;
            }
            self.source = text_editor::Content::with_text(&old);
        }
        self.status = Some("Banner text is limited to 4 KiB and 100 lines.".into());
    }

    fn request_picture(&mut self) -> Task<Message> {
        self.picture_revision = self.picture_revision.wrapping_add(1);
        self.render_picture()
    }

    fn render_picture(&mut self) -> Task<Message> {
        let Some(image) = self.image.clone().filter(|_| self.rendering.is_none()) else {
            return Task::none();
        };
        let revision = self.picture_revision;
        let size = self.preferences.image_size;
        let adjustments = self.adjustments;
        let cached = self.grid.clone().filter(|grid| grid.size == size);
        self.rendering = Some(revision);
        worker(
            move || {
                let grid = cached.unwrap_or_else(|| Arc::new(image.grid(size)));
                let text = grid.render(adjustments);
                Ok(RenderedPicture { image, grid, text })
            },
            move |result| Message::PictureRendered(revision, result),
        )
    }

    fn accept_picture(
        &mut self,
        revision: u64,
        result: Result<RenderedPicture, String>,
    ) -> Task<Message> {
        if self.rendering != Some(revision) {
            return Task::none();
        }
        self.rendering = None;
        match result {
            Ok(rendered) => {
                if self
                    .image
                    .as_ref()
                    .is_some_and(|image| Arc::ptr_eq(image, &rendered.image))
                {
                    self.grid = Some(rendered.grid);
                }
                if self.picture_revision == revision {
                    self.picture = rendered.text;
                }
            }
            Err(error) if self.picture_revision == revision => {
                self.picture.clear();
                self.status = Some(format!("Could not render image ({error})."));
            }
            Err(_) => {}
        }
        if self.picture_revision == revision {
            Task::none()
        } else {
            // Use the latest slider values after the one active conversion finishes.
            self.render_picture()
        }
    }

    fn view(&self, ai_allowed: bool) -> Element<'_, Message> {
        if let Some(browser) = &self.browser {
            return browser.view().map(Message::Browser);
        }
        let tabs = row![
            tab_button("Text Banner", Tab::Banner, self.tab),
            tab_button("AI Picture", Tab::Ai, self.tab),
            tab_button("Image File", Tab::Image, self.tab),
        ]
        .spacing(4);
        let body = match self.tab {
            Tab::Banner => self.banner_view(),
            Tab::Ai => self.ai.view(self.preview_view(), ai_allowed),
            Tab::Image => self.image_view(),
        };
        let format = self.preferences.format;
        let copy_controls = row![
            text("Copy as"),
            text("Comment").size(13),
            pick_list(Comment::ALL, Some(format.comment), Message::Comment).width(Length::Fill),
            text("Output code").size(13),
            pick_list(Output::ALL, Some(format.output), Message::Output).width(Length::Fill),
            text("Gap").size(13),
            button("−").on_press_maybe(
                (format.gap > 0).then_some(Message::Gap(format.gap.saturating_sub(1)))
            ),
            text(format.gap).width(22),
            button("+").on_press_maybe((format.gap < 20).then_some(Message::Gap(format.gap + 1))),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        let zoom = self.preferences.zoom;
        let copy = tooltip(
            button("Copy to Clipboard")
                .on_press_maybe(self.can_copy(ai_allowed).then_some(Message::Copy)),
            text(format.sample(if self.tab == Tab::Ai && !ai_allowed {
                ""
            } else {
                self.preview()
            }))
            .font(Font::MONOSPACE),
            tooltip::Position::Top,
        );
        let bottom = row![
            text("Zoom"),
            button("−").on_press_maybe((zoom > FONT_MIN).then_some(Message::Zoom(-1))),
            text(format!("{} %", (u16::from(zoom) * 100 + 6) / 12)).width(46),
            button("+").on_press_maybe((zoom < FONT_MAX).then_some(Message::Zoom(1))),
            button("⟲").on_press(Message::Zoom(0)),
            space::horizontal(),
            copy,
            button("Close").on_press(Message::Close),
        ]
        .spacing(7)
        .align_y(Alignment::Center);
        let mut content = column![tabs, rule::horizontal(1), body];
        if let Some(status) = &self.status {
            content = content.push(text(status).size(13));
        }
        content
            .push(copy_controls)
            .push(bottom)
            .height(Length::Fill)
            .padding(14)
            .spacing(10)
            .into()
    }

    fn banner_view(&self) -> Element<'_, Message> {
        let field = text_editor(&self.source)
            .placeholder("Enter text to turn into a banner")
            .on_action(Message::Edit)
            .height(74)
            .key_binding(|event| {
                key_message(&event.key, event.modifiers)
                    .map(text_editor::Binding::Custom)
                    .or_else(|| text_editor::Binding::from_key_press(event))
            });
        let style = row![
            label("Style"),
            button("◀").on_press(Message::StepStyle(false)),
            pick_list(Style::ALL, Some(self.style), Message::Style).width(190),
            button("▶").on_press(Message::StepStyle(true)),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        column![
            style,
            row![label("Text"), field].spacing(6),
            row![label("Preview"), self.preview_view()]
                .spacing(6)
                .height(Length::Fill),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn image_view(&self) -> Element<'_, Message> {
        let loaded = self.image.is_some();
        let adjustments = self.adjustments;
        let source = self.image_name.as_deref().unwrap_or("No image selected");
        let controls = row![
            button("Open image…").on_press_maybe(self.loading.is_none().then_some(Message::Browse)),
            text(source).width(Length::Fill),
            text("Size"),
            pick_list(
                picture::Size::ALL,
                Some(self.preferences.image_size),
                Message::ImageSize
            ),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let adjustments_row = row![
            text("Brightness").size(13),
            slider(-100..=100, adjustments.brightness, move |value| {
                Message::Brightness(if loaded { value } else { 0 })
            })
            .width(Length::Fill),
            text("Contrast").size(13),
            slider(-100..=100, adjustments.contrast, move |value| {
                Message::Contrast(if loaded { value } else { 0 })
            })
            .width(Length::Fill),
            checkbox(adjustments.invert)
                .label("Invert")
                .on_toggle_maybe(loaded.then_some(Message::Invert)),
            button("Reset").on_press_maybe(loaded.then_some(Message::ResetAdjustments)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let note = if self.loading.is_some() {
            "Loading image…".into()
        } else if self.rendering.is_some() {
            "Rendering image…".into()
        } else if let Some(image) = &self.image {
            let (width, height) = image.source;
            let rows = self.picture.lines().count();
            let columns = self.picture.lines().map(str::len).max().unwrap_or_default();
            format!("{width} × {height} pixels · {columns} × {rows} characters")
        } else {
            "Choose or drop a PNG, JPEG, GIF, BMP or WBMP image.".into()
        };
        column![
            controls,
            adjustments_row,
            self.preview_view(),
            text(note).size(13),
        ]
        .spacing(10)
        .height(Length::Fill)
        .into()
    }

    fn preview_view(&self) -> Element<'_, Message> {
        let preview = container(
            text(self.preview())
                .font(Font::MONOSPACE)
                .size(f32::from(self.preferences.zoom))
                .wrapping(iced::widget::text::Wrapping::None),
        )
        .padding(7)
        .width(Length::Fill)
        .height(Length::Fill);
        let mut area = mouse_area(preview);
        if zoom_modifiers(self.modifiers) {
            area = area.on_scroll(|delta| {
                let y = match delta {
                    iced::mouse::ScrollDelta::Lines { y, .. }
                    | iced::mouse::ScrollDelta::Pixels { y, .. } => y,
                };
                if y == 0.0 {
                    Message::HorizontalScroll
                } else {
                    Message::Zoom(if y > 0.0 { 1 } else { -1 })
                }
            });
        }
        container(
            scrollable(area)
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .style(container::bordered_box)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn save(&self) {
        if let Err(error) = self.preferences.save() {
            eprintln!("Could not save ASCII Art preferences: {error}");
        }
    }
}

fn label(title: &'static str) -> iced::widget::Text<'static> {
    text(title).width(66)
}

fn tab_button(
    title: &'static str,
    tab: Tab,
    current: Tab,
) -> iced::widget::Button<'static, Message> {
    button(title)
        .style(if tab == current {
            button::primary
        } else {
            button::secondary
        })
        .on_press(Message::Tab(tab))
}

fn zoom_modifiers(modifiers: Modifiers) -> bool {
    modifiers.command() && !modifiers.alt()
}

fn key_message(key: &Key, modifiers: Modifiers) -> Option<Message> {
    match key {
        Key::Named(Named::Escape) if modifiers.is_empty() => Some(Message::Escape),
        Key::Character(value) if zoom_modifiers(modifiers) => match value.as_str() {
            "+" | "=" => Some(Message::Zoom(1)),
            "-" => Some(Message::Zoom(-1)),
            "0" => Some(Message::Zoom(0)),
            _ => None,
        },
        _ => None,
    }
}

fn worker<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    ready: impl Fn(Result<T, String>) -> Message + Send + 'static,
) -> Task<Message> {
    Task::perform(
        async move {
            let (sender, receiver) = async_channel::bounded(1);
            std::thread::Builder::new()
                .name("kortty-ascii-art".into())
                .spawn(move || {
                    let _ = sender.send_blocking(work());
                })
                .map_err(|error| error.to_string())?;
            receiver
                .recv()
                .await
                .map_err(|_| String::from("The image worker stopped."))?
        },
        ready,
    )
}

impl Workspace {
    pub(super) fn open_ascii_art(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(id) = self.ascii_art_for_owner(owner) {
            return window::gain_focus(id);
        }
        let preferences = Preferences::load();
        let (id, open) = self.open_placed_dialog_with_fallback(
            DialogKind::AsciiArt,
            window::Settings {
                size: Size::new(DEFAULT_PLACEMENT.width, DEFAULT_PLACEMENT.height),
                min_size: Some(MINIMUM),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            Some(preferences.placement),
        );
        let mut dialog = Dialog::new(owner, preferences);
        dialog.ai.configure(self.ai_configuration());
        self.ascii_art.insert(id, dialog);
        open.map(|_| Desktop::Noop)
    }

    pub(super) fn ascii_art_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.ascii_art
            .iter()
            .find_map(|(&id, dialog)| (dialog.owner == owner).then_some(id))
    }

    pub(super) fn close_ascii_art(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let dialog = self.ascii_art.remove(&id)?;
        dialog.save();
        Some(
            self.close_placed_dialog(id)
                .chain(window::gain_focus(dialog.owner)),
        )
    }

    pub(super) fn configure_ascii_ai(&mut self) {
        let configuration = self.ai_configuration();
        for dialog in self.ascii_art.values_mut() {
            dialog.ai.configure(configuration.clone());
        }
    }

    pub(super) fn save_ascii_art_on_quit(&mut self) {
        let most_recent = self
            .ascii_art
            .iter()
            .max_by_key(|(id, _)| **id)
            .map(|(_, dialog)| dialog);
        if let Some(dialog) = most_recent {
            dialog.save();
        }
        for dialog in self.ascii_art.values_mut() {
            dialog.ai.cancel();
        }
    }

    pub(super) fn ascii_art_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        self.ascii_art.get(&id).map(|dialog| {
            dialog
                .view(self.ai_features_enabled())
                .map(move |message| Desktop::AsciiArt(id, message))
        })
    }

    pub(super) fn ascii_art_modifiers(&mut self, id: window::Id, modifiers: Modifiers) {
        if let Some(dialog) = self.ascii_art.get_mut(&id) {
            dialog.modifiers = modifiers;
        }
    }

    pub(super) fn ascii_art_key(
        &mut self,
        id: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        self.ascii_art.contains_key(&id).then_some(())?;
        key_message(key, modifiers).map(|message| self.update_ascii_art(id, message))
    }

    pub(super) fn drop_ascii_art_image(&mut self, id: window::Id, path: PathBuf) -> Task<Desktop> {
        if !self
            .ascii_art
            .get(&id)
            .is_some_and(|dialog| dialog.tab == Tab::Image && dialog.browser.is_none())
        {
            return Task::none();
        }
        self.load_ascii_art_image(id, path)
    }

    pub(super) fn update_ascii_art(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        let ai_allowed = self.ai_features_enabled();
        let Some(dialog) = self.ascii_art.get_mut(&id) else {
            return Task::none();
        };
        match message {
            Message::Focus => return window::gain_focus(id),
            Message::Escape if dialog.browser.is_some() => dialog.browser = None,
            Message::Escape if dialog.tab == Tab::Ai && dialog.ai.busy() => dialog.ai.cancel(),
            Message::Close | Message::Escape => return self.close_window(id),
            Message::Tab(tab) => {
                dialog.tab = tab;
                dialog.status = None;
            }
            Message::Ai(message) => {
                let task = dialog.ai.update(message, ai_allowed);
                dialog.preferences.ai_size = dialog.ai.size();
                return task.map(move |message| Desktop::AsciiArt(id, Message::Ai(message)));
            }
            Message::Style(style) => {
                dialog.style = style;
                dialog.banner = figlet::render(style, &dialog.source.text());
            }
            Message::StepStyle(forward) => {
                let index = Style::ALL
                    .iter()
                    .position(|style| *style == dialog.style)
                    .unwrap_or_default();
                let offset = if forward { 1 } else { Style::ALL.len() - 1 };
                let style = Style::ALL[(index + offset) % Style::ALL.len()];
                return self.update_ascii_art(id, Message::Style(style));
            }
            Message::Edit(action) => dialog.edit(action),
            Message::Browse if dialog.tab == Tab::Image && dialog.loading.is_none() => {
                if let Some(folder) = dialog.image_dir.clone().or_else(std::env::home_dir) {
                    return self.list_ascii_art_folder(id, folder);
                }
                dialog.status =
                    Some("The home directory is unavailable. Drop an image onto this tab.".into());
            }
            Message::Browser(message) => return self.update_ascii_art_browser(id, message),
            Message::ImageListed(token, result) => {
                if let Some(browser) = &mut dialog.browser {
                    browser.listed(token, result);
                }
            }
            Message::ImageLoaded(token, path, result) if dialog.loading == Some(token) => {
                dialog.loading = None;
                match result {
                    Ok(image) => {
                        dialog.image_name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned());
                        dialog.image_dir = path.parent().map(Path::to_owned);
                        dialog.image = Some(image);
                        dialog.grid = None;
                        dialog.picture.clear();
                        dialog.status = None;
                        return dialog
                            .request_picture()
                            .map(move |message| Desktop::AsciiArt(id, message));
                    }
                    Err(error) => dialog.status = Some(format!("Could not open image ({error}).")),
                }
            }
            Message::PictureRendered(revision, result) => {
                return dialog
                    .accept_picture(revision, result)
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::ImageSize(size) => {
                dialog.preferences.image_size = size;
                return dialog
                    .request_picture()
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::Brightness(brightness) if dialog.image.is_some() => {
                dialog.adjustments.brightness = brightness;
                return dialog
                    .request_picture()
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::Contrast(contrast) if dialog.image.is_some() => {
                dialog.adjustments.contrast = contrast;
                return dialog
                    .request_picture()
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::Invert(invert) if dialog.image.is_some() => {
                dialog.adjustments.invert = invert;
                return dialog
                    .request_picture()
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::ResetAdjustments => {
                dialog.adjustments = Adjustments::default();
                return dialog
                    .request_picture()
                    .map(move |message| Desktop::AsciiArt(id, message));
            }
            Message::Zoom(0) => dialog.preferences.zoom = FONT_SIZE,
            Message::Zoom(step) => {
                dialog.preferences.zoom = dialog
                    .preferences
                    .zoom
                    .saturating_add_signed(step)
                    .clamp(FONT_MIN, FONT_MAX);
            }
            Message::Comment(style) => dialog.preferences.format.comment(style),
            Message::Output(style) => dialog.preferences.format.output(style),
            Message::Gap(gap) => dialog.preferences.format.gap = gap.min(20),
            Message::Copy if dialog.can_copy(ai_allowed) => {
                let value = dialog.preferences.format.format(dialog.preview());
                if !value.is_empty() {
                    return self.write_clipboard(value);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn update_ascii_art_browser(
        &mut self,
        id: window::Id,
        message: browser::Message,
    ) -> Task<Desktop> {
        let Some(dialog) = self.ascii_art.get_mut(&id) else {
            return Task::none();
        };
        let Some(browser) = dialog.browser.as_mut() else {
            return Task::none();
        };
        use browser::Message::*;
        match message {
            Back => dialog.browser = None,
            Parent => {
                if let Some(folder) = browser.folder.parent().map(Path::to_owned) {
                    return self.list_ascii_art_folder(id, folder);
                }
            }
            Page(forward) => browser.step(forward),
            PathChanged(value) => browser.set_path(value),
            OpenPath => match browser.typed_path() {
                Some(path) => return self.load_ascii_art_image(id, path),
                None => browser.problem = Some("Enter an absolute path to an image."),
            },
            Select(token, index) => {
                if let Some((path, kind)) = browser.selection(token, index) {
                    match kind {
                        EntryKind::Directory => return self.list_ascii_art_folder(id, path),
                        EntryKind::File => return self.load_ascii_art_image(id, path),
                        _ => {}
                    }
                }
            }
        }
        Task::none()
    }

    fn list_ascii_art_folder(&mut self, id: window::Id, folder: PathBuf) -> Task<Desktop> {
        let token = self.allocate();
        let Some(dialog) = self.ascii_art.get_mut(&id) else {
            return Task::none();
        };
        dialog.browser = Some(Browser::new(token, folder.clone()));
        worker(
            move || {
                list_local(&folder)
                    .map(Arc::new)
                    .map_err(|error| error.to_string())
            },
            move |result| Message::ImageListed(token, result),
        )
        .map(move |message| Desktop::AsciiArt(id, message))
    }

    fn load_ascii_art_image(&mut self, id: window::Id, path: PathBuf) -> Task<Desktop> {
        let token = self.allocate();
        let Some(dialog) = self
            .ascii_art
            .get_mut(&id)
            .filter(|dialog| dialog.loading.is_none())
        else {
            return Task::none();
        };
        dialog.browser = None;
        dialog.loading = Some(token);
        dialog.status = None;
        let file = path.clone();
        worker(
            move || Picture::load(&file).map(Arc::new),
            move |result| Message::ImageLoaded(token, path.clone(), result),
        )
        .map(move |message| Desktop::AsciiArt(id, message))
    }
}
