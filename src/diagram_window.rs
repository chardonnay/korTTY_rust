use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::text::Fragment;
use iced::widget::{
    self, button, column, container, mouse_area, pick_list, row, scrollable, svg, text,
    text_editor, text_input,
};
use iced::{Alignment, Element, Length, Point, Size, Task};
use kortty_diagram::{
    Diagram, DiagramKind, Direction, MAX_SCRIPT_BYTES, MAX_SCRIPT_LINES, Point as ScenePoint,
    Scene, SourceRange, Theme, parse_mermaid, script_flow,
};

mod ai;

const MAX_PREVIEW_DIMENSION: f32 = 4096.0;
const MAX_PREVIEW_PIXELS: f32 = 8_000_000.0;

#[derive(Debug, Clone)]
pub enum Message {
    Close,
    Edit(text_editor::Action),
    Render,
    SetMode(SourceMode),
    Rendered(u64, Result<Arc<Chart>, String>),
    AiRendered(u64, Result<Arc<Chart>, String>),
    ShowAi(bool),
    AiEndpoint(String),
    AiModel(String),
    AiFamily(ai::Family),
    GenerateAi,
    PreviewPointer(Option<Point>),
    JumpToSource,
    ToggleTheme,
    ToggleDirection,
    Zoom(f32),
    Fit,
    CopySource,
    CopyMermaid,
    SetPath(String),
    Export(Format),
    ExportReport,
    Exported(Result<PathBuf, String>),
}

#[derive(Debug, Clone, Copy)]
pub enum Format {
    Svg,
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceMode {
    Mermaid,
    Shell,
}

#[derive(Clone, Debug)]
pub struct Chart {
    diagram: Diagram,
    scene: Arc<Scene>,
    code_links: Option<HashMap<String, SourceRange>>,
    fallback: bool,
}

impl Chart {
    fn local(diagram: Diagram) -> Self {
        let scene = Arc::new(diagram.layout());
        Self {
            diagram,
            scene,
            code_links: None,
            fallback: false,
        }
    }

    fn ai(diagram: Diagram, code_links: HashMap<String, SourceRange>) -> Self {
        Self {
            code_links: Some(code_links),
            ..Self::local(diagram)
        }
    }

    fn fallback(diagram: Diagram) -> Self {
        Self {
            fallback: true,
            ..Self::local(diagram)
        }
    }
}

pub struct DiagramWindow {
    content: text_editor::Content,
    editor_id: widget::Id,
    chart: Option<Arc<Chart>>,
    scene: Option<Arc<Scene>>,
    svg: Option<svg::Handle>,
    size: Size,
    scale_factor: f32,
    scale: Option<f32>,
    theme: Theme,
    mode: SourceMode,
    pointer: Option<Point>,
    revision: u64,
    path: String,
    status: Option<String>,
    ai_allowed: bool,
    show_ai: bool,
    ai: ai::Draft,
    ai_pending: bool,
    ai_cancel: Option<crate::local_ai::Cancel>,
}

impl DiagramWindow {
    pub fn source(&self) -> String {
        self.content.text()
    }

    fn script_source(&self) -> Result<String, String> {
        if self.content.line_count() > MAX_SCRIPT_LINES {
            return Err(format!("Source is limited to {MAX_SCRIPT_LINES} lines"));
        }
        let mut source = String::new();
        for (index, line) in self.content.lines().enumerate() {
            let separator = usize::from(index != 0);
            if source.len() + line.text.len() + separator > MAX_SCRIPT_BYTES {
                return Err("Source is limited to 2 MiB".into());
            }
            if separator != 0 {
                source.push('\n');
            }
            source.push_str(&line.text);
        }
        Ok(source)
    }

    pub fn mermaid(&self) -> Option<String> {
        Some(self.chart.as_ref()?.diagram.to_mermaid())
    }

    pub fn new(size: Size, ai_allowed: bool) -> Self {
        Self {
            content: text_editor::Content::new(),
            editor_id: widget::Id::unique(),
            chart: None,
            scene: None,
            svg: None,
            size,
            scale_factor: 2.0,
            scale: None,
            theme: Theme::Dark,
            mode: SourceMode::Mermaid,
            pointer: None,
            revision: 0,
            path: String::new(),
            status: None,
            ai_allowed,
            show_ai: false,
            ai: ai::Draft::default(),
            ai_pending: false,
            ai_cancel: None,
        }
    }

    pub(crate) fn with_script(size: Size, ai_allowed: bool, script: &str) -> Self {
        let mut window = Self::new(size, ai_allowed);
        window.content = text_editor::Content::with_text(script);
        window.mode = SourceMode::Shell;
        window
    }

    pub(crate) fn configure_ai(
        &mut self,
        allowed: bool,
        configuration: Option<Arc<crate::ai_manager::Configuration>>,
    ) {
        self.cancel_ai_preview();
        self.ai.configure(configuration);
        self.ai_allowed = allowed;
        if !allowed {
            self.show_ai = false;
        }
    }

    pub fn resize(&mut self, size: Size) {
        self.size = size;
    }

    pub fn set_scale_factor(&mut self, factor: f32) {
        if factor.is_finite() && factor > 0.0 {
            self.scale_factor = factor;
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Close => {}
            Message::Edit(action) => {
                if action.is_edit() {
                    self.clear_preview();
                }
                self.content.perform(action);
            }
            Message::SetMode(mode) => {
                if self.mode != mode {
                    self.mode = mode;
                    self.show_ai = false;
                    self.clear_preview();
                }
            }
            Message::Render => {
                let source = match self.mode {
                    SourceMode::Mermaid => Ok(self.source()),
                    SourceMode::Shell => self.script_source(),
                };
                self.clear_preview();
                let source = match source {
                    Ok(source) => source,
                    Err(error) => {
                        self.status = Some(error);
                        return Task::none();
                    }
                };
                let revision = self.revision;
                let mode = self.mode;
                self.status = Some("Rendering diagram…".into());
                return Task::future(async move {
                    let diagram = match mode {
                        SourceMode::Mermaid => {
                            parse_mermaid(&source).map_err(|error| error.to_string())
                        }
                        SourceMode::Shell => {
                            script_flow(&source).map_err(|error| error.to_string())
                        }
                    };
                    Message::Rendered(revision, diagram.map(Chart::local).map(Arc::new))
                });
            }
            Message::Rendered(revision, result) => self.accept_render(revision, result),
            Message::AiRendered(revision, result) if revision == self.revision => {
                self.ai_pending = false;
                self.ai_cancel = None;
                self.accept_render(revision, result);
            }
            Message::AiRendered(_, _) => {}
            Message::ShowAi(show) if self.ai_allowed && self.mode == SourceMode::Shell => {
                self.show_ai = show;
            }
            Message::AiEndpoint(value) => {
                self.cancel_ai_preview();
                self.ai.manual_override();
                self.ai.endpoint = value;
            }
            Message::AiModel(value) => {
                self.cancel_ai_preview();
                self.ai.manual_override();
                self.ai.model = value;
            }
            Message::AiFamily(value) => {
                self.cancel_ai_preview();
                self.ai.family = value;
            }
            Message::GenerateAi if self.ai_allowed && self.show_ai && !self.ai_pending => {
                match self
                    .script_source()
                    .and_then(|source| self.ai.start(source))
                {
                    Ok(crate::local_ai::Pending { reply, cancel }) => {
                        self.clear_preview();
                        self.ai_pending = true;
                        self.ai_cancel = Some(cancel);
                        let revision = self.revision;
                        self.status = Some("Waiting for the local AI diagram…".into());
                        return Task::future(async move {
                            let result = reply
                                .recv()
                                .await
                                .unwrap_or_else(|_| Err("The AI worker stopped".into()))
                                .map(Arc::new);
                            Message::AiRendered(revision, result)
                        });
                    }
                    Err(error) => self.status = Some(error),
                }
            }
            Message::ShowAi(_) | Message::GenerateAi => {}
            Message::PreviewPointer(position) => self.pointer = position,
            Message::JumpToSource => {
                if let Some(source) = self.pointed_source() {
                    self.content
                        .perform(text_editor::Action::Move(text_editor::Motion::Home));
                    self.content.move_to(text_editor::Cursor {
                        position: text_editor::Position {
                            line: source.start() as usize - 1,
                            column: 0,
                        },
                        selection: None,
                    });
                    return widget::operation::focus(self.editor_id.clone());
                }
            }
            Message::ToggleTheme => {
                self.theme = if self.theme == Theme::Dark {
                    Theme::Light
                } else {
                    Theme::Dark
                };
                self.refresh_svg();
            }
            Message::ToggleDirection => {
                if let Some(mut chart) = self.chart.take() {
                    self.clear_preview();
                    let revision = self.revision;
                    return Task::future(async move {
                        let chart_data = Arc::make_mut(&mut chart);
                        let direction = match chart_data.diagram.direction() {
                            Direction::TopToBottom => Direction::LeftToRight,
                            Direction::LeftToRight => Direction::TopToBottom,
                        };
                        chart_data.diagram.set_direction(direction);
                        chart_data.scene = Arc::new(chart_data.diagram.layout());
                        Message::Rendered(revision, Ok(chart))
                    });
                }
            }
            Message::Zoom(factor) => {
                self.scale = Some(
                    (self.zoom() * factor)
                        .max(f32::EPSILON)
                        .min(self.max_preview_scale()),
                );
            }
            Message::Fit => self.scale = None,
            Message::CopySource | Message::CopyMermaid => {}
            Message::SetPath(path) => self.path = path,
            Message::Export(format) => {
                if let Some(scene) = &self.scene {
                    let path = PathBuf::from(self.path.trim());
                    let scene = Arc::clone(scene);
                    let theme = self.theme;
                    return Task::future(async move {
                        Message::Exported(
                            export(&path, &scene, theme, format)
                                .map(|()| path)
                                .map_err(|error| error.to_string()),
                        )
                    });
                }
            }
            Message::ExportReport => {
                if let Some(source) = self.mermaid() {
                    let path = PathBuf::from(self.path.trim());
                    return Task::future(async move {
                        Message::Exported(
                            write_export(&path, report(&source).as_bytes())
                                .map(|()| path)
                                .map_err(|error| error.to_string()),
                        )
                    });
                }
            }
            Message::Exported(result) => {
                self.status = Some(match result {
                    Ok(path) => format!("Saved {}", path.display()),
                    Err(error) => format!("Export failed ({error})"),
                });
            }
        }
        Task::none()
    }

    pub fn view(&self, content_theme: iced::Theme) -> Element<'_, Message> {
        let mut toolbar = row![
            button("Mermaid")
                .style(if self.mode == SourceMode::Mermaid {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Message::SetMode(SourceMode::Mermaid)),
            button("Script")
                .style(if self.mode == SourceMode::Shell {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press(Message::SetMode(SourceMode::Shell)),
            button("Render").on_press(Message::Render),
            button("Copy source").on_press(Message::CopySource),
        ]
        .spacing(6)
        .align_y(Alignment::Center);
        if self.ai_allowed && self.mode == SourceMode::Shell {
            toolbar = toolbar.push(
                button(if self.show_ai {
                    "Hide AI"
                } else {
                    "Local AI…"
                })
                .on_press(Message::ShowAi(!self.show_ai)),
            );
        }
        if let Some(chart) = &self.chart {
            if chart.diagram.kind() != DiagramKind::Sequence {
                toolbar = toolbar.push(
                    button(match chart.diagram.direction() {
                        Direction::TopToBottom => "Vertical",
                        Direction::LeftToRight => "Horizontal",
                    })
                    .on_press(Message::ToggleDirection),
                );
            }
            toolbar = toolbar.extend([
                button("−").on_press(Message::Zoom(0.8)).into(),
                button("+").on_press(Message::Zoom(1.25)).into(),
                button("Fit").on_press(Message::Fit).into(),
                text(format!("{:.0}%", self.zoom() * 100.0)).into(),
                button(if self.theme == Theme::Dark {
                    "Light"
                } else {
                    "Dark"
                })
                .on_press(Message::ToggleTheme)
                .into(),
                button("Copy Mermaid").on_press(Message::CopyMermaid).into(),
            ]);
        }
        let editor = text_editor(&self.content)
            .id(self.editor_id.clone())
            .style(move |_, status| text_editor::default(&content_theme, status))
            .font(iced::Font::MONOSPACE)
            .placeholder(match self.mode {
                SourceMode::Mermaid => {
                    "Paste a flowchart, sequence, state, class, ER, mindmap, or architecture diagram"
                }
                SourceMode::Shell => "Paste source code. Local Render supports shell scripts.",
            })
            .height(Length::Fill)
            .on_action(Message::Edit)
            .key_binding(diagram_key_binding);
        let mut contents = column![toolbar.width(Length::Fill).wrap()]
            .spacing(8)
            .padding(10);
        if self.show_ai {
            contents = contents.push(self.ai_settings());
        }
        contents = contents.push(
            row![
                container(editor)
                    .width(Length::FillPortion(2))
                    .height(Length::Fill),
                container(self.preview())
                    .width(Length::FillPortion(3))
                    .height(Length::Fill),
            ]
            .spacing(10),
        );
        if self.scene.is_some() {
            contents = contents.push(
                row![
                    text_input("Absolute export path", &self.path).on_input(Message::SetPath),
                    button("Save SVG").on_press_maybe(
                        (!self.path.trim().is_empty()).then_some(Message::Export(Format::Svg))
                    ),
                    button("Save PNG").on_press_maybe(
                        (!self.path.trim().is_empty()).then_some(Message::Export(Format::Png))
                    ),
                    button("Save Markdown").on_press_maybe(
                        (!self.path.trim().is_empty()).then_some(Message::ExportReport)
                    ),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            );
        }
        let status = self.status.as_deref().map(Fragment::Borrowed).or_else(|| {
            self.pointed_source().map(|source| {
                Fragment::Owned(format!(
                    "Source lines {}–{} · click to jump",
                    source.start(),
                    source.end()
                ))
            })
        });
        if self.scene.is_some() {
            // Keep the export controls in place when the pointer leaves the preview.
            contents = contents.push(
                text(status.unwrap_or_default())
                    .size(13)
                    .height(18)
                    .wrapping(widget::text::Wrapping::None),
            );
        } else if let Some(status) = status {
            contents = contents.push(text(status).size(13));
        }
        contents.into()
    }

    fn ai_settings(&self) -> Element<'_, Message> {
        column![
            row![
                text_input("Loopback address and port", &self.ai.endpoint)
                    .on_input(Message::AiEndpoint)
                    .width(180),
                text_input("Installed Ollama model", &self.ai.model)
                    .on_input(Message::AiModel),
                pick_list(ai::Family::ALL, Some(self.ai.family), Message::AiFamily),
                button("Generate with AI")
                    .on_press_maybe((!self.ai_pending).then_some(Message::GenerateAi)),
            ].spacing(6),
            text("Generate sends numbered source to the configured loopback service. Long scripts use a structural outline. A model may forward the input.")
                .size(13),
        ]
        .spacing(6)
        .into()
    }

    fn accept_render(&mut self, revision: u64, result: Result<Arc<Chart>, String>) {
        if revision != self.revision {
            return;
        }
        match result {
            Ok(chart) => {
                self.status = chart.fallback.then(|| {
                    "AI diagram unavailable. Showing the locally generated flowchart.".into()
                });
                self.chart = Some(chart);
                self.refresh_scene();
                self.scale = None;
            }
            Err(error) => self.status = Some(error),
        }
    }

    fn refresh_scene(&mut self) {
        self.scene = self.chart.as_ref().map(|chart| Arc::clone(&chart.scene));
        self.refresh_svg();
    }

    fn refresh_svg(&mut self) {
        self.svg = self
            .scene
            .as_ref()
            .map(|scene| svg::Handle::from_memory(scene.to_svg(self.theme).into_bytes()));
    }

    fn clear_preview(&mut self) {
        self.revision += 1;
        self.ai_pending = false;
        self.ai_cancel = None;
        self.chart = None;
        self.scene = None;
        self.svg = None;
        self.pointer = None;
        self.status = None;
    }

    fn cancel_ai_preview(&mut self) {
        if self.ai_pending {
            self.clear_preview();
        }
    }

    fn pointed_source(&self) -> Option<SourceRange> {
        let point = self.pointer?;
        let zoom = self.zoom();
        let point = ScenePoint {
            x: (point.x / zoom).floor() as i32,
            y: (point.y / zoom).floor() as i32,
        };
        let scene = self.scene.as_ref()?;
        match &self.chart.as_ref()?.code_links {
            Some(links) => links.get(scene.node_at(point)?.id()).copied(),
            None => scene.source_at(point),
        }
    }

    fn zoom(&self) -> f32 {
        let zoom = self.scale.unwrap_or_else(|| {
            self.scene.as_ref().map_or(1.0, |scene| {
                let bounds = scene.bounds();
                let width = (self.size.width - 30.0) * 0.6;
                let height = self.size.height - 130.0;
                (width / bounds.width as f32)
                    .min(height / bounds.height as f32)
                    .min(1.0)
            })
        });
        zoom.max(f32::EPSILON).min(self.max_preview_scale())
    }

    fn max_preview_scale(&self) -> f32 {
        self.scene.as_ref().map_or(4.0, |scene| {
            let bounds = scene.bounds();
            let width = (bounds.width as f32 * self.scale_factor).max(1.0);
            let height = (bounds.height as f32 * self.scale_factor).max(1.0);
            let extent_limit = MAX_PREVIEW_DIMENSION / width.max(height);
            let pixel_limit = (MAX_PREVIEW_PIXELS / (width * height)).sqrt();
            4.0_f32.min(extent_limit).min(pixel_limit)
        })
    }

    fn preview(&self) -> Element<'_, Message> {
        let (Some(svg), Some(scene)) = (&self.svg, &self.scene) else {
            return container(text("Diagram preview"))
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .into();
        };
        let scale = self.zoom();
        let bounds = scene.bounds();
        scrollable(
            mouse_area(
                iced::widget::svg(svg.clone())
                    .width(bounds.width as f32 * scale)
                    .height(bounds.height as f32 * scale),
            )
            .on_move(|point| Message::PreviewPointer(Some(point)))
            .on_exit(Message::PreviewPointer(None))
            .on_press(Message::JumpToSource),
        )
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::new(),
            horizontal: scrollable::Scrollbar::new(),
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

fn diagram_key_binding(event: text_editor::KeyPress) -> Option<text_editor::Binding<Message>> {
    if event.modifiers.command()
        && !event.modifiers.alt()
        && event.key.to_latin(event.physical_key) == Some('w')
    {
        return Some(text_editor::Binding::Custom(Message::Close));
    }
    let command = event.modifiers.logo() || (event.modifiers.command() && !event.modifiers.alt());
    text_editor::Binding::from_key_press(event)
        .filter(|binding| !command || !matches!(binding, text_editor::Binding::Insert(_)))
}

pub(crate) fn export(path: &Path, scene: &Scene, theme: Theme, format: Format) -> io::Result<()> {
    let bytes = match format {
        Format::Svg => scene.to_svg(theme).into_bytes(),
        Format::Png => scene.to_png(theme, 1).map_err(io::Error::other)?,
    };
    write_export(path, &bytes)
}

fn report(source: &str) -> String {
    let ticks = source
        .split(|character| character != '\x60')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "\x60".repeat((ticks + 1).max(3));
    format!(
        "# Diagram\n\n{fence}mermaid\n{}\n{fence}\n",
        source.trim_end()
    )
}

fn write_export(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "enter an absolute file path",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_diagram::{Diagram, Direction, Node, NodeShape};

    #[test]
    fn clicking_a_zoomed_message_navigates_to_its_source_line() {
        use iced::advanced::{Layout, Shell, clipboard, layout, widget::Tree};
        use iced::{Event, Font, Pixels, Renderer, mouse};

        let source = "sequenceDiagram\nparticipant Client\nClient->>Server: Connect";
        let chart = Arc::new(Chart::local(parse_mermaid(source).unwrap()));
        let bounds = chart.scene.edges()[0].label_bounds().unwrap();
        let mut window = DiagramWindow::new(Size::new(1024.0, 768.0), false);
        window.content = text_editor::Content::with_text(source);
        let _ = window.update(Message::Rendered(0, Ok(chart)));
        let _ = window.update(Message::Zoom(1.5));
        let zoom = window.zoom();
        let position = Point::new(
            (bounds.x as f32 + bounds.width as f32 / 2.0) * zoom,
            (bounds.y as f32 + bounds.height as f32 / 2.0) * zoom,
        );
        let mut messages = Vec::new();
        {
            let mut preview = window.preview();
            let mut tree = Tree::new(preview.as_widget());
            let renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
            let limits = layout::Limits::new(Size::ZERO, Size::new(600.0, 700.0));
            let node = preview
                .as_widget_mut()
                .layout(&mut tree, &renderer, &limits);
            assert!(node.bounds().contains(position));
            for event in [
                mouse::Event::CursorMoved { position },
                mouse::Event::ButtonPressed(mouse::Button::Left),
            ] {
                preview.as_widget_mut().update(
                    &mut tree,
                    &Event::Mouse(event),
                    Layout::new(&node),
                    mouse::Cursor::Available(position),
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &node.bounds(),
                );
            }
        }
        assert!(matches!(
            messages.as_slice(),
            [Message::PreviewPointer(Some(_)), Message::JumpToSource]
        ));
        for message in messages {
            let _ = window.update(message);
        }
        assert_eq!(window.content.cursor().position.line, 2);
    }

    #[test]
    fn script_snapshot_preserves_editor_lines_across_line_endings() {
        let mut window = DiagramWindow::new(Size::new(1024.0, 768.0), false);
        window.content = text_editor::Content::with_text("echo 1\recho 2\r\necho 3\n\recho 4");
        let source = window.script_source().unwrap();
        assert_eq!(source, "echo 1\necho 2\necho 3\necho 4");
        let chart = script_flow(&source).unwrap();
        assert_eq!(chart.nodes()[4].source_range(), SourceRange::new(4, 4));
        assert_eq!(window.content.line(3).unwrap().text, "echo 4");
    }

    #[test]
    fn ai_source_target_is_bound_to_a_node_even_on_shared_mermaid_lines() {
        let diagram = parse_mermaid("flowchart TD\na[A] --> b[B]").unwrap();
        let source = SourceRange::new(3, 3).unwrap();
        let chart = Arc::new(Chart::ai(diagram, HashMap::from([("a".into(), source)])));
        let mut window = DiagramWindow::new(Size::new(1024.0, 768.0), true);
        let _ = window.update(Message::AiRendered(0, Ok(chart.clone())));
        let zoom = window.zoom();
        for node in chart.scene.nodes() {
            let bounds = node.bounds();
            window.pointer = Some(Point::new(
                (bounds.x as f32 + bounds.width as f32 / 2.0) * zoom,
                (bounds.y as f32 + bounds.height as f32 / 2.0) * zoom,
            ));
            assert_eq!(
                window.pointed_source(),
                (node.id() == "a").then_some(source)
            );
        }
    }

    #[test]
    fn preview_caps_physical_raster_size_even_at_maximum_zoom() {
        let mut diagram = Diagram::new(Direction::TopToBottom);
        for index in 0..120 {
            diagram
                .add_node(
                    Node::new(
                        format!("node{index}"),
                        "A long node label",
                        NodeShape::Action,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let mut window = DiagramWindow::new(Size::new(1024.0, 768.0), false);
        let scene = Arc::new(diagram.layout());
        let bounds = scene.bounds();
        window.scene = Some(scene);
        window.scale = Some(4.0);
        window.set_scale_factor(3.0);
        let width = bounds.width as f32 * window.zoom() * window.scale_factor;
        let height = bounds.height as f32 * window.zoom() * window.scale_factor;
        assert!(width.max(height).round() <= MAX_PREVIEW_DIMENSION);
        assert!(width * height <= MAX_PREVIEW_PIXELS + 1.0);
    }
}
