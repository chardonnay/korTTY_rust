use std::{error::Error, fmt};

use crate::{
    Diagram, DiagramError, Direction, Edge, MAX_EDGES, MAX_LABEL_CHARS, MAX_NODES, Node, NodeShape,
    SourceRange,
};

/// Maximum UTF-8 input for the local shell parser.
pub const MAX_SCRIPT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum number of input lines for the local shell parser.
pub const MAX_SCRIPT_LINES: usize = 100_000;
const MAX_NESTING: usize = 16;
const START: &str = "start_1";
const STOP: &str = "stop_1";

/// Why a shell script could not be represented by the supported grammar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptErrorKind {
    /// The input is larger than the script byte limit.
    SourceTooLarge,
    /// The input has more lines than the script line limit.
    TooManyLines,
    /// The script contains no statements.
    Empty,
    /// The script exceeds the control-flow nesting limit.
    NestingTooDeep,
    /// The source requires a shell construct this parser does not model.
    Unsupported(&'static str),
    /// A construct in the supported grammar is incomplete or malformed.
    Syntax(&'static str),
    /// The graph would violate a diagram limit or invariant.
    Model(DiagramError),
}

/// A recoverable shell parser error with an optional one-based source line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptError {
    line: Option<usize>,
    kind: ScriptErrorKind,
}

impl ScriptError {
    fn at(line: usize, kind: ScriptErrorKind) -> Self {
        Self {
            line: Some(line),
            kind,
        }
    }

    /// Returns the source line where a problem was detected.
    pub fn line(&self) -> Option<usize> {
        self.line
    }

    /// Returns the reason this script could not be modeled.
    pub fn kind(&self) -> &ScriptErrorKind {
        &self.kind
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(formatter, "line {line}: ")?;
        }
        match &self.kind {
            ScriptErrorKind::SourceTooLarge => {
                write!(formatter, "shell script exceeds {MAX_SCRIPT_BYTES} bytes")
            }
            ScriptErrorKind::TooManyLines => {
                write!(formatter, "shell script exceeds {MAX_SCRIPT_LINES} lines")
            }
            ScriptErrorKind::Empty => formatter.write_str("shell script has no statements"),
            ScriptErrorKind::NestingTooDeep => write!(
                formatter,
                "shell script exceeds {MAX_NESTING} nested control blocks"
            ),
            ScriptErrorKind::Unsupported(reason) => {
                write!(formatter, "unsupported shell construct: {reason}")
            }
            ScriptErrorKind::Syntax(reason) => formatter.write_str(reason),
            ScriptErrorKind::Model(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for ScriptError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            ScriptErrorKind::Model(error) => Some(error),
            _ => None,
        }
    }
}

impl From<DiagramError> for ScriptError {
    fn from(error: DiagramError) -> Self {
        Self {
            line: None,
            kind: ScriptErrorKind::Model(error),
        }
    }
}

/// Builds a deterministic control-flow graph for a bounded shell subset.
///
/// It supports one simple command or assignment per line, `if`/`elif`/`else`/`fi`,
/// `while`/`until`, and word-list `for` loops. A header's `then` or `do` can be
/// on the following line or on the header line after `;`. Quoted arguments,
/// simple parameter expansions, blank lines and comments are understood.
///
/// The graph describes shell structure and does not execute code. Action and
/// condition nodes point to their actual source lines. Command status only
/// controls the transitions that the script explicitly expresses in supported
/// conditions. Pipelines, substitutions, redirections, and non-local flow
/// commands such as `break`, `continue`, `exit` and `return` are unsupported.
/// When the graph would exceed its limits, consecutive commands in the same
/// control-flow block are grouped into an action spanning their source lines.
///
/// # Errors
///
/// Returns a specific error for unsupported syntax, missing control delimiters,
/// or input or graph limits. The caller may preserve the original source for
/// inspection when no graph can be constructed.
pub fn script_flow(source: &str) -> Result<Diagram, ScriptError> {
    if source.len() > MAX_SCRIPT_BYTES {
        return Err(ScriptError {
            line: None,
            kind: ScriptErrorKind::SourceTooLarge,
        });
    }
    if source.lines().take(MAX_SCRIPT_LINES + 1).count() > MAX_SCRIPT_LINES {
        return Err(ScriptError {
            line: None,
            kind: ScriptErrorKind::TooManyLines,
        });
    }
    if let Some(interpreter) = source
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
    {
        check_interpreter(interpreter)?;
    }
    let lines = source
        .lines()
        .enumerate()
        .map(|(index, text)| lex_line(index + 1, text))
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>, _>>()?;
    let mut parser = Parser {
        lines: &lines,
        offset: 0,
    };
    let mut statements = parser.block(0)?;
    if let Some(line) = parser.peek() {
        return Err(line.error(ScriptErrorKind::Syntax("unexpected shell block delimiter")));
    }
    let (mut nodes, mut edges) = (2, 1);
    count_graph(&statements, &mut nodes, &mut edges);
    if nodes > MAX_NODES || edges > MAX_EDGES {
        group_commands(&mut statements);
    }
    let Some(first) = statements.first() else {
        return Err(ScriptError {
            line: None,
            kind: ScriptErrorKind::Empty,
        });
    };
    let mut diagram = Diagram::new(Direction::TopToBottom);
    diagram.add_node(Node::new(START, "Start", NodeShape::Terminal)?)?;
    add_nodes(&statements, &mut diagram)?;
    diagram.add_node(Node::new(STOP, "End", NodeShape::Terminal)?)?;
    diagram.add_edge(Edge::new(START, first.id(), "")?)?;
    connect(&statements, STOP, &mut diagram)?;
    Ok(diagram)
}

fn check_interpreter(interpreter: &str) -> Result<(), ScriptError> {
    let mut parts = interpreter.split_ascii_whitespace();
    let mut shell = parts.next().and_then(|path| path.rsplit('/').next());
    if shell == Some("env") {
        shell = parts.next().and_then(|path| path.rsplit('/').next());
    }
    if matches!(shell, Some("sh" | "bash" | "dash" | "ash" | "ksh" | "zsh"))
        && parts.next().is_none()
    {
        Ok(())
    } else {
        Err(ScriptError::at(
            1,
            ScriptErrorKind::Unsupported("interpreter or interpreter options"),
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Word<'a> {
    Text(&'a str),
    Separator,
}

struct Line<'a> {
    number: usize,
    source: &'a str,
    words: Vec<Word<'a>>,
}

impl<'a> Line<'a> {
    fn head(&self) -> &'a str {
        match self.words.first() {
            Some(Word::Text(word)) => word,
            _ => "",
        }
    }

    fn is_delimiter(&self, delimiter: &str) -> bool {
        match self.words.as_slice() {
            [Word::Text(word)] => *word == delimiter,
            [Word::Text(word), Word::Separator] => {
                *word == delimiter && matches!(delimiter, "fi" | "done")
            }
            _ => false,
        }
    }

    fn error(&self, kind: ScriptErrorKind) -> ScriptError {
        ScriptError::at(self.number, kind)
    }
}

fn lex_line(number: usize, raw: &str) -> Result<Option<Line<'_>>, ScriptError> {
    let text = raw.trim_matches([' ', '\t']);
    let bytes = text.as_bytes();
    let error = |kind| ScriptError::at(number, kind);
    let mut cursor = 0;
    let mut words = Vec::new();
    while cursor < bytes.len() {
        match bytes[cursor] {
            b' ' | b'\t' => cursor += 1,
            b'#' => break,
            b';' => {
                words.push(Word::Separator);
                cursor += 1;
            }
            _ => {
                let start = cursor;
                cursor = lex_word(bytes, start).map_err(error)?;
                words.push(Word::Text(&text[start..cursor]));
            }
        }
    }
    if words.is_empty() {
        return Ok(None);
    }
    Ok(Some(Line {
        number,
        source: text[..cursor].trim_end(),
        words,
    }))
}

fn lex_word(bytes: &[u8], mut cursor: usize) -> Result<usize, ScriptErrorKind> {
    let mut quote = None;
    while let Some(&byte) = bytes.get(cursor) {
        match (quote, byte) {
            (Some(b'\''), b'\'') | (Some(b'"'), b'"') => quote = None,
            (Some(b'\''), _) => {}
            (None, b'\'' | b'"') => quote = Some(byte),
            (_, b'\\') => {
                if bytes.get(cursor + 1).is_none() {
                    return Err(ScriptErrorKind::Unsupported("line continuation"));
                }
                cursor += 1;
            }
            (_, b'`') => return Err(ScriptErrorKind::Unsupported("command substitution")),
            (_, b'$') if bytes.get(cursor + 1) == Some(&b'(') => {
                return Err(ScriptErrorKind::Unsupported(
                    "command or arithmetic substitution",
                ));
            }
            (_, b'$') if bytes.get(cursor + 1) == Some(&b'{') => {
                let expansion = &bytes[cursor + 2..];
                let closing = expansion
                    .iter()
                    .position(|&byte| byte == b'}')
                    .ok_or(ScriptErrorKind::Syntax("unclosed parameter expansion"))?;
                let name = std::str::from_utf8(&expansion[..closing])
                    .map_err(|_| ScriptErrorKind::Unsupported("complex parameter expansion"))?;
                let positional = !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit());
                let special = matches!(name, "@" | "*" | "#" | "?" | "-" | "$" | "!");
                if !(shell_identifier(name) || positional || special) {
                    return Err(ScriptErrorKind::Unsupported("complex parameter expansion"));
                }
                cursor += closing + 2;
            }
            (None, b' ' | b'\t' | b';') => break,
            (None, b'|' | b'&') => {
                return Err(ScriptErrorKind::Unsupported("pipeline or command list"));
            }
            (None, b'<' | b'>') => {
                return Err(ScriptErrorKind::Unsupported("redirection or here-document"));
            }
            (None, b'(' | b')' | b'{' | b'}') => {
                return Err(ScriptErrorKind::Unsupported(
                    "compound command or function declaration",
                ));
            }
            _ => {}
        }
        cursor += 1;
    }
    if quote.is_some() {
        Err(ScriptErrorKind::Syntax("unclosed shell quote"))
    } else {
        Ok(cursor)
    }
}

struct Statement<'a> {
    label: &'a str,
    start: usize,
    end: usize,
    kind: StatementKind<'a>,
}

impl Statement<'_> {
    fn id(&self) -> String {
        format!("line_{}", self.start)
    }
}

enum StatementKind<'a> {
    Commands(usize),
    If {
        yes: Vec<Statement<'a>>,
        no: Vec<Statement<'a>>,
    },
    Loop {
        body: Vec<Statement<'a>>,
        repeat: &'static str,
        finish: &'static str,
    },
}

struct Parser<'a, 'source> {
    lines: &'a [Line<'source>],
    offset: usize,
}

impl<'a, 'source> Parser<'a, 'source> {
    fn peek(&self) -> Option<&'a Line<'source>> {
        self.lines.get(self.offset)
    }

    fn block(&mut self, depth: usize) -> Result<Vec<Statement<'source>>, ScriptError> {
        let mut statements = Vec::new();
        while let Some(line) = self.peek() {
            if matches!(line.head(), "then" | "elif" | "else" | "fi" | "do" | "done") {
                break;
            }
            self.offset += 1;
            let statement = match line.head() {
                "if" => {
                    let conditional = self.conditional(depth, line)?;
                    self.expect("fi", line)?;
                    conditional
                }
                "while" | "until" | "for" => {
                    self.check_depth(depth, line)?;
                    let for_loop = line.head() == "for";
                    let (label, end) = self.header(line, "do", for_loop)?;
                    let body = self.required_block(depth, line)?;
                    self.expect("done", line)?;
                    let (repeat, finish) = match line.head() {
                        "for" => ("next", "done"),
                        "until" => ("no", "yes"),
                        _ => ("yes", "no"),
                    };
                    Statement {
                        label,
                        start: line.number,
                        end,
                        kind: StatementKind::Loop {
                            body,
                            repeat,
                            finish,
                        },
                    }
                }
                _ => {
                    validate_command(without_trailing_separator(&line.words))
                        .map_err(|kind| line.error(kind))?;
                    Statement {
                        label: line.source,
                        start: line.number,
                        end: line.number,
                        kind: StatementKind::Commands(1),
                    }
                }
            };
            statements.push(statement);
        }
        Ok(statements)
    }

    fn conditional(
        &mut self,
        depth: usize,
        line: &'a Line<'source>,
    ) -> Result<Statement<'source>, ScriptError> {
        self.check_depth(depth, line)?;
        let (label, end) = self.header(line, "then", false)?;
        let yes = self.required_block(depth, line)?;
        let no = match self.peek() {
            Some(next) if next.head() == "elif" => {
                self.offset += 1;
                vec![self.conditional(depth + 1, next)?]
            }
            Some(next) if next.is_delimiter("else") => {
                self.offset += 1;
                self.required_block(depth, next)?
            }
            _ => Vec::new(),
        };
        Ok(Statement {
            label,
            start: line.number,
            end,
            kind: StatementKind::If { yes, no },
        })
    }

    fn check_depth(&self, depth: usize, line: &Line<'_>) -> Result<(), ScriptError> {
        if depth >= MAX_NESTING {
            Err(line.error(ScriptErrorKind::NestingTooDeep))
        } else {
            Ok(())
        }
    }

    fn required_block(
        &mut self,
        depth: usize,
        opening: &Line<'_>,
    ) -> Result<Vec<Statement<'source>>, ScriptError> {
        let block = self.block(depth + 1)?;
        if block.is_empty() {
            Err(self
                .peek()
                .unwrap_or(opening)
                .error(ScriptErrorKind::Syntax("expected commands in shell block")))
        } else {
            Ok(block)
        }
    }

    fn header(
        &mut self,
        line: &Line<'source>,
        delimiter: &str,
        for_loop: bool,
    ) -> Result<(&'source str, usize), ScriptError> {
        let body = &line.words[1..];
        let (body, inline) = match body.split_last() {
            Some((Word::Text(word), rest))
                if *word == delimiter && rest.last() == Some(&Word::Separator) =>
            {
                (&rest[..rest.len() - 1], true)
            }
            _ => (without_trailing_separator(body), false),
        };
        if for_loop {
            validate_for(body).map_err(|kind| line.error(kind))?;
        } else {
            validate_command(body).map_err(|kind| line.error(kind))?;
        }
        if inline {
            // The checked delimiter is a literal word outside quotes.
            let label = line
                .source
                .rsplit_once(';')
                .map_or(line.source, |(label, _)| label.trim_end());
            Ok((label, line.number))
        } else {
            let end = self.expect(delimiter, line)?;
            Ok((line.source, end))
        }
    }

    fn expect(&mut self, delimiter: &str, opening: &Line<'_>) -> Result<usize, ScriptError> {
        if let Some(line) = self.peek() {
            if line.is_delimiter(delimiter) {
                self.offset += 1;
                return Ok(line.number);
            }
            return Err(line.error(ScriptErrorKind::Syntax(expected_delimiter(delimiter))));
        }
        Err(opening.error(ScriptErrorKind::Syntax(expected_delimiter(delimiter))))
    }
}

fn without_trailing_separator<'a, 'source>(words: &'a [Word<'source>]) -> &'a [Word<'source>] {
    if words.last() == Some(&Word::Separator) {
        &words[..words.len() - 1]
    } else {
        words
    }
}

fn expected_delimiter(delimiter: &str) -> &'static str {
    match delimiter {
        "then" => "expected then for if block",
        "do" => "expected do for loop",
        "done" => "expected done to close loop",
        _ => "expected fi to close if block",
    }
}

fn validate_for(words: &[Word<'_>]) -> Result<(), ScriptErrorKind> {
    let Some((Word::Text(name), rest)) = words.split_first() else {
        return Err(ScriptErrorKind::Syntax("expected a loop variable"));
    };
    if !shell_identifier(name) {
        return Err(ScriptErrorKind::Syntax("invalid loop variable"));
    }
    match rest {
        [] => Ok(()),
        [Word::Text("in"), list @ ..] if list.iter().all(|word| matches!(word, Word::Text(_))) => {
            Ok(())
        }
        _ => Err(ScriptErrorKind::Unsupported(
            "for loop outside the word-list grammar",
        )),
    }
}

fn validate_command(words: &[Word<'_>]) -> Result<(), ScriptErrorKind> {
    if words.contains(&Word::Separator) {
        return Err(ScriptErrorKind::Unsupported(
            "multiple commands on one line",
        ));
    }
    let words = match words {
        [Word::Text("!"), rest @ ..] => rest,
        _ => words,
    };
    if words.is_empty() {
        return Err(ScriptErrorKind::Syntax("expected a simple shell command"));
    }
    let command = words.iter().find_map(|word| match word {
        Word::Text(text)
            if !text
                .split_once('=')
                .is_some_and(|(name, _)| shell_identifier(name)) =>
        {
            Some(*text)
        }
        _ => None,
    });
    let Some(command) = command else {
        return Ok(());
    };
    if matches!(
        command,
        "." | "source"
            | "eval"
            | "exec"
            | "exit"
            | "logout"
            | "return"
            | "break"
            | "continue"
            | "trap"
            | "set"
            | "alias"
            | "unalias"
            | "enable"
            | "builtin"
            | "command"
            | "case"
            | "esac"
            | "select"
            | "function"
            | "elif"
            | "coproc"
            | "time"
            | "[["
            | "if"
            | "then"
            | "else"
            | "fi"
            | "while"
            | "until"
            | "for"
            | "do"
            | "done"
    ) {
        return Err(ScriptErrorKind::Unsupported(
            "shell keyword or command that can change control flow",
        ));
    }
    if command != "["
        && !command.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'/' | b'+' | b'-' | b':')
        })
    {
        return Err(ScriptErrorKind::Unsupported("dynamic command name"));
    }
    Ok(())
}

fn shell_identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn count_graph(statements: &[Statement<'_>], nodes: &mut usize, edges: &mut usize) {
    for statement in statements {
        *nodes += 1;
        match &statement.kind {
            StatementKind::Commands(_) => *edges += 1,
            StatementKind::If { yes, no } => {
                *edges += 2;
                count_graph(yes, nodes, edges);
                count_graph(no, nodes, edges);
            }
            StatementKind::Loop { body, .. } => {
                *edges += 2;
                count_graph(body, nodes, edges);
            }
        }
    }
}

fn group_commands(statements: &mut Vec<Statement<'_>>) {
    for statement in statements.iter_mut() {
        match &mut statement.kind {
            StatementKind::Commands(_) => {}
            StatementKind::If { yes, no } => {
                group_commands(yes);
                group_commands(no);
            }
            StatementKind::Loop { body, .. } => group_commands(body),
        }
    }
    statements.dedup_by(|next, previous| match (&next.kind, &mut previous.kind) {
        (StatementKind::Commands(next_count), StatementKind::Commands(previous_count)) => {
            *previous_count += next_count;
            previous.end = next.end;
            true
        }
        _ => false,
    });
}

fn add_nodes(statements: &[Statement<'_>], diagram: &mut Diagram) -> Result<(), ScriptError> {
    for statement in statements {
        let label = statement.label.replace('\t', " ");
        let (shape, mut label) = match statement.kind {
            StatementKind::Commands(1) => (NodeShape::Action, label),
            StatementKind::Commands(count) => {
                (NodeShape::Action, format!("{count} commands · {label}"))
            }
            _ => (NodeShape::Decision, label),
        };
        if label.chars().count() > MAX_LABEL_CHARS {
            label = label.chars().take(MAX_LABEL_CHARS - 1).collect::<String>() + "…";
        }
        let mut node = Node::new(statement.id(), label, shape)
            .map_err(|error| ScriptError::at(statement.start, ScriptErrorKind::Model(error)))?;
        // The script byte limit bounds the number of lines well below u32::MAX.
        if let Some(range) = SourceRange::new(statement.start as u32, statement.end as u32) {
            node = node.with_source_range(range);
        }
        diagram
            .add_node(node)
            .map_err(|error| ScriptError::at(statement.start, ScriptErrorKind::Model(error)))?;
        match &statement.kind {
            StatementKind::Commands(_) => {}
            StatementKind::If { yes, no } => {
                add_nodes(yes, diagram)?;
                add_nodes(no, diagram)?;
            }
            StatementKind::Loop { body, .. } => add_nodes(body, diagram)?,
        }
    }
    Ok(())
}

fn connect(
    statements: &[Statement<'_>],
    exit: &str,
    diagram: &mut Diagram,
) -> Result<(), ScriptError> {
    for (index, statement) in statements.iter().enumerate() {
        let successor = statements.get(index + 1).map(Statement::id);
        let successor = successor.as_deref().unwrap_or(exit);
        let id = statement.id();
        match &statement.kind {
            StatementKind::Commands(_) => diagram.add_edge(Edge::new(id, successor, "")?)?,
            StatementKind::If { yes, no } => {
                connect_branch(&id, yes, successor, "yes", diagram)?;
                connect_branch(&id, no, successor, "no", diagram)?;
            }
            StatementKind::Loop {
                body,
                repeat,
                finish,
            } => {
                connect_branch(&id, body, &id, repeat, diagram)?;
                diagram.add_edge(Edge::new(&id, successor, *finish)?)?;
            }
        }
    }
    Ok(())
}

fn connect_branch(
    source: &str,
    body: &[Statement<'_>],
    exit: &str,
    label: &str,
    diagram: &mut Diagram,
) -> Result<(), ScriptError> {
    let first = body.first().map(Statement::id);
    diagram.add_edge(Edge::new(source, first.as_deref().unwrap_or(exit), label)?)?;
    connect(body, exit, diagram)
}
