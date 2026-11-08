use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

const LEGACY_TAGS: [char; 7] = ['Ä', 'Ö', 'Ü', 'ä', 'ö', 'ü', 'ß'];
const KERN: u16 = 64;
const SMUSH: u16 = 128;

macro_rules! styles {
    ($($name:ident => ($label:literal, $file:literal)),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
        pub enum Style {
            #[default]
            Standard,
            $($name),+
        }

        impl Style {
            pub(super) const ALL: [Self; 9] = [Self::Standard, $(Self::$name),+];

            fn font(self) -> Option<&'static Font> {
                match self {
                    Self::Standard => {
                        static FONT: OnceLock<Option<Font>> = OnceLock::new();
                        FONT.get_or_init(|| Font::parse(include_bytes!("fonts/standard.flf"))).as_ref()
                    }
                    $(Self::$name => {
                        static FONT: OnceLock<Option<Font>> = OnceLock::new();
                        FONT.get_or_init(|| Font::parse(include_bytes!($file))).as_ref()
                    }),+
                }
            }
        }

        impl fmt::Display for Style {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(match self {
                    Self::Standard => "Standard",
                    $(Self::$name => $label),+
                })
            }
        }
    };
}

styles! {
    Slant => ("Slant", "fonts/slant.flf"),
    Banner => ("banner", "fonts/banner.flf"),
    Big => ("big", "fonts/big.flf"),
    Block => ("block", "fonts/block.flf"),
    Digital => ("Digital", "fonts/digital.flf"),
    Lean => ("Lean", "fonts/lean.flf"),
    Script => ("script", "fonts/script.flf"),
    Small => ("small", "fonts/small.flf"),
}

struct Font {
    height: usize,
    hardblank: char,
    horizontal: u16,
    glyphs: BTreeMap<char, Vec<Vec<char>>>,
}

impl Font {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let latin1;
        let source = if let Ok(source) = std::str::from_utf8(bytes) {
            source
        } else {
            latin1 = bytes.iter().copied().map(char::from).collect::<String>();
            &latin1
        };
        let mut lines = source.lines();
        let mut header = lines.next()?.strip_prefix("flf2a")?.chars();
        let hardblank = header.next()?;
        let mut fields = header.as_str().split_whitespace();
        let height = fields.next()?.parse().ok()?;
        let mut fields = fields.skip(2);
        let legacy_layout: i16 = fields.next()?.parse().ok()?;
        let comments: usize = fields.next()?.parse().ok()?;
        let horizontal = fields
            .nth(1)
            .and_then(|value| value.parse().ok())
            .unwrap_or(match legacy_layout {
                -1 => 0,
                0 => KERN,
                rules => SMUSH | rules as u16,
            });
        let mut lines = lines.skip(comments).peekable();
        let endmark = lines.peek()?.trim_end().chars().next_back()?;
        let read = |lines: &mut std::iter::Peekable<_>| {
            (0..height)
                .map(|_| {
                    let row: &str = lines.next()?;
                    Some(
                        row.trim_end()
                            .trim_end_matches(endmark)
                            .chars()
                            .collect::<Vec<_>>(),
                    )
                })
                .collect::<Option<Vec<_>>>()
        };

        let mut glyphs = BTreeMap::new();
        for character in (' '..='~').chain(LEGACY_TAGS) {
            glyphs.insert(character, read(&mut lines)?);
        }
        while let Some(tag) = lines.next() {
            let Some(tag) = tag.split_whitespace().next() else {
                continue;
            };
            let character = tag.strip_prefix("0x").map_or_else(
                || tag.parse::<u32>().ok(),
                |hex| u32::from_str_radix(hex, 16).ok(),
            );
            let rows = read(&mut lines)?;
            if let Some(character) = character.and_then(char::from_u32) {
                glyphs.insert(character, rows);
            }
        }
        Some(Self {
            height,
            hardblank,
            horizontal,
            glyphs,
        })
    }

    fn render_line(&self, line: &str) -> String {
        let mut output = vec![Vec::new(); self.height];
        for character in line.chars() {
            let Some(glyph) = self
                .glyphs
                .get(&character)
                .or_else(|| self.glyphs.get(&'?'))
            else {
                continue;
            };
            let width = glyph.first().map_or(0, Vec::len);
            let overlap = if output[0].is_empty() || self.horizontal & (SMUSH | KERN) == 0 {
                0
            } else {
                output
                    .iter()
                    .zip(glyph)
                    .map(|(left, right)| self.overlap(left, right))
                    .min()
                    .unwrap_or_default()
                    .min(width)
                    .min(output[0].len())
            };
            for (left, right) in output.iter_mut().zip(glyph) {
                let count = overlap.min(left.len()).min(right.len());
                let from = left.len() - count;
                for (cell, &next) in left[from..].iter_mut().zip(right) {
                    if let Some(result) = self.merge(*cell, next) {
                        *cell = result;
                    }
                }
                left.extend_from_slice(&right[count..]);
            }
        }
        output
            .into_iter()
            .map(|row| {
                let row: String = row
                    .into_iter()
                    .map(|ch| if ch == self.hardblank { ' ' } else { ch })
                    .collect();
                row.trim_end().to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn overlap(&self, left: &[char], right: &[char]) -> usize {
        let trailing = left.iter().rev().take_while(|&&ch| ch == ' ').count();
        let leading = right.iter().take_while(|&&ch| ch == ' ').count();
        let Some(&a) = left.iter().rev().nth(trailing) else {
            return left.len() + leading;
        };
        let can_merge = right
            .get(leading)
            .is_some_and(|&b| self.merge(a, b).is_some());
        trailing + leading + usize::from(can_merge)
    }

    fn merge(&self, left: char, right: char) -> Option<char> {
        if left == ' ' {
            return Some(right);
        }
        if right == ' ' {
            return Some(left);
        }
        if self.horizontal & SMUSH == 0 {
            return None;
        }
        let rules = self.horizontal & 63;
        if rules == 0 {
            return match (left == self.hardblank, right == self.hardblank) {
                (true, false) => Some(right),
                (false, true) => Some(left),
                (false, false) => Some(right),
                (true, true) => None,
            };
        }
        if left == self.hardblank || right == self.hardblank {
            return (rules & 32 != 0 && left == right).then_some(self.hardblank);
        }
        if rules & 1 != 0 && left == right {
            return Some(left);
        }
        const HIERARCHY: [&str; 6] = ["|", "/\\", "[]", "{}", "()", "<>"];
        let left_class = HIERARCHY.iter().position(|group| group.contains(left));
        let right_class = HIERARCHY.iter().position(|group| group.contains(right));
        if rules & 2 != 0 {
            if left == '_' && right_class.is_some() {
                return Some(right);
            }
            if right == '_' && left_class.is_some() {
                return Some(left);
            }
        }
        if rules & 4 != 0
            && let (Some(a), Some(b)) = (left_class, right_class)
            && a != b
        {
            return Some(if a > b { left } else { right });
        }
        if rules & 8 != 0
            && matches!(
                (left, right),
                ('[', ']') | (']', '[') | ('{', '}') | ('}', '{') | ('(', ')') | (')', '(')
            )
        {
            return Some('|');
        }
        if rules & 16 != 0 {
            return match (left, right) {
                ('/', '\\') => Some('|'),
                ('\\', '/') => Some('Y'),
                ('>', '<') => Some('X'),
                _ => None,
            };
        }
        None
    }
}

pub(super) fn render(style: Style, input: &str) -> String {
    let Some(font) = style.font() else {
        return input.to_owned();
    };
    if input.trim().is_empty() {
        return String::new();
    }
    input
        .lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                font.render_line(line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_art_all_packaged_figfonts_parse_and_render() {
        for style in Style::ALL {
            let font = style
                .font()
                .unwrap_or_else(|| panic!("invalid {style} font"));
            let output = render(style, "Aé\n\nß");
            assert_eq!(output.split('\n').count(), font.height * 2 + 1, "{style}");
            assert!(!output.trim().is_empty());
            assert!(!output.contains(font.hardblank));
        }
    }

    #[test]
    fn ascii_art_standard_font_smushes_and_preserves_blank_source_lines() {
        let font = Style::Standard.font().unwrap();
        let single = font.render_line("H");
        let both = font.render_line("Hi");
        assert!(
            both.lines().next().unwrap().len()
                < font.glyphs[&'H'][0].len() + font.glyphs[&'i'][0].len()
        );
        assert_eq!(
            render(Style::Standard, "H\n\nH"),
            format!("{single}\n\n{single}")
        );
    }
}
