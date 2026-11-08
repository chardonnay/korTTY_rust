use std::sync::Arc;

use iced::widget::{
    button, column, container, row, slider, space, text, text_editor, text_input, tooltip,
};
use iced::{Alignment, Element, Length, Task, window};

use crate::session_journal::annotation::{
    self, Annotation, DEFAULT_COLOR, DEFAULT_STROKE_WIDTH, Kind, MAX_ANNOTATIONS, MAX_POINTS_TOTAL,
};
use crate::session_journal::screenshot_edit::Draft;
use crate::terminal_theme::Rgb;
use crate::workspace::chrome::popup;

use super::canvas::{Brush, Picture};
use super::{Desktop, Event, Loaded, Message, Preview, note_translation, read_task};

const COLOURS: [u32; 8] = [
    0xe11d48, 0xf97316, 0xeab308, 0x16a34a, 0x0284c7, 0x7c3aed, 0xffffff, 0x000000,
];

pub(super) struct Form {
    draft: Arc<Draft>,
    note: String,
    note_editor: text_editor::Content,
    translation: note_translation::State,
    marks: Vec<Annotation>,
    tool: Kind,
    colour: Rgb,
    colour_input: String,
    colour_picker: bool,
    width: u8,
    preview: Arc<Preview>,
    revision: u64,
    rendering: Option<u64>,
    render_problem: Option<String>,
    status: Option<String>,
}

impl Form {
    pub(super) fn new(loaded: Arc<Loaded>) -> Self {
        let draft = Arc::clone(&loaded.draft);
        let note = draft.note().to_owned();
        let marks = draft.annotations().to_vec();
        let colour = Rgb::from_bytes(DEFAULT_COLOR);
        Self {
            draft,
            note_editor: text_editor::Content::with_text(&note),
            note,
            translation: note_translation::State::default(),
            marks,
            tool: Kind::Pen,
            colour,
            colour_input: colour.to_string(),
            colour_picker: false,
            width: DEFAULT_STROKE_WIDTH as u8,
            preview: Arc::new(Preview {
                image: loaded.preview.image.clone(),
                dimensions: loaded.preview.dimensions,
            }),
            revision: 0,
            rendering: None,
            render_problem: None,
            status: None,
        }
    }

    pub(super) fn draft(&self) -> &Arc<Draft> {
        &self.draft
    }

    pub(super) fn annotations(&self) -> &[Annotation] {
        &self.marks
    }

    pub(super) fn note_for_save(&self) -> String {
        self.note.clone()
    }

    pub(super) fn translate_note(
        &mut self,
        id: window::Id,
        event: note_translation::Event,
        setup: note_translation::Setup,
    ) -> note_translation::Update {
        let mut update = self
            .translation
            .update(event, &self.note, setup, move |event| {
                Desktop::SessionJournal(id, Message::Annotate(Event::Translate(event)))
            });
        if let Some(note) = update.take_replacement() {
            self.note_editor = text_editor::Content::with_text(&note);
            self.note_editor
                .perform(text_editor::Action::Move(text_editor::Motion::DocumentEnd));
            self.note = note;
        }
        update
    }

    pub(super) fn cancel_translation(&mut self) {
        self.translation.cancel();
    }

    #[cfg(test)]
    pub(super) fn fixture_pending_translation(&mut self, profile: crate::local_ai::Profile) -> u64 {
        self.translation.fixture_pending(&self.note, profile)
    }

    pub(super) fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    pub(super) fn can_save(&self) -> bool {
        self.rendering.is_none() && self.render_problem.is_none()
    }

    pub(super) fn can_mark(&self, revision: u64) -> bool {
        revision == self.revision
            && self.draft.can_annotate()
            && self.marks.len() < MAX_ANNOTATIONS
            && self.points_left() > 0
    }

    pub(super) fn add(&mut self, id: window::Id, mark: Annotation) -> Task<Desktop> {
        if !self.can_mark(self.revision) || mark.points.len() > self.points_left() {
            return Task::none();
        }
        self.marks.push(mark);
        if let Err(error) = annotation::validate(&self.marks) {
            self.marks.pop();
            self.set_status(error.to_string());
            return Task::none();
        }
        self.changed(id)
    }

    pub(super) fn update(&mut self, id: window::Id, event: Event) -> Task<Desktop> {
        match event {
            Event::Select(tool) => self.tool = tool,
            Event::ChooseColour => self.colour_picker = true,
            Event::CloseColour => {
                self.colour_input = self.colour.to_string();
                self.colour_picker = false;
            }
            Event::Colour(value) if value.len() <= 7 => {
                if let Some(colour) = Rgb::parse(&value) {
                    self.colour = colour;
                }
                self.colour_input = value;
            }
            Event::Width(width) if (2..=24).contains(&width) => self.width = width,
            Event::Mark(revision, mark) if self.can_mark(revision) && mark.kind != Kind::Text => {
                return self.add(id, mark);
            }
            Event::Undo if self.draft.can_annotate() && self.marks.pop().is_some() => {
                return self.changed(id);
            }
            Event::Clear if self.draft.can_annotate() && !self.marks.is_empty() => {
                self.marks.clear();
                return self.changed(id);
            }
            Event::Note(action) => {
                if super::super::entry_editor::edit_text(
                    &mut self.note_editor,
                    &mut self.note,
                    action,
                ) {
                    self.translation.edited();
                }
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn rendered(
        &mut self,
        id: window::Id,
        revision: u64,
        result: Result<Arc<Preview>, String>,
    ) -> Task<Desktop> {
        if self.rendering != Some(revision) {
            return Task::none();
        }
        self.rendering = None;
        if self.revision != revision {
            return self.render(id);
        }
        match result {
            Ok(preview) => self.preview = preview,
            Err(error) => self.render_problem = Some(error),
        }
        Task::none()
    }

    fn changed(&mut self, id: window::Id) -> Task<Desktop> {
        self.revision = self.revision.wrapping_add(1);
        self.status = None;
        self.render_problem = None;
        self.render(id)
    }

    fn render(&mut self, id: window::Id) -> Task<Desktop> {
        if self.rendering.is_some() {
            return Task::none();
        }
        let revision = self.revision;
        self.rendering = Some(revision);
        let draft = Arc::clone(&self.draft);
        let marks = self.marks.clone();
        read_task(
            id,
            move || Preview::render(&draft, &marks),
            move |result| Message::Annotate(Event::Rendered(revision, result)),
        )
    }

    fn points_left(&self) -> usize {
        MAX_POINTS_TOTAL.saturating_sub(self.marks.iter().map(|mark| mark.points.len()).sum())
    }

    pub(super) fn view<'a>(
        &'a self,
        enabled: bool,
        busy: bool,
        saving: bool,
        translation: note_translation::View<'a>,
    ) -> Element<'a, Event> {
        let annotate = enabled && self.draft.can_annotate();
        let ready =
            annotate && self.can_mark(self.revision) && Rgb::parse(&self.colour_input).is_some();
        let controls = row![
            self.tool("Pen", Kind::Pen, annotate),
            self.tool("Box", Kind::Box, annotate),
            tooltip(
                self.tool("Unreadable", Kind::Pixelate, annotate),
                text("Drag a rectangle to coarsen its contents into blocks."),
                tooltip::Position::Bottom,
            ),
            self.tool("Text", Kind::Text, annotate),
            text("Colour"),
            self.colour_field(annotate),
            text("Width"),
            if annotate {
                Element::from(slider(2..=24, self.width, Event::Width).width(Length::Fixed(102.0)))
            } else {
                Element::from(text(self.width).width(102))
            },
            text(self.width),
            button("Undo")
                .on_press_maybe((annotate && !self.marks.is_empty()).then_some(Event::Undo)),
            button("Remove all")
                .on_press_maybe((annotate && !self.marks.is_empty()).then_some(Event::Clear)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .wrap();
        let picture = Picture {
            image: &self.preview.image,
            dimensions: self.preview.dimensions,
            brush: Brush {
                tool: self.tool,
                color: self.colour.bytes(),
                width: f32::from(self.width),
            },
            revision: self.revision,
            points_left: self.points_left(),
            active: ready,
        };
        let mut note = text_editor(&self.note_editor)
            .placeholder("What should someone reading this journal know about the picture?")
            .height(104);
        if enabled {
            note = note
                .on_action(Event::Note)
                .key_binding(move |key| self.translation.binding(key, Event::Translate));
        }
        let status = if saving {
            Some("Saving screenshot…")
        } else if self.status.is_some() {
            self.status.as_deref()
        } else if busy {
            Some("Save or revert open journal entry changes before saving the screenshot.")
        } else if !self.draft.can_annotate() {
            Some(
                "The original image cannot be verified for editing. You can still change the note.",
            )
        } else if self.rendering.is_some() {
            Some("Updating preview…")
        } else if Rgb::parse(&self.colour_input).is_none() {
            Some("Enter a six-digit RGB colour.")
        } else {
            self.render_problem.as_deref()
        };
        container(column![
            controls,
            picture.view(),
            text("Note"),
            note,
            self.translation.view(&self.note, translation, Event::Translate),
            text("The unmarked capture is kept locally when you add marks. Exports use the visible image.").size(12),
            row![
                status.map(|status| text(status).size(13)),
                space::horizontal(),
                button("Cancel").on_press_maybe((!saving).then_some(Event::Close)),
                button("OK").on_press_maybe((enabled && !busy && self.can_save()).then_some(Event::Save)),
            ].spacing(8).align_y(Alignment::Center),
        ].spacing(8)).padding(12).width(Length::Fill).height(Length::Fill).into()
    }

    fn tool(
        &self,
        label: &'static str,
        kind: Kind,
        enabled: bool,
    ) -> iced::widget::Button<'_, Event> {
        button(label)
            .style(if self.tool == kind {
                button::primary
            } else {
                button::secondary
            })
            .on_press_maybe(enabled.then_some(Event::Select(kind)))
    }

    fn colour_field(&self, enabled: bool) -> Element<'_, Event> {
        let trigger = button(
            row![swatch(self.colour), text(self.colour.to_string()).size(13)]
                .spacing(5)
                .align_y(Alignment::Center),
        )
        .on_press_maybe(enabled.then_some(Event::ChooseColour))
        .into();
        let panel = (enabled && self.colour_picker).then(|| {
            let mut palette = row![].spacing(4);
            for colour in COLOURS.map(Rgb::from_hex) {
                palette = palette.push(
                    button(swatch(colour))
                        .padding(4)
                        .on_press(Event::Colour(colour.to_string())),
                );
            }
            container(
                column![
                    palette,
                    text_input("#RRGGBB", &self.colour_input).on_input(Event::Colour),
                    button("Done").on_press(Event::CloseColour),
                ]
                .spacing(6),
            )
            .width(258)
            .padding(8)
            .style(container::bordered_box)
            .into()
        });
        popup::popover(trigger, panel, Event::CloseColour)
    }
}

fn swatch(colour: Rgb) -> Element<'static, Event> {
    container(space().width(15).height(15))
        .style(move |_| container::Style::default().background(colour.color()))
        .into()
}
