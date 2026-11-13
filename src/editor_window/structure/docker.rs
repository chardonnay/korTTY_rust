//! A bounded Dockerfile outline for stages and their ARG and ENV declarations.

use std::collections::HashMap;

use super::{Analysis, Language, Masker, Scan, identifier, lines, masked, references};

pub(super) fn scan(source: &str) -> Analysis {
    let mut scan = Scan::default();
    let mut lexer = Masker::default();
    let mut stage: Option<usize> = None;
    let mut last = 0;
    for (line, raw) in lines(source).enumerate() {
        last = line;
        let (_, code) = masked(raw, &mut lexer, Language::Dockerfile, false);
        let mut words = code.split_whitespace();
        let Some(command) = words.next() else {
            continue;
        };
        if command.eq_ignore_ascii_case("FROM") {
            let Some(image) = words.find(|word| !word.starts_with("--")) else {
                continue;
            };
            let name = if words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("AS"))
            {
                words.next().unwrap_or(image)
            } else {
                image
            };
            if let Some(previous) = stage.take() {
                scan.analysis.nodes[previous].end_line = line.saturating_sub(1);
            }
            stage = scan.node("stage", name, line, token_column(&code, name), None, ".");
            if stage.is_none() {
                break;
            }
        } else if command.eq_ignore_ascii_case("ARG") {
            if let Some(word) = words.next() {
                let name = word.split_once('=').map_or(word, |(name, _)| name);
                bind(&mut scan, "parameter", name, &code, line, stage);
            }
        } else if command.eq_ignore_ascii_case("ENV")
            && let Some(first) = words.next()
        {
            if first.contains('=') {
                for (name, _) in std::iter::once(first)
                    .chain(words)
                    .filter_map(|word| word.split_once('='))
                {
                    bind(&mut scan, "exported", name, &code, line, stage);
                }
            } else {
                bind(&mut scan, "exported", first, &code, line, stage);
            }
        }
    }
    if let Some(stage) = stage {
        scan.analysis.nodes[stage].end_line = last + usize::from(source.ends_with(['\r', '\n']));
    }
    count_uses(source, &mut scan);
    scan.analysis
}

fn count_uses(source: &str, scan: &mut Scan) {
    if scan.analysis.variables.is_empty() {
        return;
    }
    // A stage declaration shadows the global name throughout that stage, including FROM.
    let mut global = HashMap::new();
    let mut scoped = vec![HashMap::new(); scan.analysis.nodes.len()];
    for (index, (variable, stage)) in scan
        .analysis
        .variables
        .iter()
        .zip(&scan.binding_scopes)
        .enumerate()
    {
        let names = match stage {
            Some(stage) => &mut scoped[*stage],
            None => &mut global,
        };
        names.insert(variable.name.as_str(), index);
    }
    let mut uses = vec![0; scan.analysis.variables.len()];
    let mut stages = scan.analysis.nodes.iter().enumerate().peekable();
    let mut current = None;
    let mut lexer = Masker::default();
    for (line, raw) in lines(source).enumerate() {
        if let Some(&(index, stage)) = stages.peek()
            && stage.line == line
        {
            current = Some((index, stage.end_line));
            stages.next();
        }
        let local = current
            .filter(|&(_, end)| line <= end)
            .map(|(index, _)| &scoped[index]);
        let (_, code) = masked(raw, &mut lexer, Language::Dockerfile, true);
        for (_, name) in references(&code, Language::Dockerfile) {
            if let Some(&index) = local
                .and_then(|names| names.get(name))
                .or_else(|| global.get(name))
            {
                uses[index] += 1;
            }
        }
    }
    for (variable, uses) in scan.analysis.variables.iter_mut().zip(uses) {
        variable.uses = uses;
    }
}

fn bind(
    scan: &mut Scan,
    kind: &'static str,
    name: &str,
    code: &str,
    line: usize,
    stage: Option<usize>,
) {
    if name.is_ascii() && identifier(name).is_some_and(|(_, rest)| rest.is_empty()) {
        scan.binding(
            (kind, None),
            name,
            line,
            token_column(code, name),
            stage,
            Language::Dockerfile,
        );
    }
}

fn token_column(line: &str, token: &str) -> usize {
    // Each token is a slice of the masked line, which preserves the source byte offsets.
    token.as_ptr().addr() - line.as_ptr().addr()
}

#[cfg(test)]
mod tests {
    use super::super::{Status, analyze};

    #[test]
    fn dockerfile_spans_point_to_tokens_even_when_text_appears_earlier() {
        let from = "FROM --platform=build build AS build";
        let env = "ENV FOO=BAR BAR=1";
        let report = analyze(&format!("{from}\n{env}\n"), "dockerfile");
        let stage = &report.nodes[0];
        assert_eq!(&from[stage.column..stage.end_column], "build");
        assert_eq!(stage.column, from.rfind("build").unwrap());
        let variable = &report.variables[1];
        assert_eq!(&env[variable.column..variable.end_column], "BAR");
        assert_eq!(variable.column, env.rfind("BAR").unwrap());
    }

    #[test]
    fn dockerfile_structure_matches_pinned_stages_and_variable_uses() {
        // SnippetStructureScannerTest.dockerfileStagesWithArgsAndEnv in Java 2.26.0.
        let source = "ARG BASE=alpine\nFROM ${BASE} AS build\nENV APP_HOME=/app MODE=prod\nRUN make -C $APP_HOME\nFROM alpine\nENV LEGACY value\n";
        let report = analyze(source, "dockerfile");
        assert_eq!(report.status, Status::Ok);
        assert_eq!(
            report
                .nodes
                .iter()
                .map(|node| (
                    node.kind,
                    node.name.as_str(),
                    node.line + 1,
                    node.end_line + 1
                ))
                .collect::<Vec<_>>(),
            [("stage", "build", 2, 4), ("stage", "alpine", 5, 7)]
        );
        assert_eq!(
            report
                .variables
                .iter()
                .map(|variable| (
                    variable.kind,
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.line + 1,
                    variable.uses
                ))
                .collect::<Vec<_>>(),
            [
                ("parameter", "BASE", "", 1, 1),
                ("exported", "APP_HOME", "build", 3, 1),
                ("exported", "MODE", "build", 3, 0),
                ("exported", "LEGACY", "alpine", 6, 0)
            ]
        );
        for node in &report.nodes {
            assert_eq!(
                &source.lines().nth(node.line).unwrap()[node.column..node.end_column],
                node.name
            );
        }
        for variable in &report.variables {
            assert_eq!(
                &source.lines().nth(variable.line).unwrap()[variable.column..variable.end_column],
                variable.name
            );
        }
    }

    #[test]
    fn dockerfile_arg_shadows_the_global_name_throughout_its_stage() {
        let source = "ARG BASE=alpine\nFROM ${BASE} AS build\nRUN echo $BASE\nARG BASE\nARG BASE=again\nRUN echo $BASE\nFROM ${BASE}\nRUN echo $BASE\n";
        let report = analyze(source, "dockerfile");
        assert_eq!(
            report
                .variables
                .iter()
                .map(|variable| (
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.line,
                    variable.uses
                ))
                .collect::<Vec<_>>(),
            [("BASE", "", 0, 2), ("BASE", "build", 3, 3)]
        );
    }

    #[test]
    fn dockerfile_structure_masks_quoted_values_comments_and_preserves_stage_scope() {
        let source = "arg BASE=alpine\r\nFrOm --platform=linux/amd64 ${BASE} aS Build\r\nENV TARGET=\"/tmp fake=1\" SECOND=on\r\nRUN echo \"$TARGET\" '$TARGET' \"$SECOND\" \\$TARGET # $TARGET\r\n# FROM ignored\r\nFROM base\r\nARG TARGET=second\r\nRUN echo ${TARGET}\r\n";
        let report = analyze(source, "Dockerfile");
        assert_eq!(
            report
                .nodes
                .iter()
                .map(|node| node.name.as_str())
                .collect::<Vec<_>>(),
            ["Build", "base"]
        );
        assert_eq!(
            report
                .variables
                .iter()
                .map(|variable| (
                    variable.name.as_str(),
                    variable.scope.as_str(),
                    variable.uses
                ))
                .collect::<Vec<_>>(),
            [
                ("BASE", "", 1),
                ("TARGET", "Build", 1),
                ("SECOND", "Build", 1),
                ("TARGET", "base", 1)
            ]
        );
    }
}
