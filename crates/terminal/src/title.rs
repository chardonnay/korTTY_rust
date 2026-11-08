use crate::is_bidi_control;

const MAX_LENGTH: usize = 80;
const RAW_SCAN_LIMIT: usize = MAX_LENGTH * 8;

pub(crate) fn normalize(payload: &str) -> String {
    let mut units = 0;
    let mut clean = String::new();
    for character in payload.chars() {
        units += character.len_utf16();
        if units > RAW_SCAN_LIMIT {
            break;
        }
        match character {
            '\t'..='\r' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                if !clean.ends_with(' ') {
                    clean.push(' ');
                }
            }
            character if !character.is_control() && !is_bidi_control(character) => {
                clean.push(character);
            }
            _ => {}
        }
    }
    let mut title: String = clean
        .trim_matches(title_whitespace)
        .chars()
        .take(MAX_LENGTH)
        .collect();
    title.truncate(title.trim_end_matches(title_whitespace).len());
    title
}

fn title_whitespace(character: char) -> bool {
    // Java String.strip() leaves the three non-breaking spaces intact.
    character.is_whitespace() && !matches!(character, '\u{a0}' | '\u{2007}' | '\u{202f}')
}
