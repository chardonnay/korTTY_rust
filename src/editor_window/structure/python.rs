//! Python declarations and signatures over the editor's masked source.

use std::collections::{HashMap, HashSet};

use super::{
    Analysis, Language, Masker, Scan, Scope, column_of, declaration, identifier, lines, masked,
};

mod bindings;
mod loops;
mod targets;
use bindings::Statement;
pub(super) use loops::Loops;
use targets::Site;

const MAX_PARAMETER_UNITS: usize = 4_000;

pub(super) fn scan(source: &str, count_references: bool) -> Analysis {
    let mut scan = Scan {
        python_field_writes: count_references.then(HashSet::new),
        ..Scan::default()
    };
    let mut lexer = Masker::default();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut signature: Option<Parameters> = None;
    let mut statement: Option<Statement> = None;
    let mut loops = Loops::default();
    let mut last_code = 0;
    for (line, raw) in lines(source).enumerate() {
        let (starts_in_code, code) = masked(raw, &mut lexer, Language::Python, false);
        if let Some(mut parameters) = signature.take()
            && !starts_in_code
            && parameters.read_line(&mut scan, raw, &code, line, 0)
        {
            signature = Some(parameters);
        }
        if code.trim().is_empty() {
            if starts_in_code {
                statement = None;
            }
            loops.line(&mut scan, line, &code, starts_in_code, None);
            continue;
        }
        let previous_code = last_code;
        last_code = line;
        if starts_in_code {
            statement = None;
            let level = indentation(raw);
            scan.close_scopes(&mut scopes, level, previous_code);
            let parent = scopes.last().map(|scope| scope.node);
            if let Some((kind, name)) =
                declaration(raw.trim_start(), Language::Python).or_else(|| {
                    let (name, tail) = identifier(raw.trim_start())?;
                    bindings::augmented(tail).then_some(("variable", name))
                })
            {
                let column = column_of(raw, name);
                if kind == "variable" {
                    let kind = local_kind(&scan, parent);
                    let shape = bindings::right_hand_side(&code[column + name.len()..])
                        .and_then(bindings::shape);
                    let category = match kind {
                        "field" => (kind, shape),
                        _ => (shape.unwrap_or(kind), None),
                    };
                    scan.binding(category, name, line, column, parent, Language::Python);
                } else if let Some(node) = scan.node(kind, name, line, column, parent, ".") {
                    scopes.push(Scope { node, level });
                    if kind == "def" {
                        let end = column + name.len();
                        if let Some(open) = code[end..].find('(') {
                            let mut parameters = Parameters::new(node);
                            if parameters.read_line(&mut scan, raw, &code, line, end + open + 1) {
                                signature = Some(parameters);
                            }
                        }
                    }
                }
            } else if let Some((owner, name, rhs)) = field_assignment(&scan, &scopes, &code) {
                scan.binding(
                    ("field", bindings::shape(rhs)),
                    name,
                    line,
                    column_of(&code, name),
                    Some(owner),
                    Language::Python,
                );
            } else {
                statement = Statement::start(&mut scan, line, &code, parent);
            }
        } else if let Some(mut pending) = statement.take()
            && pending.read_line(&mut scan, line, &code, 0)
        {
            statement = Some(pending);
        }
        let scope = scopes.last().map(|scope| scope.node);
        loops.line(&mut scan, line, &code, starts_in_code, scope);
    }
    scan.close_scopes(&mut scopes, 0, last_code);
    if count_references {
        scan.count_uses(source, Language::Python, Some(&loops));
    }
    scan.analysis
        .variables
        .sort_by_key(|binding| (binding.line, binding.column));
    scan.analysis
}

fn local_kind(scan: &Scan, scope: Option<usize>) -> &'static str {
    if scope.is_some_and(|index| scan.analysis.nodes[index].kind == "class") {
        "field"
    } else {
        "variable"
    }
}

fn field_assignment<'a>(
    scan: &Scan,
    scopes: &[Scope],
    code: &'a str,
) -> Option<(usize, &'a str, &'a str)> {
    let (name, tail) = identifier(code.trim_start())?;
    if matches!(name, "self" | "cls")
        && let Some(member) = tail.trim_start().strip_prefix('.')
    {
        let (member, tail) = identifier(member.trim_start())?;
        let owner = scopes
            .iter()
            .rev()
            .find(|scope| scan.analysis.nodes[scope.node].kind == "class")?;
        return Some((owner.node, member, bindings::right_hand_side(tail)?));
    }
    let owner = scopes.last()?.node;
    if local_kind(scan, Some(owner)) == "field" {
        Some((owner, name, bindings::right_hand_side(tail)?))
    } else {
        None
    }
}

pub(super) enum Reference {
    Plain,
    Binding(usize),
    Ignored,
}

pub(super) struct References<'a> {
    analysis: &'a Analysis,
    loops: &'a Loops,
    special: Vec<bool>,
    field_writes: Option<&'a HashSet<Site>>,
    fields: HashMap<usize, HashMap<&'a str, usize>>,
    classes: Vec<usize>,
    active_classes: Vec<usize>,
    next_class: usize,
    comprehensions: Vec<usize>,
    active_comprehensions: Vec<usize>,
    next_comprehension: usize,
}

impl<'a> References<'a> {
    pub(super) fn new(
        loops: &'a Loops,
        analysis: &'a Analysis,
        scopes: &[Option<usize>],
        field_writes: Option<&'a HashSet<Site>>,
    ) -> Self {
        let mut fields: HashMap<_, HashMap<_, _>> = HashMap::new();
        let mut special = vec![false; analysis.variables.len()];
        for (index, (binding, scope)) in analysis.variables.iter().zip(scopes).enumerate() {
            if binding.kind == "field"
                && let Some(scope) = *scope
            {
                fields
                    .entry(scope)
                    .or_default()
                    .insert(binding.name.as_str(), index);
                special[index] = true;
            }
        }
        let mut comprehensions: Vec<_> = (0..loops.comprehensions.len()).collect();
        comprehensions.sort_by_key(|&id| loops.comprehensions[id].open);
        for comprehension in &loops.comprehensions {
            for &index in comprehension.names.values() {
                special[index] = true;
            }
        }
        let classes = analysis
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == "class")
            .map(|(index, _)| index)
            .collect();
        Self {
            analysis,
            loops,
            special,
            field_writes,
            fields,
            classes,
            active_classes: Vec::new(),
            next_class: 0,
            comprehensions,
            active_comprehensions: Vec::new(),
            next_comprehension: 0,
        }
    }

    pub(super) fn special(&self, binding: usize) -> bool {
        self.special[binding]
    }

    pub(super) fn lookup(&mut self, site: Site, prefix: &str, name: &str) -> Reference {
        if let Some(object) = prefix.trim_end().strip_suffix('.') {
            return self.field(site.0, object, name);
        }
        if self
            .field_writes
            .is_some_and(|writes| writes.contains(&site))
        {
            return Reference::Ignored;
        }
        let scopes = &self.loops.comprehensions;
        while self
            .active_comprehensions
            .last()
            .is_some_and(|&id| scopes[id].end <= site)
        {
            self.active_comprehensions.pop();
        }
        while let Some(&id) = self.comprehensions.get(self.next_comprehension)
            && scopes[id].open < site
        {
            self.next_comprehension += 1;
            if site < scopes[id].end {
                self.active_comprehensions.push(id);
            }
        }
        self.active_comprehensions
            .iter()
            .rev()
            .find_map(|&id| {
                let scope = &scopes[id];
                let iterable_end = scope.next_clause.unwrap_or(scope.end);
                if site >= scope.first_iterable && site < iterable_end {
                    return None;
                }
                scope.names.get(name).copied()
            })
            .map_or(Reference::Plain, Reference::Binding)
    }

    fn field(&mut self, line: usize, object: &str, name: &str) -> Reference {
        let object = object.trim_end();
        let receiver = object
            .rsplit(|ch: char| !ch.is_alphanumeric() && ch != '_')
            .next()
            .unwrap_or_default();
        let prefix = &object[..object.len() - receiver.len()];
        if receiver.is_empty() || prefix.trim_end().ends_with('.') {
            return Reference::Ignored;
        }
        let nodes = &self.analysis.nodes;
        while self
            .active_classes
            .last()
            .is_some_and(|&id| nodes[id].end_line < line)
        {
            self.active_classes.pop();
        }
        while let Some(&id) = self.classes.get(self.next_class)
            && nodes[id].line <= line
        {
            self.next_class += 1;
            if line <= nodes[id].end_line {
                self.active_classes.push(id);
            }
        }
        let owner = if matches!(receiver, "self" | "cls") {
            self.active_classes.last().copied()
        } else {
            self.active_classes
                .iter()
                .rev()
                .copied()
                .find(|&id| nodes[id].name == receiver)
        };
        owner
            .and_then(|owner| self.fields.get(&owner)?.get(name).copied())
            .map_or(Reference::Ignored, Reference::Binding)
    }
}

fn indentation(line: &str) -> usize {
    line.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .fold(0, |width, byte| {
            if byte == b'\t' {
                (width / 8 + 1) * 8
            } else {
                width + 1
            }
        })
}

struct Parameter {
    name: String,
    line: usize,
    column: usize,
}

struct Parameters {
    node: usize,
    nesting: usize,
    remaining: usize,
    at_start: bool,
    pending: Option<Parameter>,
}

impl Parameters {
    fn new(node: usize) -> Self {
        Self {
            node,
            nesting: 0,
            remaining: MAX_PARAMETER_UNITS - 1,
            at_start: true,
            pending: None,
        }
    }

    fn read_line(
        &mut self,
        scan: &mut Scan,
        raw: &str,
        code: &str,
        line: usize,
        start: usize,
    ) -> bool {
        // Charge the original UTF-16 units. Masking preserves UTF-8 byte columns.
        let end = raw[start..]
            .char_indices()
            .find_map(
                |(offset, ch)| match self.remaining.checked_sub(ch.len_utf16()) {
                    Some(left) => {
                        self.remaining = left;
                        None
                    }
                    None => Some(start + offset),
                },
            )
            .unwrap_or(raw.len());
        for (offset, ch) in code[start..end].char_indices() {
            let column = start + offset;
            if ch == '\\' && column + 1 == raw.len() {
                continue;
            }
            if self.nesting == 0 && matches!(ch, ',' | ')') {
                if let Some(parameter) = self.pending.take() {
                    scan.binding(
                        ("parameter", None),
                        &parameter.name,
                        parameter.line,
                        parameter.column,
                        Some(self.node),
                        Language::Python,
                    );
                }
                if ch == ')' {
                    return false;
                }
                self.at_start = true;
                continue;
            }
            if self.at_start && !ch.is_whitespace() && ch != '*' {
                self.at_start = false;
                if let Some((name, _)) = identifier(&code[column..end])
                    && !matches!(name, "self" | "cls" | "this")
                {
                    self.pending = Some(Parameter {
                        name: name.to_owned(),
                        line,
                        column,
                    });
                }
            }
            match ch {
                '(' | '[' | '{' => self.nesting += 1,
                ')' | ']' | '}' => self.nesting = self.nesting.saturating_sub(1),
                _ => {}
            }
        }
        if end < raw.len() || self.remaining == 0 {
            scan.analysis.limited = true;
            return false;
        }
        self.remaining -= 1; // A normalized newline between physical lines.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Language, analyze, select_name};
    use crate::editor_window::scan_outline;
    use iced::widget::text_editor;

    #[test]
    fn python_class_fields_and_local_names_keep_separate_references() {
        let source = concat!(
            "größe = 0\n",
            "class Geräte:\n",
            "    größe = 1\n",
            "    def __init__(self, größe):\n",
            "        self.token: str = 'hidden.self.größe'\n",
            "        größe = größe + self.größe\n",
            "        self.token += str(größe)\n",
            "        return other.größe + wrapper.self.größe + Geräte.größe\n",
            "    @classmethod\n",
            "    def reset(cls, größe):\n",
            "        cls.status = größe\n",
            "        return cls.größe + cls.status\n",
            "    größe = 2\n",
            "print(größe)\n",
        );
        let result = analyze(source, "python");
        let variables: Vec<_> = result
            .variables
            .iter()
            .map(|variable| {
                (
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.kind,
                    variable.line,
                    variable.uses,
                )
            })
            .collect();
        assert_eq!(
            variables,
            [
                ("größe", "", "variable", 0, 1),
                ("größe", "Geräte", "field", 2, 3),
                ("größe", "Geräte.__init__", "parameter", 3, 3),
                ("token", "Geräte", "field", 4, 1),
                ("größe", "Geräte.reset", "parameter", 9, 1),
                ("status", "Geräte", "field", 10, 1),
            ]
        );

        let outline = scan_outline(source, Language::Python);
        let status = outline
            .items
            .iter()
            .find(|item| item.name == "status")
            .unwrap();
        let mut content = text_editor::Content::with_text(source);
        assert_eq!((status.kind, status.line, status.column), ("field", 10, 12));
        assert!(select_name(
            &mut content,
            status.line,
            status.column..status.end_column
        ));
        assert_eq!(content.selection().as_deref(), Some("status"));
    }

    #[test]
    fn python_loop_bindings_preserve_destructuring_and_comprehension_scopes() {
        let source = concat!(
            "async def parse(records):\n",
            "    item = 42\n",
            "    for i, (größe, *tail) in records:\n",
            "        log(i, größe, tail)\n",
            "    async for label in records:\n",
            "        log(label)\n",
            "    mapped = [item for item in item if item]\n",
            "    log(item)\n",
            "    nested = [\n",
            "        (x, y)\n",
            "        for x in records\n",
            "        for y in x\n",
            "        if y\n",
            "    ]\n",
            "    combined = [[item for item in item] for item in records]\n",
            "    log(item) # for decoy in records\n",
        );
        let result = analyze(source, "python");
        assert!(!result.limited);
        let uses = |name: &str| {
            result
                .variables
                .iter()
                .filter(|binding| binding.name == name)
                .map(|binding| (binding.line, binding.uses))
                .collect::<Vec<_>>()
        };
        assert_eq!(uses("item"), [(1, 3), (6, 2), (14, 1), (14, 1)]);
        assert_eq!(uses("records"), [(0, 4)]);
        for name in ["i", "größe", "tail"] {
            assert_eq!(uses(name), [(2, 1)]);
        }
        assert_eq!(uses("label"), [(4, 1)]);
        assert_eq!(uses("x"), [(10, 2)]);
        assert_eq!(uses("y"), [(11, 2)]);
        assert!(uses("decoy").is_empty());

        let outline = scan_outline(source, Language::Python);
        let target = outline
            .items
            .iter()
            .find(|item| item.name == "größe")
            .unwrap();
        let mut content = text_editor::Content::with_text(source);
        assert!(select_name(
            &mut content,
            target.line,
            target.column..target.end_column
        ));
        assert_eq!(content.selection().as_deref(), Some("größe"));
    }

    #[test]
    fn python_assignments_and_aliases_cover_the_java_reference_cases() {
        let source = concat!(
            "def load(pair):\n",
            "    left, right = pair\n",
            "    with open('Quelle as ignored') as handle:\n",
            "        content = handle.read()\n",
            "    try:\n",
            "        return left, right, content\n",
            "    except OSError as error:\n",
            "        return str(error)\n",
            "\n",
            "counter += 1\n",
            "mapping = dict()\n",
            "items = tuple(source)\n",
            "print(counter, mapping, items)\n",
        );
        let result = analyze(source, "python");
        assert!(!result.limited);
        assert_eq!(result.variables.len(), 9);
        for (name, scope, kind, line) in [
            ("pair", "load", "parameter", 0),
            ("left", "load", "variable", 1),
            ("right", "load", "variable", 1),
            ("handle", "load", "variable", 2),
            ("content", "load", "variable", 3),
            ("error", "load", "variable", 6),
            ("counter", "", "variable", 9),
            ("mapping", "", "hash", 10),
            ("items", "", "array", 11),
        ] {
            let binding = result
                .variables
                .iter()
                .find(|binding| binding.name == name)
                .unwrap();
            assert_eq!(
                (
                    binding.scope.as_str(),
                    binding.kind,
                    binding.line,
                    binding.uses
                ),
                (scope, kind, line, 1)
            );
            let line = source.lines().nth(line).unwrap();
            assert_eq!(&line[binding.column..binding.end_column], name);
        }
    }

    #[test]
    fn python_multiline_binding_targets_keep_scope_and_utf8_navigation() {
        let source = concat!(
            "head = 0\n",
            "async def parse(pairs):\n",
            "    (\n",
            "        head, (größe, *rest),\n",
            "    ) = pairs\n",
            "    left, \\\n",
            "        right = pairs\n",
            "    async with (\n",
            "        opener('as hidden') as file,\n",
            "        manager('über') as (enter, schließen),\n",
            "    ):\n",
            "        use(file, enter, schließen, left, right)\n",
            "    try:\n",
            "        return head, größe, rest\n",
            "    except* (\n",
            "        ValueError, TypeError,\n",
            "    ) as error:\n",
            "        log(error)\n",
            "    (decoy, another) == pairs\n",
            "    fn(non_binding=pairs)\n",
            "log(head)\n",
        );
        let result = analyze(source, "python");
        assert!(!result.limited);
        assert_eq!(result.variables.len(), 11);
        for (name, line) in [
            ("head", 3),
            ("größe", 3),
            ("rest", 3),
            ("left", 5),
            ("right", 6),
            ("file", 8),
            ("enter", 9),
            ("schließen", 9),
            ("error", 16),
        ] {
            let binding = result
                .variables
                .iter()
                .find(|binding| binding.name == name && binding.scope == "parse")
                .unwrap();
            assert_eq!((binding.line, binding.uses), (line, 1));
        }
        assert_eq!(result.variables[0].uses, 1);
        let outline = scan_outline(source, Language::Python);
        let target = outline
            .items
            .iter()
            .find(|item| item.name == "schließen")
            .unwrap();
        let mut content = text_editor::Content::with_text(source);
        assert!(select_name(
            &mut content,
            target.line,
            target.column..target.end_column
        ));
        assert_eq!(content.selection().as_deref(), Some("schließen"));
    }

    #[test]
    fn python_multiline_parameters_keep_exact_names_and_reference_scopes() {
        let source = concat!(
            "scale = 7\n",
            "class Gerät(\n",
            "Base, # a continuation at column zero\n",
            "):\n",
            "    @cached(\"def hidden():\")\n",
            "    async def run(self,\\\n",
            "    größe: tuple[int, int] = (\n",
            "        1, 2,\n",
            "    ),\n",
            "    /,\n",
            "    scale=choose(\")\", {\"x\": 1}),\n",
            "    *,\n",
            "    limit: int = 2,\n",
            "    **options,\n",
            "    ) -> int:\n",
            "        def nested(\\\n",
            "        scale=5,\n",
            "        ):\n",
            "            return scale + größe\n",
            "        return scale + limit + len(options) + obj.scale\n",
            "\n",
            "print(scale)\n",
        );
        let result = analyze(source, "python");
        assert!(!result.limited);
        let outline: Vec<_> = result
            .nodes
            .iter()
            .map(|node| {
                (
                    node.qualified.as_str(),
                    node.kind,
                    node.parent,
                    node.line,
                    node.end_line,
                )
            })
            .collect();
        assert_eq!(
            outline,
            [
                ("Gerät", "class", None, 1, 19),
                ("Gerät.run", "method", Some(0), 5, 19),
                ("Gerät.run.nested", "function", Some(1), 15, 18),
            ]
        );
        let bindings: Vec<_> = result
            .variables
            .iter()
            .map(|variable| {
                (
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.kind,
                    variable.line,
                    variable.column,
                    variable.uses,
                )
            })
            .collect();
        assert_eq!(
            bindings,
            [
                ("scale", "", "variable", 0, 0, 1),
                ("größe", "Gerät.run", "parameter", 6, 4, 1),
                ("scale", "Gerät.run", "parameter", 10, 4, 1),
                ("limit", "Gerät.run", "parameter", 12, 4, 1),
                ("options", "Gerät.run", "parameter", 13, 6, 1),
                ("scale", "Gerät.run.nested", "parameter", 16, 8, 1),
            ]
        );

        let parameter = &result.variables[1];
        let mut content = text_editor::Content::with_text(source);
        assert!(select_name(
            &mut content,
            parameter.line,
            parameter.column..parameter.end_column
        ));
        assert_eq!(content.selection().as_deref(), Some("größe"));
    }

    #[test]
    fn python_parameter_limit_preserves_following_definitions() {
        let long_comment = "🙂".repeat(2_100);
        let source = format!(
            concat!(
                "def bounded(\n",
                "    first=1,\n",
                "    # {comment}\n",
                "    outside_limit=2,\n",
                "):\n",
                "    return first\n",
                "\n",
                "def after(keep):\n",
                "    return keep\n",
            ),
            comment = long_comment
        );
        let result = analyze(&source, "python");
        assert!(result.limited);
        let names: Vec<_> = result
            .variables
            .iter()
            .map(|variable| {
                (
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.uses,
                )
            })
            .collect();
        assert_eq!(names, [("first", "bounded", 1), ("keep", "after", 1)]);
        assert_eq!(result.nodes[0].end_line, 5);
        assert_eq!(result.nodes[1].line, 7);
    }
}
