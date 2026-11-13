//! Revision-checked background analysis for the active snippet editor.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use iced::widget;
use iced::{Element, Task};
use serde::{Deserialize, Serialize};

use crate::editor_window::structure::{self, Analysis};
use crate::snippets::{
    Variable,
    resolve::{self, PlaceholderDeclaration, PlaceholderReport},
};

use super::{Message as WindowMessage, SnippetWindow, Update};

mod view;

const EDIT_DELAY: Duration = Duration::from_millis(350);
const CARET_DELAY: Duration = Duration::from_millis(150);
const DEFAULT_WIDTH: u16 = 340;
static NEXT_SOURCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(crate) struct Preferences {
    pub(crate) visible: bool,
    width: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            visible: false,
            width: DEFAULT_WIDTH,
        }
    }
}

impl Preferences {
    pub(crate) fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn width(self) -> u16 {
        self.width.clamp(160, 1600)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Span {
    pub(super) line: usize,
    pub(super) column: usize,
    pub(super) end_column: usize,
}

pub struct Report {
    analysis: Analysis,
    placeholders: PlaceholderReport,
}

#[derive(Clone)]
pub enum Message {
    Toggle,
    Ready(u64, Arc<Report>),
    Panel(view::Message),
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Source {
    text: u64,
    declarations: u64,
}

#[derive(Clone, Copy)]
struct Scan {
    ticket: u64,
    source: Source,
}

pub(super) struct Editor {
    generation: u64,
    edited_at: Option<Instant>,
    result: Option<(Scan, Arc<Report>)>,
    panel: view::Panel,
    caret_due: Option<Instant>,
}

impl Default for Editor {
    fn default() -> Self {
        Self {
            generation: NEXT_SOURCE.fetch_add(1, Ordering::Relaxed),
            edited_at: None,
            result: None,
            panel: view::Panel::default(),
            caret_due: None,
        }
    }
}

impl Editor {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn changed(&mut self, edited_at: Option<Instant>) {
        self.generation = NEXT_SOURCE.fetch_add(1, Ordering::Relaxed);
        self.edited_at = edited_at;
        self.caret_due = None;
    }

    pub(super) fn caret_moved(&mut self) {
        self.caret_due = Some(Instant::now() + CARET_DELAY);
    }
}

#[derive(Default)]
pub(super) struct State {
    preferences: Preferences,
    declarations: Arc<[PlaceholderDeclaration]>,
    declarations_revision: u64,
    flight: Option<Scan>,
    next_ticket: u64,
    show_immediately: bool,
}

impl State {
    pub(super) fn declarations_changed(&mut self, variables: &[Variable]) {
        let declarations = resolve::placeholder_declarations(variables);
        if self.declarations.as_ref() != declarations.as_slice() {
            self.declarations = declarations.into();
            self.declarations_revision = self.declarations_revision.wrapping_add(1);
        }
    }
}

impl SnippetWindow {
    pub(crate) fn structure_preferences(&self) -> Preferences {
        self.structure.preferences
    }

    pub(crate) fn apply_structure_preferences(&mut self, preferences: Preferences) {
        self.structure.show_immediately |=
            preferences.visible && !self.structure.preferences.visible;
        self.structure.preferences = preferences;
        let width = f32::from(preferences.width());
        self.draft.structure.panel.set_width(width);
    }

    fn structure_source(&self) -> Source {
        Source {
            text: self.draft.structure.generation,
            declarations: self.structure.declarations_revision,
        }
    }

    fn structure_result(&self) -> Option<&(Scan, Arc<Report>)> {
        self.draft
            .structure
            .result
            .as_ref()
            .filter(|(scan, _)| scan.source == self.structure_source())
    }

    fn structure_active(&self) -> bool {
        self.structure.preferences.visible
            && !self.tabs.is_empty()
            && !self.tabs.is_preview()
            && self.prompt.is_none()
            && self.hosted_close.is_none()
            && self.variables.is_none()
            && self.copy.is_none()
            && self.archive.is_none()
            && self.details.is_none()
            && !self.folders.modal()
            && !self.recovery_busy
    }

    pub(crate) fn structure_needs_tick(&self) -> bool {
        self.structure_active()
            && if self.structure_result().is_some() {
                self.draft.structure.caret_due.is_some()
            } else {
                self.structure.flight.is_none()
            }
    }

    pub(crate) fn poll_structure(&mut self, now: Instant) -> Task<WindowMessage> {
        if !self.structure_active() {
            return Task::none();
        }
        let editor = &mut self.draft.structure;
        let source = Source {
            text: editor.generation,
            declarations: self.structure.declarations_revision,
        };
        if let Some((scan, report)) = &editor.result
            && scan.source == source
        {
            if editor.caret_due.is_some_and(|due| now >= due) {
                editor.caret_due = None;
                editor
                    .panel
                    .follow_caret(report, self.draft.content.cursor().position.line);
            }
            self.structure.show_immediately = false;
            return Task::none();
        }
        if self.structure.flight.is_some()
            || !self.structure.show_immediately
                && editor
                    .edited_at
                    .is_some_and(|edited| now.saturating_duration_since(edited) < EDIT_DELAY)
        {
            return Task::none();
        }
        self.structure.show_immediately = false;
        self.structure.next_ticket = self.structure.next_ticket.wrapping_add(1);
        let scan = Scan {
            ticket: self.structure.next_ticket,
            source,
        };
        self.structure.flight = Some(scan);
        let text = self
            .draft
            .base
            .as_ref()
            .filter(|_| !self.draft.content_changed)
            .map_or_else(
                || Arc::<str>::from(self.draft.content.text()),
                super::Snippet::shared_content,
            );
        let language = self.draft.language.clone();
        let declarations = Arc::clone(&self.structure.declarations);
        Task::perform(
            async move {
                Arc::new(Report {
                    analysis: structure::analyze(&text, &language),
                    placeholders: resolve::inspect_placeholders(&text, &declarations),
                })
            },
            move |report| WindowMessage::Structure(Message::Ready(scan.ticket, report)),
        )
    }

    pub(super) fn update_structure(&mut self, message: Message) -> Update {
        match message {
            Message::Toggle if !self.tabs.is_empty() && !self.tabs.is_preview() => {
                self.structure.preferences.visible = !self.structure.preferences.visible;
                self.structure.show_immediately = self.structure.preferences.visible;
            }
            Message::Ready(ticket, report) => {
                let Some(scan) = self.structure.flight.filter(|scan| scan.ticket == ticket) else {
                    return Update::None;
                };
                self.structure.flight = None;
                if scan.source == self.structure_source() && self.structure_active() {
                    let editor = &mut self.draft.structure;
                    editor
                        .panel
                        .follow_caret(&report, self.draft.content.cursor().position.line);
                    editor.caret_due = None;
                    editor.result = Some((scan, report));
                }
            }
            Message::Panel(message) if self.structure_active() => {
                let result = self
                    .structure_result()
                    .map(|(scan, report)| (scan.ticket, Arc::clone(report)));
                let report = result.as_ref().map(|(_, report)| report.as_ref());
                if let view::Message::Jump {
                    revision,
                    span,
                    focus,
                } = message
                {
                    if result
                        .as_ref()
                        .is_none_or(|(ticket, _)| *ticket != revision)
                        || !structure::select_name(
                            &mut self.draft.content,
                            span.line,
                            span.column..span.end_column,
                        )
                    {
                        return Update::None;
                    }
                    self.draft.structure.caret_due = None;
                    self.draft.structure.panel.update(message, report);
                    if focus {
                        return Update::FocusEditor;
                    }
                } else {
                    self.draft.structure.panel.update(message, report);
                    self.structure.preferences.width =
                        self.draft
                            .structure
                            .panel
                            .width()
                            .round()
                            .clamp(160.0, 1600.0) as u16;
                }
            }
            Message::Toggle | Message::Panel(_) => {}
        }
        Update::None
    }

    pub(super) fn structure_view<'a>(
        &'a self,
        code: impl Fn() -> Element<'a, WindowMessage> + 'a,
    ) -> Element<'a, WindowMessage> {
        if let Some((scan, report)) = self.structure_result() {
            self.draft
                .structure
                .panel
                .view(Some(report), scan.ticket, code)
        } else {
            let previous = self
                .draft
                .structure
                .result
                .as_ref()
                .map(|(_, report)| report.as_ref());
            self.draft.structure.panel.view(previous, 0, code)
        }
    }

    pub(crate) fn focus_editor(&self) -> Task<WindowMessage> {
        widget::operation::focus(self.editor_id.clone())
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt, executor::block_on};
    use iced::widget::text_editor;

    use super::*;
    use crate::snippets::tests::Profile;

    fn ready(task: Task<WindowMessage>) -> WindowMessage {
        let mut stream = iced_runtime::task::into_stream(task).expect("structure scan");
        let Some(iced_runtime::Action::Output(message)) = block_on(stream.next()) else {
            panic!("expected scan reply");
        };
        message
    }

    fn set_source(window: &mut SnippetWindow, source: &str) {
        let _ = window.update(WindowMessage::New);
        let _ = window.update(WindowMessage::Language("rust".into()));
        let _ = window.update(WindowMessage::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new(source.into())),
        )));
    }

    #[test]
    fn structure_waits_for_edits_and_rejects_a_scan_for_replaced_text() {
        let profile = Profile::new();
        let mut window = SnippetWindow::open(profile.path()).unwrap();
        set_source(&mut window, "fn old() {}\n");
        assert!(!window.structure_needs_tick());
        let _ = window.update(WindowMessage::Structure(Message::Toggle));
        let first = ready(window.poll_structure(Instant::now()));
        let _ = window.update(WindowMessage::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new("fn café() {}\n".into())),
        )));
        let edited = window.draft.structure.edited_at.unwrap();
        assert!(!window.structure_needs_tick());
        let _ = window.update(first);
        assert!(window.structure_result().is_none());
        assert!(
            iced_runtime::task::into_stream(
                window.poll_structure(edited + EDIT_DELAY - Duration::from_millis(1))
            )
            .is_none()
        );
        let scanned = ready(window.poll_structure(edited + EDIT_DELAY));
        let _ = window.update(scanned);
        let (scan, report) = window.structure_result().unwrap();
        let node = report
            .analysis
            .nodes
            .iter()
            .find(|node| node.name == "café")
            .unwrap();
        let span = Span {
            line: node.line,
            column: node.column,
            end_column: node.end_column,
        };
        let jump = Message::Panel(view::Message::Jump {
            revision: scan.ticket,
            span,
            focus: true,
        });
        let source = window.draft.content.text();
        assert_eq!(window.update_structure(jump.clone()), Update::FocusEditor);
        assert_eq!(window.draft.content.selection().as_deref(), Some("café"));
        assert_eq!(window.draft.content.text(), source);
        let _ = window.update(WindowMessage::Language("plain".into()));
        assert_eq!(window.update_structure(jump), Update::None);
    }

    #[test]
    fn structure_results_are_scoped_to_the_active_editor_and_variable_revision() {
        let profile = Profile::new();
        let mut window = SnippetWindow::open(profile.path()).unwrap();
        set_source(&mut window, "fn first() {}\n");
        let first_tab = window.tabs.active();
        let _ = window.update(WindowMessage::Structure(Message::Toggle));
        let scan = ready(window.poll_structure(Instant::now()));
        set_source(&mut window, "fn second() { println!(\"${SERVER}\"); }\n");
        let _ = window.update(scan);
        assert!(window.structure_result().is_none());
        let later = Instant::now() + EDIT_DELAY;
        let scan = ready(window.poll_structure(later));
        window.structure.declarations_changed(&[Variable {
            name: "SERVER".into(),
            value: "stored value".into(),
        }]);
        let _ = window.update(scan);
        assert!(window.structure_result().is_none());
        let scan = ready(window.poll_structure(later));
        let _ = window.update(scan);
        let (_, report) = window.structure_result().unwrap();
        assert_eq!(report.analysis.nodes[0].name, "second");
        assert_eq!(
            report.placeholders.entries[0].status,
            resolve::PlaceholderStatus::Stored
        );
        let _ = window.update(WindowMessage::SelectTab(first_tab));
        assert!(window.structure_result().is_none());
        let scan = ready(window.poll_structure(later));
        let _ = window.update(scan);
        assert_eq!(
            window.structure_result().unwrap().1.analysis.nodes[0].name,
            "first"
        );
    }
}
