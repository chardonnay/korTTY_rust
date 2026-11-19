use std::fmt;
use std::sync::{Arc, Weak};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Alignment, Element, Task, window};

use crate::editor_window::caret::Shape;
use crate::snippet_analysis::model::{DEFAULT_CONTENT_BYTES, valid_content_limit};
use crate::snippet_analysis::store::{
    DEFAULT_HISTORY_LIMIT, HISTORY_LIMITS, RetentionPreview, Store,
};
use crate::snippet_window::ai_assist::{ALTERNATIVE_COUNTS, DEFAULT_ALTERNATIVE_COUNT};
use crate::snippet_window::appearance::{MAX_FONT_SIZE, Preferences};
use crate::terminal_theme::{Rgb, installed_families, installed_font};
use crate::workspace::SnippetCompletionChord;

use super::{Desktop, Dialog, Message, Setting, highlighted};

#[derive(Clone, Copy, Debug)]
pub enum Color {
    Foreground,
    Background,
    Cursor,
}

impl Color {
    fn setting(self) -> Setting {
        match self {
            Self::Foreground => Setting::SnippetForeground,
            Self::Background => Setting::SnippetBackground,
            Self::Cursor => Setting::SnippetCursorColor,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Edit {
    Family(Option<String>),
    Size(String),
    CursorStyle(Option<Shape>),
    LineNumbers(bool),
    WordWrap(bool),
    Color(Color, String),
}

#[derive(Default)]
pub(super) struct Editor {
    size: Option<String>,
    foreground: Option<String>,
    background: Option<String>,
    cursor: Option<String>,
    content_limit: Option<String>,
    history_preview: Option<HistoryPreview>,
    recording_shortcut: bool,
    shortcut_problem: Option<&'static str>,
}

enum HistoryPreview {
    Counting(Arc<()>),
    Ready(RetentionPreview),
    Unavailable,
}

impl Editor {
    pub(super) fn recording_shortcut(&self) -> bool {
        self.recording_shortcut
    }

    pub(super) fn stop_recording_shortcut(&mut self) {
        self.recording_shortcut = false;
        self.shortcut_problem = None;
    }

    pub(super) fn record_shortcut(&mut self) {
        self.recording_shortcut = true;
        self.shortcut_problem = None;
    }

    pub(super) fn capture_shortcut(
        &mut self,
        saved: &mut Option<SnippetCompletionChord>,
        key: &Key,
        modifiers: Modifiers,
    ) -> bool {
        if !self.recording_shortcut {
            return false;
        }
        match key {
            Key::Named(Named::Escape) => self.stop_recording_shortcut(),
            Key::Named(
                Named::Alt
                | Named::AltGraph
                | Named::Control
                | Named::Shift
                | Named::Meta
                | Named::Super,
            ) => {}
            _ => match SnippetCompletionChord::capture(key, modifiers) {
                Some(chord) => {
                    *saved = Some(chord);
                    self.stop_recording_shortcut();
                }
                None => {
                    self.shortcut_problem = Some("This key cannot be used for snippet completion.")
                }
            },
        }
        true
    }

    pub(super) fn update_content_limit(&mut self, saved: &mut Option<usize>, mut value: String) {
        if value.len() <= 16
            && let Ok(limit) = value.parse::<usize>()
            && valid_content_limit(limit)
        {
            *saved = Some(limit);
            self.content_limit = None;
        } else {
            value.truncate(value.floor_char_boundary(16));
            self.content_limit = Some(value);
        }
    }

    pub(super) fn preview_history_limit(
        &mut self,
        id: window::Id,
        initial: u8,
        limit: u8,
    ) -> Task<Desktop> {
        self.history_preview = None;
        if limit >= initial {
            return Task::none();
        }
        let token = Arc::new(());
        let ticket = Arc::downgrade(&token);
        let worker_ticket = ticket.clone();
        self.history_preview = Some(HistoryPreview::Counting(token));
        Task::perform(
            async move {
                if worker_ticket.strong_count() == 0 {
                    return None;
                }
                let (send, receive) = async_channel::bounded(1);
                std::thread::Builder::new()
                    .name("kortty-analysis-retention-preview".into())
                    .spawn(move || {
                        let count = || {
                            let path = crate::snippets::profile_path().ok()?;
                            let store = Store::new(path.parent()?.to_path_buf());
                            let library = crate::snippets::Library::open(path).ok()?;
                            store
                                .preview_retention(&library, limit, || {
                                    worker_ticket.strong_count() != 0
                                })
                                .ok()
                        };
                        if worker_ticket.strong_count() != 0 {
                            let _ = send.send_blocking(count());
                        }
                    })
                    .ok()?;
                receive.recv().await.ok().flatten()
            },
            move |result| {
                Desktop::Settings(Message::SnippetAnalysisHistoryPreview(
                    id,
                    ticket.clone(),
                    result,
                ))
            },
        )
    }

    pub(super) fn finish_history_preview(
        &mut self,
        ticket: Weak<()>,
        result: Option<RetentionPreview>,
    ) {
        if let Some(HistoryPreview::Counting(current)) = &self.history_preview
            && ticket.ptr_eq(&Arc::downgrade(current))
        {
            self.history_preview =
                Some(result.map_or(HistoryPreview::Unavailable, HistoryPreview::Ready));
        }
    }

    fn history_preview_text(&self) -> Option<String> {
        Some(match self.history_preview.as_ref()? {
            HistoryPreview::Counting(_) => "Counting reports affected by this limit…".into(),
            HistoryPreview::Ready(preview) if preview.reports == 0 => {
                "No reports would be removed at this limit.".into()
            }
            HistoryPreview::Ready(preview) => {
                let reports = preview.reports;
                let subjects = preview.subjects;
                let report_label = if reports == 1 { "report" } else { "reports" };
                let subject_label = if subjects == 1 {
                    "snippet or folder"
                } else {
                    "snippets or folders"
                };
                format!(
                    "{reports} saved {report_label} would be removed across {subjects} {subject_label} as their next analyses are saved."
                )
            }
            HistoryPreview::Unavailable => "Could not count affected reports.".into(),
        })
    }

    pub(super) fn update(&mut self, preferences: &mut Preferences, edit: Edit) {
        match edit {
            Edit::Family(family) => {
                if family
                    .as_deref()
                    .is_none_or(|name| installed_font(name).is_some())
                {
                    preferences.font_family = family;
                }
            }
            Edit::Size(mut value) => {
                value.truncate(value.floor_char_boundary(3));
                let size = if value.is_empty() {
                    Some(0)
                } else {
                    value.parse::<u8>().ok()
                };
                if let Some(size) = size.filter(|&size| size <= MAX_FONT_SIZE) {
                    preferences.font_size = size;
                    self.size = None;
                } else {
                    self.size = Some(value);
                }
            }
            Edit::CursorStyle(style) => preferences.cursor_style = style,
            Edit::LineNumbers(enabled) => preferences.line_numbers = Some(enabled),
            Edit::WordWrap(enabled) => preferences.word_wrap = Some(enabled),
            Edit::Color(channel, mut value) => {
                value.truncate(value.floor_char_boundary(12));
                let (color, input) = match channel {
                    Color::Foreground => (&mut preferences.foreground, &mut self.foreground),
                    Color::Background => (&mut preferences.background, &mut self.background),
                    Color::Cursor => (&mut preferences.cursor_color, &mut self.cursor),
                };
                if value.trim().is_empty() {
                    *color = None;
                    *input = None;
                } else if let Some(value) = Rgb::parse(&value) {
                    *color = Some(value);
                    *input = None;
                } else {
                    *input = Some(value);
                }
            }
        }
    }

    pub(super) fn first_problem(&self) -> Option<Setting> {
        [
            (Setting::SnippetFontSize, &self.size),
            (Setting::SnippetForeground, &self.foreground),
            (Setting::SnippetBackground, &self.background),
            (Setting::SnippetCursorColor, &self.cursor),
            (Setting::SnippetAnalysisContentLimit, &self.content_limit),
        ]
        .into_iter()
        .find_map(|(setting, value)| value.is_some().then_some(setting))
    }
}

#[derive(Clone, PartialEq)]
struct Family(Option<String>);

impl fmt::Display for Family {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.as_deref().unwrap_or("Inherit terminal font"))
    }
}

#[derive(Clone, Copy, PartialEq)]
struct CursorStyle(Option<Shape>);

impl fmt::Display for CursorStyle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(shape) => fmt::Display::fmt(&shape, formatter),
            None => formatter.write_str("Inherit"),
        }
    }
}

pub(super) fn view(dialog: &Dialog, managed_content_limit: Option<usize>) -> Element<'_, Desktop> {
    let id = dialog.id;
    let preferences = &dialog.draft.preferences.snippet_appearance;
    let editor = &dialog.snippet_appearance;
    let edit = move |edit| Desktop::Settings(Message::SnippetAppearance(id, edit));
    let families = std::iter::once(Family(None))
        .chain(
            installed_families()
                .iter()
                .cloned()
                .map(|name| Family(Some(name))),
        )
        .collect::<Vec<_>>();
    let cursor_styles = std::iter::once(CursorStyle(None))
        .chain(Shape::ALL.map(|shape| CursorStyle(Some(shape))))
        .collect::<Vec<_>>();
    let size = preferences.font_size.to_string();
    let color_row = |channel: Color, color: Option<Rgb>, input: Option<&str>| {
        let setting = channel.setting();
        let color = color.map(|color| color.to_string());
        let current = input.or(color.as_deref()).unwrap_or_default();
        highlighted(
            dialog,
            setting,
            column![
                row![
                    text(setting.label()).width(150),
                    dialog.reveal_input(
                        setting,
                        true,
                        text_input("Inherit", current)
                            .width(120)
                            .on_input(move |value| edit(Edit::Color(channel, value))),
                    ),
                    button("Inherit").style(button::secondary).on_press_maybe(
                        (color.is_some() || input.is_some())
                            .then(|| edit(Edit::Color(channel, String::new())))
                    ),
                ]
                .align_y(Alignment::Center)
                .spacing(8),
                input.map(|_| text("Enter a color as #RRGGBB.")
                    .size(13)
                    .style(text::danger)),
            ]
            .spacing(4),
        )
    };
    let appearance = dialog.draft.preferences.snippet_editor_appearance();
    let content_limit = dialog
        .draft
        .preferences
        .snippet_analysis_content_limit
        .unwrap_or(DEFAULT_CONTENT_BYTES)
        .to_string();
    let storage_locked = managed_content_limit == Some(0);
    let displayed_limit = if storage_locked {
        "0"
    } else {
        editor.content_limit.as_deref().unwrap_or(&content_limit)
    };
    column![
        text("Snippet Editor Appearance").size(18),
        text("These settings apply to snippet editors and previews. Inherited fonts and text colors follow the terminal. Inherited cursor settings follow the file editor.").size(13),
        highlighted(dialog, Setting::SnippetFontFamily, row![
            text(Setting::SnippetFontFamily.label()).width(150),
            pick_list(families, Some(Family(preferences.font_family.clone())), move |family| edit(Edit::Family(family.0)))
                .width(240),
        ].align_y(Alignment::Center).spacing(8)),
        highlighted(dialog, Setting::SnippetFontSize, column![
            row![
                text(Setting::SnippetFontSize.label()).width(150),
                dialog.reveal_input(
                    Setting::SnippetFontSize,
                    true,
                    text_input("0–72", editor.size.as_deref().unwrap_or(&size)).width(80).on_input(move |value| edit(Edit::Size(value))),
                ),
                text("0 inherits").size(13),
            ].align_y(Alignment::Center).spacing(8),
            editor.size.as_ref().map(|_| text("Enter a font size from 0 to 72.").size(13).style(text::danger)),
        ].spacing(4)),
        color_row(Color::Foreground, preferences.foreground, editor.foreground.as_deref()),
        color_row(Color::Background, preferences.background, editor.background.as_deref()),
        highlighted(dialog, Setting::SnippetCursorStyle, row![
            text(Setting::SnippetCursorStyle.label()).width(150),
            pick_list(cursor_styles, Some(CursorStyle(preferences.cursor_style)), move |style| edit(Edit::CursorStyle(style.0)))
                .width(160),
        ].align_y(Alignment::Center).spacing(8)),
        color_row(Color::Cursor, preferences.cursor_color, editor.cursor.as_deref()),
        highlighted(dialog, Setting::SnippetLineNumbers,
            checkbox(preferences.line_numbers.unwrap_or_default())
                .label(Setting::SnippetLineNumbers.label())
                .on_toggle(move |enabled| edit(Edit::LineNumbers(enabled))),
        ),
        highlighted(dialog, Setting::SnippetWordWrap,
            checkbox(preferences.word_wrap.unwrap_or(true))
                .label(Setting::SnippetWordWrap.label())
                .on_toggle(move |enabled| edit(Edit::WordWrap(enabled))),
        ),
        text("Preview").size(16),
        container(text("AaBbCcDdEe 0123456789")
            .font(appearance.font)
            .size(f32::from(appearance.size))
            .color(appearance.foreground.color()))
            .padding(12)
            .style(move |_| container::Style::default().background(appearance.background.color())),
        highlighted(dialog, Setting::SnippetAnalysisHistory, column![
            row![
                text(Setting::SnippetAnalysisHistory.label()),
                pick_list(
                    HISTORY_LIMITS.collect::<Vec<_>>(),
                    Some(dialog.draft.preferences.snippet_analysis_history_limit.unwrap_or(DEFAULT_HISTORY_LIMIT)),
                    move |limit| Desktop::Settings(Message::SnippetAnalysisHistory(id, limit)),
                ),
            ].align_y(Alignment::Center).spacing(8),
            text("The limit applies when a new report is saved. Pinned reports and records needed for recovery are kept.").size(13),
            editor.history_preview_text().map(|preview| text(preview).size(13)),
        ].spacing(4)),
        highlighted(dialog, Setting::SnippetVersionHistory, row![
            text(Setting::SnippetVersionHistory.label()),
            pick_list(
                crate::snippets::history::LIMITS.collect::<Vec<_>>(),
                Some(dialog.draft.preferences.snippet_history_limit.unwrap_or(crate::snippets::history::DEFAULT_LIMIT)),
                move |limit| Desktop::Settings(Message::SnippetVersionHistory(id, limit)),
            ),
        ].align_y(Alignment::Center).spacing(8)),
        highlighted(dialog, Setting::SnippetAlternativeCount, row![
            text(Setting::SnippetAlternativeCount.label()),
            pick_list(
                ALTERNATIVE_COUNTS.collect::<Vec<_>>(),
                Some(dialog.draft.preferences.snippet_alternative_count.unwrap_or(DEFAULT_ALTERNATIVE_COUNT)),
                move |count| Desktop::Settings(Message::SnippetAlternativeCount(id, count)),
            ),
        ].align_y(Alignment::Center).spacing(8)),
        highlighted(dialog, Setting::SnippetCompletionShortcut, column![
            row![
                text(Setting::SnippetCompletionShortcut.label()),
                text(dialog.draft.preferences.snippet_completion_shortcut.unwrap_or_default().to_string()),
                button(if editor.recording_shortcut { "Press a shortcut…" } else { "Record…" })
                    .on_press(Desktop::Settings(Message::RecordSnippetCompletionShortcut(id))),
                button("Reset").on_press(Desktop::Settings(Message::ResetSnippetCompletionShortcut(id))),
            ].align_y(Alignment::Center).spacing(8).wrap(),
            editor.shortcut_problem.map(|problem| text(problem).size(13).style(text::danger)),
        ].spacing(4)),
        highlighted(dialog, Setting::SnippetAiAdditionalInstructions,
            checkbox(dialog.draft.preferences.snippet_ai_additional_instructions())
                .label(Setting::SnippetAiAdditionalInstructions.label())
                .on_toggle(move |enabled| Desktop::Settings(Message::SnippetAiAdditionalInstructions(id, enabled)))
        ),
        highlighted(dialog, Setting::SnippetAnalysisContentLimit, column![
            row![
                text(Setting::SnippetAnalysisContentLimit.label()),
                dialog.reveal_input(
                    Setting::SnippetAnalysisContentLimit,
                    !storage_locked,
                    text_input("1048576", displayed_limit)
                        .on_input_maybe((!storage_locked).then_some(move |value| {
                            Desktop::Settings(Message::SnippetAnalysisContentLimit(id, value))
                        }))
                        .width(130),
                ),
            ].align_y(Alignment::Center).spacing(8),
            text("Use 0 to omit new source text, or enter 262144 to 5242880 bytes. Lowering your limit preserves earlier source snapshots.").size(13),
            managed_content_limit.map(|limit| text(if limit == 0 {
                "Managed policy disables source storage.".to_owned()
            } else {
                format!("Managed policy permits up to {limit} bytes per field.")
            }).size(13)),
            editor.content_limit.as_ref().map(|_| text("Enter 0 or a size from 262144 to 5242880 bytes.").size(13).style(text::danger)),
        ].spacing(4)),
    ].spacing(10).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_limit_rejects_out_of_range_input_and_keeps_explicit_zero() {
        let mut editor = Editor::default();
        let mut limit = None;
        for input in ["1", "5242881", "00000000000000001"] {
            editor.update_content_limit(&mut limit, input.into());
            assert_eq!(limit, None);
            assert_eq!(
                editor.first_problem(),
                Some(Setting::SnippetAnalysisContentLimit)
            );
        }
        editor.update_content_limit(&mut limit, "262145".into());
        assert_eq!(limit, Some(262145));
        assert!(editor.first_problem().is_none());
        editor.update_content_limit(&mut limit, "0".into());
        assert_eq!(limit, Some(0));
        assert!(editor.first_problem().is_none());
    }

    #[test]
    fn a_superseded_history_preview_cannot_change_the_visible_count() {
        let id = window::Id::unique();
        let mut editor = Editor::default();
        let _ = editor.preview_history_limit(id, DEFAULT_HISTORY_LIMIT, 4);
        let Some(HistoryPreview::Counting(first)) = &editor.history_preview else {
            panic!("expected the first estimate");
        };
        let first = Arc::downgrade(first);
        let _ = editor.preview_history_limit(id, DEFAULT_HISTORY_LIMIT, 3);
        let Some(HistoryPreview::Counting(second)) = &editor.history_preview else {
            panic!("expected a replacement estimate");
        };
        let second = Arc::downgrade(second);
        assert_eq!(first.strong_count(), 0);
        editor.finish_history_preview(first, Some(RetentionPreview::default()));
        assert!(matches!(
            editor.history_preview,
            Some(HistoryPreview::Counting(_))
        ));
        editor.finish_history_preview(
            second,
            Some(RetentionPreview {
                reports: 3,
                subjects: 2,
            }),
        );
        assert!(
            editor
                .history_preview_text()
                .unwrap()
                .contains("3 saved reports")
        );
        let _ = editor.preview_history_limit(id, DEFAULT_HISTORY_LIMIT, DEFAULT_HISTORY_LIMIT);
        assert!(editor.history_preview_text().is_none());
    }
}
