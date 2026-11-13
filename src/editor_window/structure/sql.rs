//! A bounded lexical SQL outline for CREATE objects and DECLARE / SET variables.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use super::{Analysis, Language, Masker, Scan, lines, masked};

static CREATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i-u)^\s*create\s+(?:or\s+replace\s+)?(?:(?:temporary|temp)\s+)?",
        r"(function|procedure|trigger|view|table)\s+(?:if\s+not\s+exists\s+)?([a-z_][a-z0-9_.]*)",
    ))
    .expect("SQL CREATE pattern")
});
static DECLARE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)\bdeclare\s+(@[a-z_][a-z0-9_]*)").expect("SQL DECLARE pattern")
});
static SET: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)^\s*set\s+(@[a-z_][a-z0-9_]*)\s*=").expect("SQL SET pattern")
});
static WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:[#@]?[A-Za-z_$][\w$]*)").expect("SQL reference pattern"));

pub(super) fn scan(source: &str) -> Analysis {
    let mut scan = Scan::default();
    let mut lexer = Masker::default();
    let mut pending = 0;
    let mut last_line = 0;
    for (line, raw) in lines(source).enumerate() {
        last_line = line;
        let (_, code) = masked(raw, &mut lexer, Language::Sql, false);
        if let Some(create) = CREATE.captures(&code) {
            let kind = match create[1].to_ascii_lowercase().as_str() {
                "function" => "sql function",
                "procedure" => "sql procedure",
                "trigger" => "sql trigger",
                "view" => "sql view",
                _ => "sql table",
            };
            let name = create.get(2).expect("SQL object name");
            scan.node(kind, name.as_str(), line, name.start(), None, ".");
        }
        // End all pending objects at the next unmasked semicolon. Each row
        // is visited once even if the source contains many unterminated objects.
        if code.contains(';') {
            for node in &mut scan.analysis.nodes[pending..] {
                node.end_line = line;
            }
            pending = scan.analysis.nodes.len();
        }
        for captures in DECLARE.captures_iter(&code).chain(SET.captures_iter(&code)) {
            let name = captures.get(1).expect("SQL variable name");
            scan.binding(
                ("variable", None),
                name.as_str(),
                line,
                name.start(),
                None,
                Language::Sql,
            );
        }
    }
    for node in &mut scan.analysis.nodes[pending..] {
        node.end_line = last_line + usize::from(source.ends_with(['\r', '\n']));
    }
    count_uses(source, &mut scan.analysis);
    scan.analysis
        .variables
        .sort_by_key(|variable| (variable.line, variable.column));
    scan.analysis
}

fn count_uses(source: &str, analysis: &mut Analysis) {
    if analysis.variables.is_empty() {
        return;
    }
    let mut counts: HashMap<&str, usize> = analysis
        .variables
        .iter()
        .map(|variable| (variable.name.as_str(), 0))
        .collect();
    let mut lexer = Masker::default();
    for raw in lines(source) {
        let (_, code) = masked(raw, &mut lexer, Language::Sql, false);
        for word in WORD.find_iter(&code) {
            let Some(count) = counts.get_mut(word.as_str()) else {
                continue;
            };
            let before = &code[..word.start()];
            let after = &code[word.end()..];
            if before
                .chars()
                .next_back()
                .is_some_and(|ch| ch == '_' || ch.is_alphanumeric())
                || before.trim_end_matches(' ').ends_with('.')
                || after.trim_start_matches(' ').starts_with('(')
            {
                continue;
            }
            *count += 1;
        }
    }
    let uses: Vec<_> = analysis
        .variables
        .iter()
        .map(|variable| counts[variable.name.as_str()].saturating_sub(1))
        .collect();
    for (variable, uses) in analysis.variables.iter_mut().zip(uses) {
        variable.uses = uses;
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Status, analyze};

    #[test]
    fn sql_objects_and_declared_variables_match_the_pinned_fixture() {
        let source = concat!(
            "-- CREATE TABLE commented_out (id INT);\n",
            "CREATE TABLE IF NOT EXISTS users (\n",
            "  id INT\n",
            ");\n",
            "CREATE OR REPLACE VIEW active_users AS SELECT * FROM users;\n",
            "DECLARE @limit INT = 5;\n",
            "SELECT TOP (@limit) * FROM users;\n",
        );
        let analysis = analyze(source, "sql");
        assert_eq!(analysis.status, Status::Ok);
        assert!(!analysis.limited);
        assert_eq!(
            analysis
                .nodes
                .iter()
                .map(|node| (node.kind, node.name.as_str(), node.line, node.end_line))
                .collect::<Vec<_>>(),
            [
                ("sql table", "users", 1, 3),
                ("sql view", "active_users", 4, 4)
            ],
        );
        let [variable] = analysis.variables.as_slice() else {
            panic!("expected one variable");
        };
        assert_eq!(variable.name, "@limit");
        assert_eq!(variable.uses, 1);
        assert!(variable.scope.is_empty());
        assert_eq!(
            (variable.line, variable.column, variable.end_column),
            (5, 8, 14)
        );
    }

    #[test]
    fn sql_masks_comments_and_literals_without_moving_source_columns() {
        let source = concat!(
            "/* CREATE TABLE fake;\n",
            "DECLARE @fake INT; */\r\n",
            "/* ä */ CrEaTe Or RePlAcE TeMp VIEW IF NOT EXISTS schema.same AS\r\n",
            "  SELECT '@name '' DECLARE @fake;', \"@name;\"\r\n",
            "  WHERE @name = 1; -- @name\r\n",
            "DECLARE @name INT = 1; DECLARE @Other INT;\r\n",
            "SET @name = @Other;\r\n",
            "SELECT @name, @other, table_alias.@name, @name(1);\r\n",
            "SET @added = @name;\r\n",
            "CREATE OR REPLACE VIEW schema.same AS SELECT @added\r\n",
        );
        let analysis = analyze(source, "SQL");
        assert_eq!(analysis.nodes.len(), 2);
        let [first, second] = analysis.nodes.as_slice() else {
            unreachable!();
        };
        assert_eq!((first.line, first.end_line), (2, 4));
        assert_eq!((second.line, second.end_line), (9, 10));
        let line = source.lines().nth(first.line).unwrap();
        assert_eq!(&line[first.column..first.end_column], "schema.same");
        assert_eq!(
            analysis
                .variables
                .iter()
                .map(|variable| (variable.name.as_str(), variable.line, variable.uses))
                .collect::<Vec<_>>(),
            [("@name", 5, 4), ("@Other", 5, 1), ("@added", 8, 1)],
        );
    }
}
