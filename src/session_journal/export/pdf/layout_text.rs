use crate::session_journal::is_display_character;

use super::document::Document;
use super::layout::Style;

pub(super) fn fit(document: &Document, style: Style, width: f32, value: &str) -> String {
    let mut line = value
        .chars()
        .filter(|character| is_display_character(*character))
        .map(|character| {
            if character.is_whitespace() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    if measure(document, style, &line) > width {
        let available = width - measure(document, style, "...");
        if available < 0.0 {
            return String::new();
        }
        let end = fitting_end(document, style, available, &line);
        line.truncate(line[..end].trim_end().len());
        line.push_str("...");
    }
    line
}

pub(super) fn prose_lines(
    document: &Document,
    style: Style,
    width: f32,
    value: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    for source in value.split(['\n', '\u{0085}', '\u{2028}', '\u{2029}']) {
        let mut line = String::new();
        let mut used = 0.0;
        let space = measure(document, style, " ");
        let source = source
            .chars()
            .filter(|character| is_display_character(*character))
            .collect::<String>();
        for mut word in source.split_whitespace() {
            let word_width = measure(document, style, word);
            if !line.is_empty() && used + space + word_width > width {
                lines.push(std::mem::take(&mut line));
                used = 0.0;
            }
            if !line.is_empty() {
                line.push(' ');
                used += space;
            }
            if word_width > width {
                loop {
                    let end = fitting_end(document, style, width, word);
                    if end == word.len() {
                        break;
                    }
                    let end = end.max(word.chars().next().map_or(0, char::len_utf8));
                    lines.push(word[..end].to_owned());
                    word = &word[end..];
                }
            }
            line.push_str(word);
            used += measure(document, style, word);
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub(super) fn terminal_lines(
    document: &Document,
    style: Style,
    width: f32,
    prefix: &str,
    value: &str,
) -> Vec<String> {
    let mut output = Vec::new();
    let mut line = prefix.to_owned();
    let mut used = measure(document, style, &line);
    for character in value
        .chars()
        .filter(|character| is_display_character(*character))
    {
        let mut buffer = [0; 4];
        let glyph = if character == '\t' {
            "    "
        } else {
            character.encode_utf8(&mut buffer)
        };
        let advance = measure(document, style, glyph);
        if used + advance > width && !line.is_empty() {
            output.push(std::mem::take(&mut line));
            used = 0.0;
        }
        line.push_str(glyph);
        used += advance;
    }
    output.push(line);
    output
}

fn fitting_end(document: &Document, style: Style, width: f32, value: &str) -> usize {
    let mut used = 0.0;
    for (offset, character) in value.char_indices() {
        used += measure(document, style, character.encode_utf8(&mut [0; 4]));
        if used > width {
            return offset;
        }
    }
    value.len()
}

fn measure(document: &Document, style: Style, value: &str) -> f32 {
    document.text_width(style.font, style.size, value)
}
