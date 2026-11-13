//! Checked, display-only rule samples for the highlighting editor.

use iced::widget::{container, rich_text, text_editor};
use iced::{Element, Length};
use kortty_terminal::{Color, Terminal};

use crate::terminal_highlight::{Preview as Result, RuleCheck, RuleSet};
use crate::terminal_theme::Appearance;

use super::Desktop;

const MAX_BYTES: usize = 4096;
const MAX_ROWS: u16 = 100;

pub(super) struct Preview {
    pub(super) input: text_editor::Content,
    result: Option<Result>,
    pub(super) warning: Option<&'static str>,
    rows: u16,
    truncated: bool,
}

impl Preview {
    pub(super) fn new() -> Self {
        Self {
            input: text_editor::Content::with_text(
                "error: connection refused\nwarning at 192.0.2.10\nGi1/0/1 down; fe80::1%eth0/64",
            ),
            result: None,
            warning: None,
            rows: 0,
            truncated: false,
        }
    }

    pub(super) fn edit(&mut self, action: text_editor::Action) -> bool {
        if !action.is_edit() {
            self.input.perform(action);
            return false;
        }
        let old = self.input.text();
        if !matches!(&action, text_editor::Action::Edit(text_editor::Edit::Paste(value)) if value.len() > MAX_BYTES)
        {
            self.input.perform(action);
            if self.input.text().len() <= MAX_BYTES + 1 {
                self.warning = None;
                return true;
            }
            self.input = text_editor::Content::with_text(&old);
        }
        self.warning = Some("Test text is limited to 4 KiB.");
        false
    }

    pub(super) fn refresh(&mut self, set: Option<&RuleSet>) -> Option<String> {
        let Some(set) = set else {
            self.result = None;
            return None;
        };
        let source = self.input.text();
        let mut terminal = Terminal::new(80, MAX_ROWS);
        let mut lines = source.split_terminator('\n');
        for (index, line) in lines.by_ref().take(usize::from(MAX_ROWS)).enumerate() {
            if index != 0 {
                terminal.advance(b"\r\n");
            }
            let printable: String = line
                .chars()
                .filter(|&character| character == '\t' || !character.is_control())
                .collect();
            terminal.advance(printable.as_bytes());
        }
        let current = terminal.snapshot();
        self.truncated = lines.next().is_some() || current.scrollback_lines != 0;
        self.rows = if current.scrollback_lines != 0 {
            MAX_ROWS
        } else {
            current.cursor.row + 1
        };
        let screen = if current.scrollback_lines != 0 {
            terminal.viewport_snapshot(current.scrollback_lines)
        } else {
            current
        };
        let result = set.preview(&screen);
        let problem = result.problem.clone();
        self.result = Some(result);
        problem
    }

    pub(super) fn rule(&self, index: usize) -> Option<&RuleCheck> {
        self.result.as_ref()?.rules.get(index)
    }

    pub(super) fn truncated(&self) -> bool {
        self.truncated
    }

    pub(super) fn view(&self, appearance: Appearance) -> Option<Element<'static, Desktop>> {
        use iced::widget::text::Span;

        let screen = &self.result.as_ref()?.screen;
        let display_color = |color| {
            let rgb = match color {
                Color::Default => return None,
                Color::Indexed(index) => appearance.palette.rgb(index),
                Color::Rgb(r, g, b) => [r, g, b],
            };
            Some(iced::Color::from_rgb8(rgb[0], rgb[1], rgb[2]))
        };
        let [r, g, b] = appearance.foreground.bytes();
        let foreground = iced::Color::from_rgb8(r, g, b);
        let [r, g, b] = appearance.background.bytes();
        let background = iced::Color::from_rgb8(r, g, b);
        let spans = screen
            .cells
            .chunks(usize::from(screen.columns))
            .take(usize::from(self.rows))
            .flat_map(|row| {
                row.iter()
                    .filter(|cell| cell.width > 0)
                    .map(|cell| {
                        Span::<()>::new(cell.text.clone())
                            .color(display_color(cell.style.foreground).unwrap_or(foreground))
                            .background_maybe(display_color(cell.style.background))
                            .underline(cell.style.underline)
                            .font(iced::Font {
                                weight: if cell.style.bold {
                                    iced::font::Weight::Bold
                                } else {
                                    iced::font::Weight::Normal
                                },
                                style: if cell.style.italic {
                                    iced::font::Style::Italic
                                } else {
                                    iced::font::Style::Normal
                                },
                                ..iced::Font::MONOSPACE
                            })
                    })
                    .chain(std::iter::once(Span::new("\n")))
            })
            .collect::<Vec<_>>();
        Some(
            container(rich_text(spans).size(14))
                .width(Length::Fill)
                .padding(7)
                .style(move |_| container::Style::default().background(background))
                .into(),
        )
    }
}
