use std::{collections::BTreeSet, fmt::Write as _};

use super::{
    Flags, NEEDS_UTF16, UNSUPPORTED, close_ascii_case_class, is_java_supplementary, literal_escape,
    shorthand, take, write_property,
};

const MAX_CLASS_DEPTH: usize = 64;
const MAX_CLASS_OUTPUT: usize = 256 * 1024;

enum Term {
    Literal(u32),
    Range(u32, u32),
    Predicate(String, LowSurrogates, bool),
}

#[derive(Clone, Copy)]
pub(super) struct LowSurrogates([u64; 16]);

impl LowSurrogates {
    pub(super) const EMPTY: Self = Self([0; 16]);
    pub(super) const ALL: Self = Self([u64::MAX; 16]);

    pub(super) fn range(first: u32, last: u32) -> Self {
        let first = first.max(0xDC00);
        let last = last.min(0xDFFF);
        if first > last {
            return Self::EMPTY;
        }
        Self(std::array::from_fn(|index| {
            let base = 0xDC00 + index as u32 * 64;
            let start = first.max(base);
            let end = last.min(base + 63);
            if start <= end {
                (u64::MAX << (start - base)) & (u64::MAX >> (base + 63 - end))
            } else {
                0
            }
        }))
    }

    pub(super) fn negate(self) -> Self {
        Self(self.0.map(|word| !word))
    }

    fn union(self, other: Self) -> Self {
        Self(std::array::from_fn(|index| self.0[index] | other.0[index]))
    }

    fn intersect(self, other: Self) -> Self {
        Self(std::array::from_fn(|index| self.0[index] & other.0[index]))
    }

    pub(super) fn write(self, output: &mut String, scalar: &str) {
        self.write_with_canonical(output, scalar, false);
    }

    pub(super) fn write_with_canonical(self, output: &mut String, scalar: &str, canonical: bool) {
        output.push_str(r"\q{");
        if canonical {
            output.push('!');
        }
        let addresses = (0..1024).filter(|&index| self.0[index / 64] >> (index % 64) & 1 != 0);
        let mut units = addresses.map(|index| 0xDC00 + index).peekable();
        while let Some(first) = units.next() {
            let mut last = first;
            while units.next_if_eq(&(last + 1)).is_some() {
                last += 1;
            }
            write!(output, "{first:04x}{last:04x}").expect("string write");
        }
        output.push('}');
        output.push_str(scalar);
    }
}

type SetId = usize;

enum Set {
    Atom(Term),
    Latin1(BTreeSet<char>),
    Union(Vec<SetId>),
    Intersection(SetId, SetId),
    Complement(SetId),
}

struct Cursor<'a> {
    remaining: &'a str,
    flags: Flags,
    utf16: bool,
    negated_property: bool,
}

impl Cursor<'_> {
    fn peek(&mut self) -> Result<char, &'static str> {
        self.remaining = self.flags.skip_trivia(self.remaining);
        self.remaining.chars().next().ok_or(UNSUPPORTED)
    }

    fn next(&mut self) -> Result<char, &'static str> {
        self.remaining = self.flags.skip_trivia(self.remaining);
        take(&mut self.remaining).ok_or(UNSUPPORTED)
    }

    fn atom(&mut self, range_end: bool) -> Result<Term, &'static str> {
        match self.next()? {
            '\\' => {
                self.negated_property |= self.remaining.starts_with('P');
                escaped(&mut self.remaining, self.flags, range_end, self.utf16)
            }
            ch => Ok(Term::Literal(u32::from(ch))),
        }
    }

    fn term(&mut self, atom: Term) -> Result<Term, &'static str> {
        let Term::Literal(first) = atom else {
            return Ok(atom);
        };
        if self.peek()? == '-' && !self.remaining[1..].starts_with(['[', ']']) {
            self.next()?;
            match self.atom(true)? {
                Term::Literal(last) if first <= last => Ok(Term::Range(first, last)),
                _ => Err(UNSUPPORTED),
            }
        } else {
            Ok(Term::Literal(first))
        }
    }
}

pub(super) fn translate(
    remaining: &mut &str,
    flags: Flags,
    utf16: bool,
) -> Result<(String, bool), &'static str> {
    if flags.canonical && !utf16 {
        return Err(NEEDS_UTF16);
    }
    let mut cursor = Cursor {
        remaining,
        flags,
        utf16,
        negated_property: false,
    };
    let mut sets = Vec::new();
    let set = parse(&mut cursor, &mut sets, 1, true)?;
    *remaining = cursor.remaining;
    let mut output = String::new();
    let lows = render(&sets, set, flags, &mut output, 1)?;
    let codepoints = sets
        .iter()
        .fold(Vec::with_capacity(sets.len()), |mut kinds, set| {
            let codepoints = match set {
                Set::Atom(term) => term.codepoint_predicate(flags),
                Set::Latin1(_) => false,
                Set::Union(members) => members.iter().any(|&member| kinds[member]),
                Set::Intersection(left, right) => kinds[*left] || kinds[*right],
                Set::Complement(_) => true,
            };
            kinds.push(codepoints);
            kinds
        })[set];
    if utf16 {
        let mut wrapped = String::new();
        lows.write_with_canonical(&mut wrapped, &output, flags.canonical);
        let codepoint_search = if flags.canonical {
            cursor.negated_property
        } else {
            codepoints
        };
        Ok((wrapped, codepoint_search))
    } else {
        Ok((output, codepoints))
    }
}

fn parse(
    cursor: &mut Cursor<'_>,
    sets: &mut Vec<Set>,
    depth: usize,
    consume_close: bool,
) -> Result<SetId, &'static str> {
    if depth > MAX_CLASS_DEPTH {
        return Err(UNSUPPORTED);
    }
    let negated = consume_close && cursor.remaining.starts_with('^');
    if negated {
        cursor.remaining = &cursor.remaining[1..];
    }
    // All references to this bit set see literals added later in the class.
    let bits = add(sets, Set::Latin1(BTreeSet::new()));
    let mut pending_bits = false;
    let mut previous = Vec::new();
    let mut current = None;
    loop {
        let atom = match cursor.peek()? {
            ']' if pending_bits || !previous.is_empty() => {
                if consume_close {
                    cursor.next()?;
                }
                if pending_bits {
                    previous.push(bits);
                }
                let set = combine(sets, previous).ok_or(UNSUPPORTED)?;
                return Ok(if negated {
                    add(sets, Set::Complement(set))
                } else {
                    set
                });
            }
            '[' => {
                cursor.next()?;
                let nested = parse(cursor, sets, depth + 1, true)?;
                current = Some(nested);
                previous.push(nested);
                continue;
            }
            '&' => {
                cursor.next()?;
                let skipped = cursor.flags.skip_trivia(cursor.remaining) != cursor.remaining;
                if cursor.peek()? == '&' {
                    cursor.next()?;
                    let right = intersection_operand(cursor, sets, depth)?;
                    if pending_bits {
                        if previous.is_empty() {
                            current = Some(bits);
                        }
                        previous.push(bits);
                        pending_bits = false;
                    }
                    current = right.or(current);
                    let result = match combine(sets, std::mem::take(&mut previous)) {
                        Some(left) => intersect(sets, left, current.ok_or(UNSUPPORTED)?),
                        None => right.ok_or(UNSUPPORTED)?,
                    };
                    previous.push(result);
                    continue;
                }
                if skipped {
                    // Java rewinds one character after the lookahead, which
                    // can leave its range parser in the skipped trivia.
                    cursor.atom(false)?
                } else {
                    Term::Literal(u32::from('&'))
                }
            }
            _ => cursor.atom(false)?,
        };
        let term = cursor.term(atom)?;
        if !cursor.utf16 && term.has_surrogates() {
            return Err(NEEDS_UTF16);
        }
        if let Term::Literal(ch) = term
            && let Some(ch) = char::from_u32(ch)
            && latin1_bit(ch, cursor.flags)
        {
            let Set::Latin1(literals) = &mut sets[bits] else {
                unreachable!()
            };
            literals.insert(ch);
            pending_bits = true;
            current = None;
        } else {
            let term = add(sets, Set::Atom(term));
            previous.push(term);
            current = Some(term);
        }
    }
}

impl Term {
    fn codepoint_predicate(&self, flags: Flags) -> bool {
        match *self {
            Self::Literal(ch) => {
                is_java_supplementary(ch)
                    || flags.native_case()
                        && char::from_u32(ch).is_some_and(fancy_regex::internal::java_cased_literal)
            }
            Self::Range(_, last) => {
                flags.ignore_case || is_java_supplementary(last) || self.has_surrogates()
            }
            Self::Predicate(_, _, codepoints) => codepoints,
        }
    }

    fn has_surrogates(&self) -> bool {
        match *self {
            Self::Literal(ch) => (0xD800..=0xDFFF).contains(&ch),
            Self::Range(first, last) => first <= 0xDFFF && last >= 0xD800,
            Self::Predicate(..) => false,
        }
    }

    fn low_surrogates(&self) -> LowSurrogates {
        match *self {
            Self::Literal(ch) => LowSurrogates::range(ch, ch),
            Self::Range(first, last) => LowSurrogates::range(first, last),
            Self::Predicate(_, lows, _) => lows,
        }
    }
}

fn intersection_operand(
    cursor: &mut Cursor<'_>,
    sets: &mut Vec<Set>,
    depth: usize,
) -> Result<Option<SetId>, &'static str> {
    let mut members = Vec::new();
    loop {
        match cursor.peek()? {
            ']' | '&' => return Ok(combine(sets, members)),
            '[' => {
                cursor.next()?;
                members.push(parse(cursor, sets, depth + 1, true)?);
            }
            _ => members.push(parse(cursor, sets, depth + 1, false)?),
        }
    }
}

fn latin1_bit(ch: char, flags: Flags) -> bool {
    ch <= '\u{ff}'
        && !(flags.native_case()
            && matches!(
                ch,
                'ÿ' | 'µ' | 'I' | 'i' | 'S' | 's' | 'K' | 'k' | 'Å' | 'å'
            ))
}

fn add(sets: &mut Vec<Set>, set: Set) -> SetId {
    let id = sets.len();
    sets.push(set);
    id
}

fn combine(sets: &mut Vec<Set>, mut members: Vec<SetId>) -> Option<SetId> {
    members.dedup();
    match members.as_slice() {
        [] => None,
        [member] => Some(*member),
        _ => Some(add(sets, Set::Union(members))),
    }
}

fn intersect(sets: &mut Vec<Set>, left: SetId, right: SetId) -> SetId {
    if left == right || matches!(sets[left], Set::Intersection(_, rhs) if rhs == right) {
        left
    } else {
        add(sets, Set::Intersection(left, right))
    }
}

fn render(
    sets: &[Set],
    id: SetId,
    flags: Flags,
    output: &mut String,
    depth: usize,
) -> Result<LowSurrogates, &'static str> {
    if depth > MAX_CLASS_DEPTH || output.len() > MAX_CLASS_OUTPUT {
        return Err(UNSUPPORTED);
    }
    let lows = match &sets[id] {
        Set::Atom(term) => {
            output.push_str(&union(std::slice::from_ref(term), flags)?);
            term.low_surrogates()
        }
        Set::Latin1(literals) => {
            let terms: Vec<_> = literals
                .iter()
                .map(|&ch| Term::Literal(u32::from(ch)))
                .collect();
            output.push_str(&union(&terms, flags)?);
            LowSurrogates::EMPTY
        }
        Set::Union(members) => {
            output.push('[');
            let mut lows = LowSurrogates::EMPTY;
            for &member in members {
                lows = lows.union(render(sets, member, flags, output, depth + 1)?);
            }
            output.push(']');
            lows
        }
        Set::Intersection(left, right) => {
            output.push('[');
            let left = render(sets, *left, flags, output, depth + 1)?;
            output.push_str("&&");
            let right = render(sets, *right, flags, output, depth + 1)?;
            output.push(']');
            left.intersect(right)
        }
        Set::Complement(member) => {
            output.push_str(r"[\x{0}-\x{10ffff}--");
            let lows = render(sets, *member, flags, output, depth + 1)?;
            output.push(']');
            lows.negate()
        }
    };
    if output.len() > MAX_CLASS_OUTPUT {
        return Err(UNSUPPORTED);
    }
    Ok(lows)
}

pub(super) fn write(output: &mut String, members: &str, negated: bool) {
    // Set subtraction avoids a regex-syntax negation bug when two input
    // ranges meet on opposite sides of the surrogate gap.
    let (start, end) = if negated {
        (r"[\x{0}-\x{10ffff}--[", "]]")
    } else {
        ("[", "]")
    };
    output.push_str(start);
    output.push_str(members);
    output.push_str(end);
}

pub(super) fn shorthand_lows(escape: char) -> LowSurrogates {
    if escape.is_ascii_uppercase() {
        LowSurrogates::ALL
    } else {
        LowSurrogates::EMPTY
    }
}

fn escaped(
    remaining: &mut &str,
    flags: Flags,
    range_end: bool,
    utf16: bool,
) -> Result<Term, &'static str> {
    let escape = take(remaining).ok_or(UNSUPPORTED)?;
    if escape == 'v' && (range_end || remaining.starts_with('-')) {
        return Ok(Term::Literal(0x0b));
    }
    if flags.unicode_classes && matches!(escape, 'd' | 'D' | 's' | 'S' | 'w' | 'W') {
        return Ok(Term::Predicate(
            format!(r"\{escape}"),
            shorthand_lows(escape),
            true,
        ));
    }
    if let Some((predicate, _)) = shorthand(escape) {
        return Ok(Term::Predicate(
            predicate.to_owned(),
            shorthand_lows(escape),
            escape.is_ascii_uppercase(),
        ));
    }
    let character = match escape {
        'p' | 'P' => {
            let mut predicate = String::new();
            let (lows, codepoints) =
                write_property(&mut predicate, remaining, escape, flags, utf16)?;
            return Ok(Term::Predicate(predicate, lows, codepoints));
        }
        _ => literal_escape(remaining, escape, flags)?.ok_or(UNSUPPORTED)?,
    };
    Ok(Term::Literal(character))
}

fn union(terms: &[Term], flags: Flags) -> Result<String, &'static str> {
    if terms.is_empty() {
        return Err(UNSUPPORTED);
    }
    let mut operand = String::from("[");
    for term in terms {
        match term {
            Term::Literal(character) => scalar_range(&mut operand, *character, *character, false),
            Term::Range(first, last) => scalar_range(&mut operand, *first, *last, true),
            Term::Predicate(class, ..) => operand.push_str(class),
        }
    }
    if flags.ascii_case() {
        close_ascii_case_class(&mut operand, 0)?;
    }
    operand.push(']');
    if flags.native_case() {
        operand = fancy_regex::internal::java_unicode_class(&operand).ok_or(UNSUPPORTED)?;
    }
    Ok(operand)
}

fn scalar_range(output: &mut String, mut first: u32, mut last: u32, range: bool) {
    if (0xD800..=0xDFFF).contains(&first) {
        first = 0xE000;
    }
    if (0xD800..=0xDFFF).contains(&last) {
        last = 0xD7FF;
    }
    if first > last {
        output.push_str(r"[\s&&\S]");
    } else {
        write!(output, r"\x{{{first:x}}}").expect("string write");
        if range {
            write!(output, r"-\x{{{last:x}}}").expect("string write");
        }
    }
}
