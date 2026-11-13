use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use iced::widget::text_editor::{self, Action, Edit, LineEnding};

use super::MAX_FILE_BYTES;

const TOO_LARGE: &str = "Converting line endings would exceed the editor size limit.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Conversion {
    Lf,
    CrLf,
}

impl Conversion {
    pub(super) const ALL: [Self; 2] = [Self::Lf, Self::CrLf];

    const fn ending(self) -> LineEnding {
        match self {
            Self::Lf => LineEnding::Lf,
            Self::CrLf => LineEnding::CrLf,
        }
    }
}

impl fmt::Display for Conversion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Lf => "LF (Unix/macOS/Linux)",
            Self::CrLf => "CRLF (Windows)",
        })
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct State {
    // None means mixed or empty. The line count disambiguates them.
    uniform: Option<LineEnding>,
    preferred: Option<Conversion>,
}

impl State {
    pub(super) fn from_text(text: &str) -> Self {
        Self {
            uniform: uniform(separators(text).map(|(_, ending)| ending)),
            preferred: None,
        }
    }

    pub(super) fn update_from_text(&mut self, text: &str) {
        self.uniform = uniform(separators(text).map(|(_, ending)| ending));
    }

    pub(super) fn choose(&mut self, conversion: Conversion) {
        self.preferred = Some(conversion);
    }

    pub(super) fn selected(self, line_count: usize) -> Option<Conversion> {
        match self.uniform {
            Some(LineEnding::Lf) => Some(Conversion::Lf),
            Some(LineEnding::CrLf) => Some(Conversion::CrLf),
            None if line_count <= 1 => Some(self.preferred.unwrap_or(Conversion::Lf)),
            _ => None,
        }
    }

    pub(super) fn label(self, line_count: usize) -> &'static str {
        match self.selected(line_count) {
            Some(Conversion::Lf) => "LF (Unix/macOS/Linux)",
            Some(Conversion::CrLf) => "CRLF (Windows)",
            None if self.uniform == Some(LineEnding::Cr) => "CR (Classic Mac)",
            None => "Mixed",
        }
    }

    pub(super) fn prepare_edit(self, action: Action) -> Result<Action, &'static str> {
        match action {
            Action::Edit(Edit::Enter | Edit::Insert('\n')) => {
                let ending = self.preferred.map(Conversion::ending).or(self.uniform);
                match ending {
                    Some(ending @ (LineEnding::CrLf | LineEnding::Cr)) => {
                        Ok(Action::Edit(Edit::Paste(Arc::new(ending.as_str().into()))))
                    }
                    _ => Ok(action),
                }
            }
            Action::Edit(Edit::Paste(ref source)) => {
                if let Some(text) = self.convert_inserted(source)? {
                    Ok(Action::Edit(Edit::Paste(Arc::new(text))))
                } else {
                    Ok(action)
                }
            }
            _ => Ok(action),
        }
    }

    pub(super) fn convert_inserted(self, source: &str) -> Result<Option<String>, &'static str> {
        self.preferred
            .map_or(Ok(None), |preferred| convert(source, preferred))
    }

    pub(super) fn convert_formatted(
        self,
        source: &str,
        line_count: usize,
    ) -> Result<Option<String>, &'static str> {
        self.preferred
            .or_else(|| self.selected(line_count))
            .map_or(Ok(None), |ending| convert(source, ending))
    }

    pub(super) fn observe_edit(
        &mut self,
        content: &text_editor::Content,
        old_count: usize,
        selected_lines: bool,
        pasted_endings: bool,
    ) {
        let count = content.line_count();
        if count <= 1 {
            self.uniform = None;
        } else if selected_lines
            || pasted_endings
            || (count != old_count && (old_count <= 1 || self.uniform.is_none()))
        {
            // Content::text supplies LF if an interior line carries no explicit ending.
            self.uniform = uniform(
                content
                    .lines()
                    .take(count - 1)
                    .map(|line| between(line.ending)),
            );
        }
    }
}

pub(super) struct Converted {
    pub(super) original: String,
    pub(super) text: String,
}

/// Uses the editor's actual line boundaries, including adjacent CR and LF edits.
pub(super) fn convert_content(
    content: &text_editor::Content,
    conversion: Conversion,
) -> Result<Option<Converted>, &'static str> {
    let count = content.line_count();
    let target = conversion.ending().as_str();
    let mut original = String::new();
    let mut text = String::new();
    let mut changed = false;
    for (index, line) in content.lines().enumerate() {
        let last = index + 1 == count;
        let source = if last {
            ""
        } else {
            between(line.ending).as_str()
        };
        let ending = if last { "" } else { target };
        if text.len() + line.text.len() + ending.len() > MAX_FILE_BYTES {
            return Err(TOO_LARGE);
        }
        original.push_str(&line.text);
        original.push_str(source);
        text.push_str(&line.text);
        text.push_str(ending);
        changed |= source != ending;
    }
    Ok(changed.then_some(Converted { original, text }))
}

fn convert(source: &str, conversion: Conversion) -> Result<Option<String>, &'static str> {
    let target = conversion.ending();
    let mut bytes = source.len();
    let mut changed = false;
    for (range, ending) in separators(source) {
        bytes = bytes - range.len() + target.as_str().len();
        changed |= ending != target;
    }
    if bytes > MAX_FILE_BYTES {
        return Err(TOO_LARGE);
    }
    if !changed {
        return Ok(None);
    }
    let mut converted = String::with_capacity(bytes);
    let mut previous = 0;
    for (range, _) in separators(source) {
        converted.push_str(&source[previous..range.start]);
        converted.push_str(target.as_str());
        previous = range.end;
    }
    converted.push_str(&source[previous..]);
    Ok(Some(converted))
}

fn between(ending: LineEnding) -> LineEnding {
    if ending == LineEnding::None {
        LineEnding::Lf
    } else {
        ending
    }
}

fn uniform(mut endings: impl Iterator<Item = LineEnding>) -> Option<LineEnding> {
    let first = endings.next()?;
    (first != LineEnding::LfCr && endings.all(|ending| ending == first)).then_some(first)
}

fn separators(source: &str) -> impl Iterator<Item = (Range<usize>, LineEnding)> + '_ {
    let mut offset = 0;
    std::iter::from_fn(move || {
        let start = offset + source[offset..].find(['\r', '\n'])?;
        let ending = match &source.as_bytes()[start..] {
            [b'\r', b'\n', ..] => LineEnding::CrLf,
            [b'\n', b'\r', ..] => LineEnding::LfCr,
            [b'\r', ..] => LineEnding::Cr,
            _ => LineEnding::Lf,
        };
        offset = start + ending.as_str().len();
        Some((start..offset, ending))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_line_ending_conversion_checks_size_and_keeps_the_final_separator() {
        assert_eq!(
            convert("first\rsecond\r", Conversion::Lf)
                .unwrap()
                .as_deref(),
            Some("first\nsecond\n")
        );
        assert_eq!(State::from_text("old\rformat").label(2), "CR (Classic Mac)");
        assert_eq!(State::from_text("first\n\rsecond").label(2), "Mixed");
        let source = format!("{}\n", "x".repeat(MAX_FILE_BYTES - 1));
        assert!(matches!(convert(&source, Conversion::CrLf), Err(TOO_LARGE)));
    }
}
