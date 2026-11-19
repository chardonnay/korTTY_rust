//! Maps validator locations to the original JSON source bytes.

use std::borrow::Cow;
use std::ops::Range;

use super::Mark;

pub(super) fn syntax(source: &str, line: usize, column: usize) -> Range<usize> {
    let mut rows = source.split_inclusive('\n');
    let offset = rows
        .by_ref()
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum::<usize>();
    let text = rows
        .next()
        .unwrap_or_default()
        .trim_end_matches(['\r', '\n']);
    let column = text.floor_char_boundary(column.saturating_sub(1).min(text.len()));
    let length = text[column..].chars().next().map_or(0, char::len_utf8);
    offset + column..offset + column + length
}

pub(super) fn marks(source: &str, paths: &[String]) -> Vec<Mark> {
    if paths.is_empty() {
        return Vec::new();
    }
    let paths: Vec<_> = paths
        .iter()
        .map(|path| {
            let segments: Vec<_> = path
                .split('/')
                .skip(1)
                .map(|segment| segment.replace("~1", "/").replace("~0", "~"))
                .collect();
            (path, segments)
        })
        .collect();
    let pending: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, (_, segments))| (index, segments.as_slice()))
        .collect();
    let mut spans = vec![None; pending.len()];
    let mut json = Scan {
        source,
        index: 0,
        spans: &mut spans,
    };
    let _ = json.value(&pending);
    paths
        .into_iter()
        .zip(spans)
        .filter_map(|((path, _), bytes)| {
            let label = if path.is_empty() { "(root)" } else { path };
            Some(Mark {
                label: label.into(),
                bytes: bytes?,
            })
        })
        .collect()
}

type Pending<'a> = (usize, &'a [String]);

struct Scan<'a, 'b> {
    source: &'a str,
    index: usize,
    spans: &'b mut [Option<Range<usize>>],
}

impl Scan<'_, '_> {
    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.index).copied()
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.index += 1;
        }
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.index += 1;
            true
        } else {
            false
        }
    }

    fn quoted(&mut self) -> Option<Range<usize>> {
        let start = self.index;
        if !self.eat(b'"') {
            return None;
        }
        while let Some(byte) = self.peek() {
            self.index += 1;
            match byte {
                b'\\' => {
                    self.peek()?;
                    self.index += 1;
                }
                b'"' => return Some(start..self.index),
                _ => {}
            }
        }
        None
    }

    fn value(&mut self, requested: &[Pending<'_>]) -> Option<()> {
        self.whitespace();
        let start = self.index;
        // Repeated keys are represented by their last value in the parsed instance.
        for &(index, _) in requested {
            self.spans[index] = None;
        }
        let end = match self.peek()? {
            b'{' => {
                self.object(requested)?;
                start + 1
            }
            b'[' => {
                self.array(requested)?;
                start + 1
            }
            b'"' => self.quoted()?.end,
            _ => {
                while self
                    .peek()
                    .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}".contains(&byte))
                {
                    self.index += 1;
                }
                self.index
            }
        };
        for &(index, _) in requested.iter().filter(|(_, rest)| rest.is_empty()) {
            self.spans[index] = Some(start..end);
        }
        Some(())
    }

    fn object(&mut self, requested: &[Pending<'_>]) -> Option<()> {
        self.index += 1;
        loop {
            self.whitespace();
            if self.eat(b'}') {
                return Some(());
            }
            let key = self.quoted()?;
            let mut next = Vec::new();
            if !requested.is_empty() {
                let token = &self.source[key];
                let property: Cow<'_, str> = if token.contains('\\') {
                    Cow::Owned(serde_json::from_str(token).ok()?)
                } else {
                    Cow::Borrowed(&token[1..token.len() - 1])
                };
                next.extend(children(requested, |key| key.as_str() == property.as_ref()));
            }
            self.whitespace();
            if !self.eat(b':') {
                return None;
            }
            self.value(&next)?;
            self.whitespace();
            if !self.eat(b',') {
                return self.eat(b'}').then_some(());
            }
        }
    }

    fn array(&mut self, requested: &[Pending<'_>]) -> Option<()> {
        self.index += 1;
        for index in 0usize.. {
            self.whitespace();
            if self.eat(b']') {
                return Some(());
            }
            let next: Vec<_> =
                children(requested, |key| key.parse::<usize>().ok() == Some(index)).collect();
            self.value(&next)?;
            self.whitespace();
            if !self.eat(b',') {
                return self.eat(b']').then_some(());
            }
        }
        None
    }
}

fn children<'a>(
    requested: &[Pending<'a>],
    filter: impl Fn(&String) -> bool,
) -> impl Iterator<Item = Pending<'a>> {
    requested.iter().filter_map(move |&(index, path)| {
        let (first, rest) = path.split_first()?;
        filter(first).then_some((index, rest))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_follow_escaped_keys_array_indexes_and_the_last_duplicate() {
        let source =
            "{\r\n  \"a/b~\": [null, {\"\\u00e9\": \"old\", \"é\": \"new\"}], \"flag\": false\r\n}";
        let result = marks(
            source,
            &[
                "/a~1b~0/1/é".into(),
                "/a~1b~0/0".into(),
                "/flag".into(),
                String::new(),
            ],
        );
        assert_eq!(
            result
                .iter()
                .map(|mark| &source[mark.bytes.clone()])
                .collect::<Vec<_>>(),
            ["\"new\"", "null", "false", "{"]
        );
        let source = "{\"first\":{\"gone\":null},\"first\":{}}";
        assert!(marks(source, &["/first/gone".into()]).is_empty());
    }
}
