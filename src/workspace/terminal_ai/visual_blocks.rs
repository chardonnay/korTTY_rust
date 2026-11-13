//! Bounded native previews of static content from completed chat answers.

use std::cell::RefCell;
use std::io::Cursor;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Weak};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use iced::widget::{Column, button, column, container, image, row, text};
use iced::{ContentFit, Element, Font, Length, Task, window};
use resvg::{tiny_skia, usvg};

use super::{Chat, ClipboardPolicy, Desktop, Message, Turn, Workspace, code_blocks};
use crate::workspace::clipboard_access::publish_png;

mod math;
mod svg;

const MAX_WIDTH: u32 = 560;
const MAX_HEIGHT: u32 = 320;
const MAX_PREVIEWS: usize = 6;
const MAX_PREVIEWS_PER_TURN: usize = 2;
pub(super) const MAX_INLINE_SOURCES: usize = 8;
const MAX_RASTER_EDGE: u32 = 1024;
const FONT_FAMILY: &str = "Noto Sans Mono";

#[derive(Clone, Copy, Eq, PartialEq)]
enum Format {
    Svg,
    Mermaid,
    Raster,
    Math,
}

#[derive(Clone, Eq, PartialEq)]
pub(super) struct Source {
    format: Format,
    body: Range<usize>,
    indent: usize,
    inline: Option<Range<usize>>,
}

impl Source {
    pub(super) fn fenced(
        language: &str,
        answer: &str,
        body: Range<usize>,
        indent: usize,
    ) -> Option<Self> {
        let source = answer[body.clone()].trim_start();
        let format = if language.eq_ignore_ascii_case("mermaid") {
            Format::Mermaid
        } else if math::is_fenced(language, source) {
            Format::Math
        } else if language.eq_ignore_ascii_case("svg")
            || ((language.is_empty()
                || ["xml", "html"]
                    .iter()
                    .any(|tag| language.eq_ignore_ascii_case(tag)))
                && svg::has_svg_root(source))
        {
            Format::Svg
        } else if raster_format(source).is_some() {
            Format::Raster
        } else {
            return None;
        };
        Some(Self {
            format,
            body,
            indent,
            inline: None,
        })
    }

    fn content(&self, answer: &str) -> String {
        code_blocks::dedent(&answer[self.body.clone()], self.indent)
    }

    fn render(&self, answer: &str) -> Result<Preview, &'static str> {
        let source = self.content(answer);
        match self.format {
            Format::Svg => svg::render(&source),
            Format::Mermaid => mermaid(&source),
            Format::Raster => raster(&source),
            Format::Math => math::render(&source),
        }
    }
}

pub(super) fn inline_sources(value: &str, offset: usize) -> impl Iterator<Item = Source> + '_ {
    static INLINE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"!\[[^\]\r\n]*\]\(\s*(?<image>data:image/(?:png|jpe?g|gif|bmp);base64,[A-Za-z0-9+/=\s]+)\)|\$\$(?<math>(?s:.+?))\$\$",
        )
        .expect("static inline visual syntax")
    });
    let range = move |value: regex::Match<'_>| offset + value.start()..offset + value.end();
    let input = if value.contains("data:image/") || value.contains("$$") {
        value
    } else {
        ""
    };
    INLINE.captures_iter(input).filter_map(move |captures| {
        let (format, body) = if let Some(body) = captures.name("image") {
            (Format::Raster, body)
        } else {
            let body = captures.name("math")?;
            if body.as_str().trim().is_empty() {
                return None;
            }
            (Format::Math, body)
        };
        Some(Source {
            format,
            body: range(body),
            indent: 0,
            inline: Some(range(captures.get(0).expect("whole visual match"))),
        })
    })
}

#[derive(Clone)]
pub enum Event {
    ShowSource(Selection),
    CopySource(Selection),
    CopyImage(Selection),
    ImagePrepared(u64, Result<PreparedImage, String>),
    ImageCopied(u64, Result<(), String>),
}

#[derive(Clone)]
pub enum PreparedImage {
    Internal(Arc<[u8]>),
    Native(kortty_windowing::ClipboardPng),
}

#[derive(Clone)]
pub struct Selection {
    turn: Weak<Turn>,
    source: Source,
}

impl Selection {
    fn new(turn: &Arc<Turn>, source: Source) -> Self {
        Self {
            turn: Arc::downgrade(turn),
            source,
        }
    }

    fn current_turn(&self, chat: &Chat) -> Option<Arc<Turn>> {
        let turn = self.turn.upgrade()?;
        chat.turns
            .iter()
            .any(|candidate| Arc::ptr_eq(candidate, &turn))
            .then_some(turn)
    }

    fn matches(&self, other: &Self) -> bool {
        Weak::ptr_eq(&self.turn, &other.turn) && self.source == other.source
    }
}

#[derive(Default)]
pub(super) struct State(RefCell<Cache>);

#[derive(Default)]
struct Cache {
    turns: Vec<Weak<Turn>>,
    entries: Vec<Entry>,
    copy: Option<PendingCopy>,
}

struct PendingCopy {
    ticket: u64,
    selection: Selection,
    live: Arc<()>,
}

struct Entry {
    selection: Selection,
    preview: Result<Preview, &'static str>,
    source_visible: bool,
}

impl State {
    pub(super) fn cancel_copy(&mut self) {
        self.0.get_mut().copy = None;
    }

    pub(super) fn prepare(&self, turns: &[Arc<Turn>]) {
        let mut cache = self.0.borrow_mut();
        if cache.turns.len() == turns.len()
            && cache
                .turns
                .iter()
                .zip(turns)
                .all(|(cached, current)| cached.as_ptr() == Arc::as_ptr(current))
        {
            return;
        }
        cache.turns = turns.iter().map(Arc::downgrade).collect();
        let existing = &mut cache.entries;
        let mut wanted = Vec::with_capacity(MAX_PREVIEWS);
        for turn in turns.iter().rev() {
            let count = MAX_PREVIEWS_PER_TURN.min(MAX_PREVIEWS - wanted.len());
            for source in code_blocks::visual_candidates(turn.answer(), count) {
                let selection = Selection::new(turn, source);
                let entry = existing
                    .iter()
                    .position(|entry| entry.selection.matches(&selection));
                wanted.push(match entry {
                    Some(index) => existing.swap_remove(index),
                    None => Entry {
                        preview: selection.source.render(turn.answer()),
                        selection,
                        source_visible: false,
                    },
                });
            }
            if wanted.len() == MAX_PREVIEWS {
                break;
            }
        }
        *existing = wanted;
        if cache.copy.as_ref().is_some_and(|pending| {
            !cache
                .entries
                .iter()
                .any(|entry| entry.selection.matches(&pending.selection) && entry.preview.is_ok())
        }) {
            cache.copy = None;
        }
    }

    pub(super) fn view<'a>(
        &self,
        turn: &'a Arc<Turn>,
        source: Source,
        font_size: f32,
    ) -> Element<'a, Message> {
        let selection = Selection::new(turn, source);
        let cache = self.0.borrow();
        let entry = cache
            .entries
            .iter()
            .find(|entry| entry.selection.matches(&selection));
        let preview = entry.map_or(Err("Preview limit reached."), |entry| {
            entry.preview.as_ref().map_err(|error| *error)
        });
        let show_source = entry.is_some_and(|entry| entry.source_visible);
        let body = match preview {
            Ok(preview) if !show_source => preview.view(),
            _ => {
                let source = &selection.source;
                let content = source.inline.as_ref().map_or_else(
                    || source.content(turn.answer()),
                    |range| turn.answer()[range.clone()].to_owned(),
                );
                text(content)
                    .font(Font::MONOSPACE)
                    .size(font_size)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .into()
            }
        };
        let controls = row![
            preview.is_ok().then(|| button(if show_source {
                "Show preview"
            } else {
                "Show source"
            })
            .on_press(Message::Visual(Event::ShowSource(selection.clone())))),
            button("Copy source").on_press(Message::Visual(Event::CopySource(selection.clone()))),
            (preview.is_ok() && selection.source.format == Format::Raster).then(|| {
                button("Copy image").on_press_maybe(
                    cache
                        .copy
                        .is_none()
                        .then_some(Message::Visual(Event::CopyImage(selection))),
                )
            }),
        ]
        .spacing(8);
        container(
            column![
                body,
                preview.err().map(|reason| text(reason).size(13)),
                controls
            ]
            .spacing(6),
        )
        .padding(8)
        .width(Length::Fill)
        .style(container::bordered_box)
        .into()
    }

    pub(super) fn append_text<'a>(
        &self,
        mut column: Column<'a, Message>,
        turn: &'a Arc<Turn>,
        range: Range<usize>,
        font_size: f32,
        remaining: &mut usize,
    ) -> Column<'a, Message> {
        let answer = turn.answer();
        let mut previous = range.start;
        for source in inline_sources(&answer[range.clone()], range.start).take(*remaining) {
            *remaining -= 1;
            let inline = source.inline.as_ref().expect("inline source range");
            if previous < inline.start {
                column = column.push(text(&answer[previous..inline.start]).size(font_size));
            }
            previous = inline.end;
            column = column.push(self.view(turn, source, font_size));
        }
        if previous < range.end {
            column = column.push(text(&answer[previous..range.end]).size(font_size));
        }
        column
    }
}

impl Workspace {
    pub(super) fn update_chat_visual(&mut self, id: window::Id, event: Event) -> Task<Desktop> {
        let event = match event {
            Event::ImagePrepared(ticket, result) => {
                return self.copy_chat_image_prepared(id, ticket, result);
            }
            Event::ImageCopied(ticket, result) => {
                return self.copy_chat_image_finished(id, ticket, result);
            }
            event => event,
        };
        let selection = match &event {
            Event::ShowSource(selection)
            | Event::CopySource(selection)
            | Event::CopyImage(selection) => selection,
            _ => unreachable!("completion was dispatched"),
        };
        let Some(chat) = self
            .terminal_chats
            .get(&id)
            .filter(|chat| self.chat_visual_allowed(id, chat))
        else {
            return Task::none();
        };
        let Some(turn) = selection.current_turn(chat) else {
            return Task::none();
        };
        match event {
            Event::ShowSource(selection) => {
                if let Some(entry) = chat
                    .visuals
                    .0
                    .borrow_mut()
                    .entries
                    .iter_mut()
                    .find(|entry| entry.selection.matches(&selection) && entry.preview.is_ok())
                {
                    entry.source_visible = !entry.source_visible;
                }
                Task::none()
            }
            Event::CopySource(selection) => {
                self.write_clipboard(selection.source.content(turn.answer()))
            }
            Event::CopyImage(selection) => self.copy_chat_image(id, selection, turn),
            _ => unreachable!("completion was dispatched"),
        }
    }

    fn chat_visual_allowed(&self, id: window::Id, chat: &Chat) -> bool {
        self.ai_chat_enabled()
            && !self.exiting
            && !self.hosted_close_locks(id)
            && !self.private_agent_source_changed(chat)
            && chat.code_actions_available()
            && chat.code_review.is_none()
    }

    fn copy_chat_image(
        &mut self,
        id: window::Id,
        selection: Selection,
        turn: Arc<Turn>,
    ) -> Task<Desktop> {
        let ticket = self.allocate();
        let chat = &self.terminal_chats[&id];
        let mut cache = chat.visuals.0.borrow_mut();
        if selection.source.format != Format::Raster
            || cache.copy.is_some()
            || !cache
                .entries
                .iter()
                .any(|entry| entry.selection.matches(&selection) && entry.preview.is_ok())
        {
            return Task::none();
        }
        let source = selection.source.content(turn.answer());
        let internal = self.clipboard.policy.internal;
        cache.copy = Some(PendingCopy {
            ticket,
            selection,
            live: Arc::new(()),
        });
        Task::future(async move {
            let (send, receive) = async_channel::bounded(1);
            let worker = std::thread::Builder::new()
                .name("kortty-chat-image".into())
                .spawn(move || {
                    let _ = send.try_send(prepare_image(&source, internal));
                });
            let result = match worker {
                Ok(_) => receive
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The image reader stopped.".into())),
                Err(error) => Err(error.to_string()),
            };
            Desktop::TerminalAi(id, Message::Visual(Event::ImagePrepared(ticket, result)))
        })
    }

    fn copy_chat_image_prepared(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<PreparedImage, String>,
    ) -> Task<Desktop> {
        let Some(chat) = self.terminal_chats.get(&id) else {
            return Task::none();
        };
        let (live, current) = {
            let cache = chat.visuals.0.borrow();
            let Some(pending) = cache
                .copy
                .as_ref()
                .filter(|pending| pending.ticket == ticket)
            else {
                return Task::none();
            };
            (
                Arc::downgrade(&pending.live),
                pending.selection.current_turn(chat).is_some(),
            )
        };
        if !current
            || !self.chat_visual_allowed(id, chat)
            || !installed_clipboard_mode(self.clipboard.policy.internal)
        {
            chat.visuals.0.borrow_mut().copy = None;
            return Task::none();
        }
        match result {
            Ok(PreparedImage::Internal(png)) if self.clipboard.policy.internal => {
                self.clipboard.buffer.set_png(png);
                self.copy_chat_image_finished(id, ticket, Ok(()))
            }
            Ok(PreparedImage::Native(png)) if !self.clipboard.policy.internal => publish_png(
                self.hosted_tool_owner(id).unwrap_or(id),
                png,
                move || {
                    if live.upgrade().is_none() || !installed_clipboard_mode(false) {
                        return Err("The image copy is no longer authorized.".into());
                    }
                    Ok(())
                },
                move |result| {
                    Desktop::TerminalAi(id, Message::Visual(Event::ImageCopied(ticket, result)))
                },
            ),
            Ok(_) => {
                self.copy_chat_image_finished(id, ticket, Err("The clipboard mode changed.".into()))
            }
            Err(error) => self.copy_chat_image_finished(id, ticket, Err(error)),
        }
    }

    fn copy_chat_image_finished(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<(), String>,
    ) -> Task<Desktop> {
        if let Some(chat) = self.terminal_chats.get_mut(&id) {
            let cache = chat.visuals.0.get_mut();
            if cache
                .copy
                .as_ref()
                .is_some_and(|pending| pending.ticket == ticket)
            {
                cache.copy = None;
                chat.notice = match result {
                    Ok(()) => "Image copied.".into(),
                    Err(error) => format!("Could not copy the image ({error})."),
                };
            }
        }
        Task::none()
    }
}

fn installed_clipboard_mode(internal: bool) -> bool {
    let policy = ClipboardPolicy::load();
    policy.problem.is_none()
        && policy.ai_allowed
        && policy.ai_chat_allowed
        && policy.internal == internal
}

struct Preview {
    handle: image::Handle,
    height: u32,
}

impl Preview {
    fn new(width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            handle: image::Handle::from_rgba(width, height, pixels),
            height,
        }
    }

    fn view(&self) -> Element<'static, Message> {
        image(self.handle.clone())
            .width(Length::Fill)
            .height(self.height as f32)
            .content_fit(ContentFit::ScaleDown)
            .into()
    }
}

fn mermaid(source: &str) -> Result<Preview, &'static str> {
    let diagram =
        kortty_diagram::parse_mermaid(source).map_err(|_| "Unsupported Mermaid syntax.")?;
    if diagram.nodes().is_empty() || diagram.nodes().len() > 80 || diagram.edges().len() > 96 {
        return Err("The Mermaid diagram exceeds the inline preview limits.");
    }
    let scene = diagram.layout();
    let svg = scene.to_svg_with_font_family(kortty_diagram::Theme::Light, FONT_FAMILY);
    let document =
        roxmltree::Document::parse(&svg).map_err(|_| "Mermaid preview could not be rendered.")?;
    rasterize(&document)
}

fn embedded_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
        let mut fonts = usvg::fontdb::Database::new();
        fonts.load_font_data(
            include_bytes!("../../session_journal/export/pdf/fonts/NotoSansMono-Regular.ttf")
                .to_vec(),
        );
        fonts.load_font_data(
            include_bytes!("../../session_journal/export/pdf/fonts/NotoSans-Regular.ttf").to_vec(),
        );
        fonts.load_font_data(
            include_bytes!("../../session_journal/export/pdf/fonts/NotoSans-Bold.ttf").to_vec(),
        );
        fonts.set_monospace_family(FONT_FAMILY);
        fonts.set_sans_serif_family("Noto Sans");
        fonts.set_serif_family("Noto Sans");
        Arc::new(fonts)
    });
    Arc::clone(&FONTS)
}

fn rasterize(document: &roxmltree::Document<'_>) -> Result<Preview, &'static str> {
    let options = usvg::Options {
        resources_dir: None,
        font_family: "Noto Sans".into(),
        fontdb: embedded_fonts(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_xmltree(document, &options)
        .map_err(|_| "The preview could not be rendered.")?;
    let size = tree.size();
    if [size.width(), size.height()]
        .iter()
        .any(|side| !(0.01..=65_536.0).contains(side))
    {
        return Err("The preview viewport exceeds its size limit.");
    }
    let scale = (MAX_WIDTH as f32 / size.width())
        .min(MAX_HEIGHT as f32 / size.height())
        .min(1.0);
    let width = (size.width() * scale).ceil().clamp(1.0, MAX_WIDTH as f32) as u32;
    let height = (size.height() * scale).ceil().clamp(1.0, MAX_HEIGHT as f32) as u32;
    let mut surface =
        tiny_skia::Pixmap::new(width, height).ok_or("The preview could not be allocated.")?;
    surface.fill(tiny_skia::Color::WHITE);
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut surface.as_mut(),
    );
    Ok(Preview::new(width, height, surface.take()))
}

fn raster_format(source: &str) -> Option<(::image::ImageFormat, &str)> {
    let (mime, data) = source
        .trim()
        .strip_prefix("data:image/")?
        .split_once(";base64,")?;
    let format = match mime {
        "png" => ::image::ImageFormat::Png,
        "jpeg" | "jpg" => ::image::ImageFormat::Jpeg,
        "gif" => ::image::ImageFormat::Gif,
        "bmp" => ::image::ImageFormat::Bmp,
        _ => return None,
    };
    Some((format, data))
}

fn decode_raster(source: &str) -> Result<::image::RgbaImage, &'static str> {
    use ::image::ImageDecoder as _;

    let invalid = "The embedded raster image could not be decoded.";
    if source.len() > super::MAX_ANSWER_BYTES {
        return Err("The embedded raster image exceeds the chat response limit.");
    }
    let (format, encoded) = raster_format(source).ok_or(invalid)?;
    let encoded: Vec<_> = encoded
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let bytes = STANDARD.decode(encoded).map_err(|_| invalid)?;
    if ::image::guess_format(&bytes).ok() != Some(format) {
        return Err(invalid);
    }
    let mut reader = ::image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = ::image::Limits::default();
    limits.max_image_width = Some(MAX_RASTER_EDGE);
    limits.max_image_height = Some(MAX_RASTER_EDGE);
    limits.max_alloc = Some(u64::from(MAX_RASTER_EDGE).pow(2) * 8);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|_| invalid)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 || width > MAX_RASTER_EDGE || height > MAX_RASTER_EDGE {
        return Err("The embedded raster image exceeds its dimensions limit.");
    }
    limits.reserve(decoder.total_bytes()).map_err(|_| invalid)?;
    decoder.set_limits(limits).map_err(|_| invalid)?;
    ::image::DynamicImage::from_decoder(decoder)
        .map(|image| image.into_rgba8())
        .map_err(|_| invalid)
}

fn raster(source: &str) -> Result<Preview, &'static str> {
    let image = decode_raster(source)?;
    let image = if image.width() > MAX_WIDTH || image.height() > MAX_HEIGHT {
        ::image::DynamicImage::ImageRgba8(image)
            .resize(
                MAX_WIDTH,
                MAX_HEIGHT,
                ::image::imageops::FilterType::Triangle,
            )
            .into_rgba8()
    } else {
        image
    };
    Ok(Preview::new(
        image.width(),
        image.height(),
        image.into_raw(),
    ))
}

fn prepare_image(source: &str, internal: bool) -> Result<PreparedImage, String> {
    use ::image::ImageEncoder as _;

    let image = decode_raster(source)?;
    let mut png = Vec::new();
    ::image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ::image::ExtendedColorType::Rgba8,
        )
        .map_err(|error| error.to_string())?;
    let png = Arc::from(png);
    if internal {
        Ok(PreparedImage::Internal(png))
    } else {
        kortty_windowing::ClipboardPng::prepare(png)
            .map(PreparedImage::Native)
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SVG: &str = "<svg xmlns='http://www.w3.org/2000/svg' width='12' height='12'><rect width='12' height='12' fill='red'/></svg>";

    #[test]
    fn previews_mermaid_with_readable_native_labels_and_rejects_clicks() {
        let source = "flowchart TD\nA[Start] --> B[Ready]";
        let preview = mermaid(source).unwrap();
        let image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = preview.handle
        else {
            panic!("Mermaid must be rasterized before reaching the widget");
        };
        assert!((12..=MAX_WIDTH).contains(&width));
        assert!((12..=MAX_HEIGHT).contains(&height));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3] == [15, 23, 42])
        );
        assert!(mermaid("flowchart TD\nA[Start]\nclick A \"https://example.invalid\"").is_err());
        assert!(mermaid("gantt\nUnsupported").is_err());
    }

    #[test]
    fn decodes_fenced_and_inline_raster_with_exact_mime_and_dimension_limits() {
        let pixmap = tiny_skia::Pixmap::new(2, 3).unwrap();
        let data = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(pixmap.encode_png().unwrap())
        );
        let preview = raster(&data).unwrap();
        let image::Handle::Rgba { width, height, .. } = preview.handle else {
            panic!("a raster URI must be decoded before reaching the widget");
        };
        assert_eq!((width, height), (2, 3));
        assert!(raster(&data.replace("image/png", "image/gif")).is_err());
        assert!(raster("https://example.invalid/picture.png").is_err());
        let large = tiny_skia::Pixmap::new(MAX_RASTER_EDGE + 1, 1).unwrap();
        let large = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(large.encode_png().unwrap())
        );
        assert!(raster(&large).is_err());

        let answer = format!(
            "![](https://example.invalid/remote.png)\n![preview]({data})\n```text\n{data}\n```"
        );
        let candidates = code_blocks::visual_candidates(&answer, 3);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].content(&answer), data);
        assert_eq!(candidates[1].content(&answer).trim(), data);
        assert!(
            candidates
                .iter()
                .all(|source| source.render(&answer).is_ok())
        );
    }

    #[test]
    fn chat_image_copy_preserves_original_pixels_and_drops_a_stale_selection() {
        let original = ::image::RgbaImage::from_pixel(700, 12, ::image::Rgba([9, 40, 80, 127]));
        let mut bytes = Cursor::new(Vec::new());
        original
            .write_to(&mut bytes, ::image::ImageFormat::Png)
            .unwrap();
        let data = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(bytes.into_inner())
        );
        let PreparedImage::Internal(png) = prepare_image(&data, true).unwrap() else {
            panic!("internal copy must keep the bytes in memory");
        };
        let copied = ::image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(copied, original);
        assert!(matches!(
            prepare_image(&data, false),
            Ok(PreparedImage::Native(_))
        ));

        let turn = Arc::new(Turn::from_parts(
            None,
            "show".into(),
            format!("![image]({data})"),
        ));
        let state = State::default();
        state.prepare(std::slice::from_ref(&turn));
        let live = {
            let mut cache = state.0.borrow_mut();
            let selection = cache.entries[0].selection.clone();
            let live = Arc::new(());
            let lease = Arc::downgrade(&live);
            cache.copy = Some(PendingCopy {
                ticket: 1,
                selection,
                live,
            });
            lease
        };
        state.prepare(&[]);
        assert!(live.upgrade().is_none());
    }

    #[test]
    fn chat_latex_renders_fenced_and_display_math_and_preserves_other_code() {
        let answer = concat!(
            "Before $$\\frac{1}{2} + \\sqrt{x}$$ after\n",
            "```katex\n\\[\\sum_{i=1}^n i\\]\n```\n",
            "```tex\n\\documentclass{article}\n\\begin{document}\n$$x$$\n```\n",
        );
        let sources = code_blocks::visual_candidates(answer, 6);
        assert_eq!(sources.len(), 2);
        for source in sources {
            assert!(source.format == Format::Math);
            let preview = source.render(answer).expect("native math preview");
            let image::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } = preview.handle
            else {
                panic!("math must be rasterized before reaching the widget");
            };
            assert!((12..=MAX_WIDTH).contains(&width));
            assert!((12..=MAX_HEIGHT).contains(&height));
            assert!(pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[0] < 128));
        }
        for source in [
            r"\input{/tmp/external.tex}".to_owned(),
            r"\href{https://example.invalid}{x}".to_owned(),
            "x".repeat(4_096),
            "{".repeat(70) + "x" + &"}".repeat(70),
        ] {
            assert!(math::render(&source).is_err());
        }
    }

    #[test]
    fn preview_cache_reuses_only_current_turns_and_limits_work() {
        let turns = (0..5)
            .map(|_| {
                Arc::new(Turn::from_parts(
                    None,
                    "show".into(),
                    format!("```svg\n{SVG}\n```\n").repeat(4),
                ))
            })
            .collect::<Vec<_>>();
        let cache = State::default();
        cache.prepare(&turns);
        let mut stored = cache.0.borrow_mut();
        assert_eq!(stored.entries.len(), MAX_PREVIEWS);
        assert!(stored.entries.iter().all(|entry| entry.preview.is_ok()));
        assert!(
            !stored
                .entries
                .iter()
                .any(|entry| entry.selection.turn.as_ptr() == Arc::as_ptr(&turns[0]))
        );
        let chosen = stored.entries[0].selection.clone();
        let handle = stored.entries[0].preview.as_ref().unwrap().handle.id();
        stored.entries[0].source_visible = true;
        drop(stored);
        cache.prepare(&turns[2..]);
        let stored = cache.0.borrow();
        let entry = stored
            .entries
            .iter()
            .find(|entry| entry.selection.matches(&chosen))
            .unwrap();
        assert!(entry.source_visible);
        assert_eq!(entry.preview.as_ref().unwrap().handle.id(), handle);
        drop(stored);
        cache.prepare(&turns[..1]);
        assert_eq!(cache.0.borrow().entries.len(), MAX_PREVIEWS_PER_TURN);
        assert!(
            !cache
                .0
                .borrow()
                .entries
                .iter()
                .any(|entry| entry.selection.matches(&chosen))
        );
    }
}
