//! Structural CSS context for completion in an unfinished stylesheet.

#[derive(Clone, Copy, Default)]
struct Block<'a> {
    rule: &'a str,
    style: bool,
    declarations: bool,
}

impl<'a> Block<'a> {
    fn open(head: &'a str, parent: Self) -> Self {
        if !head.starts_with('@') {
            return Self {
                style: true,
                declarations: true,
                ..Self::default()
            };
        }
        let rule = head
            .split(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '(' | '/'))
            .next()
            .unwrap_or_default();
        let lower = rule.to_ascii_lowercase();
        let style = parent.style
            && matches!(
                lower.as_str(),
                "@container" | "@layer" | "@media" | "@scope" | "@starting-style" | "@supports"
            );
        let declarations = style
            || !matches!(
                lower.as_str(),
                "@container"
                    | "@font-feature-values"
                    | "@keyframes"
                    | "@layer"
                    | "@media"
                    | "@scope"
                    | "@starting-style"
                    | "@supports"
            );
        Self {
            rule,
            style,
            declarations,
        }
    }
}

#[derive(Default)]
struct Entry<'a> {
    start: Option<usize>,
    colon: bool,
    property: Option<&'a str>,
}

struct Component {
    close: u8,
    variable_name: bool,
}

pub(super) struct Context<'a> {
    pub(super) rule: &'a str,
    pub(super) declarations: bool,
    pub(super) colon: bool,
    pub(super) property: Option<&'a str>,
    pub(super) variable_name: bool,
}

pub(super) fn scan<'a>(
    css: &'a str,
    mut custom_property: impl FnMut(&'a str),
) -> Option<Context<'a>> {
    let mut blocks = Vec::<Block<'_>>::new();
    let mut components = Vec::<Component>::new();
    let mut entry = Entry::default();
    let mut cursor = 0;
    let bytes = css.as_bytes();
    while let Some(&byte) = bytes.get(cursor) {
        if bytes[cursor..].starts_with(b"/*") {
            cursor += 4 + css[cursor + 2..].find("*/")?;
            continue;
        }
        if byte.is_ascii_whitespace() {
            cursor += 1;
            continue;
        }
        let start = *entry.start.get_or_insert(cursor);
        let block = blocks.last().copied().unwrap_or_default();
        match byte {
            b'"' | b'\'' => cursor = quoted_end(bytes, cursor, byte)?,
            b'\\' => {
                // An escaped delimiter is part of the token.
                cursor += usize::from(cursor + 1 < bytes.len());
            }
            b'(' => {
                let function = css
                    .get(start..cursor)
                    .unwrap_or_default()
                    .rsplit(|ch: char| ch.is_whitespace() || !name_character(ch))
                    .next()
                    .unwrap_or_default();
                components.push(Component {
                    close: b')',
                    variable_name: function.eq_ignore_ascii_case("var"),
                });
            }
            b'[' => components.push(Component {
                close: b']',
                variable_name: false,
            }),
            b'{' if !components.is_empty()
                || entry.property.is_some_and(|name| name.starts_with("--")) =>
            {
                components.push(Component {
                    close: b'}',
                    variable_name: false,
                });
            }
            b')' | b']' | b'}' if components.last().is_some_and(|item| item.close == byte) => {
                components.pop();
            }
            b',' if !components.is_empty() => {
                components
                    .last_mut()
                    .expect("active CSS component")
                    .variable_name = false;
            }
            _ if !components.is_empty() => {}
            b'{' => {
                blocks.push(Block::open(
                    css.get(start..cursor).unwrap_or_default(),
                    block,
                ));
                entry = Entry::default();
            }
            b'}' => {
                blocks.pop();
                entry = Entry::default();
            }
            b';' => entry = Entry::default(),
            b':' if !entry.colon => {
                entry.colon = true;
                if block.declarations {
                    entry.property = css.get(start..cursor).and_then(declaration_name);
                    if let Some(name) = entry.property.filter(|name| name.starts_with("--")) {
                        custom_property(name);
                    }
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    let block = blocks.last().copied().unwrap_or_default();
    Some(Context {
        rule: block.rule,
        declarations: block.declarations,
        colon: entry.colon,
        property: entry.property,
        variable_name: components.last().is_some_and(|item| item.variable_name),
    })
}

fn declaration_name(head: &str) -> Option<&str> {
    let end = head.find(|ch| !name_character(ch)).unwrap_or(head.len());
    let (name, mut tail) = head.split_at(end);
    if !name.starts_with(|ch: char| ch.is_alphabetic() || matches!(ch, '-' | '_')) {
        return None;
    }
    while let Some(comment) = tail.trim_start().strip_prefix("/*") {
        tail = comment.split_once("*/")?.1;
    }
    tail.trim().is_empty().then_some(name)
}

fn name_character(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '-' | '_')
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut index = start + 1;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            byte if byte == quote => return Some(index),
            b'\\' => index += 1,
            _ => {}
        }
        index += 1;
    }
    None
}
