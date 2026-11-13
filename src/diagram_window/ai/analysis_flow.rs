//! Runtime-flow quality checks and source-grounded diagram fallbacks.

use std::collections::HashSet;

use kortty_diagram::{
    Arrowheads, Diagram, DiagramError, Direction, Edge, MAX_LABEL_CHARS, Node, NodeShape,
    SourceRange,
};

pub(super) fn node_budget(lines: usize) -> usize {
    12 + lines.clamp(200, 1000).saturating_sub(200) * 12 / 800
}

pub(super) fn validate(diagram: &Diagram, lines: usize) -> Result<(), String> {
    let nodes = diagram.nodes();
    let edges = diagram.edges();
    let actions = nodes
        .iter()
        .filter(|node| node.shape() != NodeShape::Terminal)
        .count();
    if actions > 3 * node_budget(lines) || nodes.len() > 3 * node_budget(lines) + 8 {
        return Err("the flowchart has too many steps for this source".into());
    }
    if edges
        .iter()
        .any(|edge| edge.arrowheads() != Arrowheads::End)
    {
        return Err("each flowchart arrow must point to its next step".into());
    }
    let targets: HashSet<_> = edges.iter().map(Edge::target).collect();
    let mut starts = nodes.iter().filter(|node| !targets.contains(node.id()));
    let Some(start) = starts
        .next()
        .filter(|node| node.shape() == NodeShape::Terminal)
    else {
        return Err("the flowchart needs one rounded start node".into());
    };
    if starts.next().is_some() || reachable(diagram, vec![start.id()], false) != nodes.len() {
        return Err("some flowchart steps are unreachable from the start".into());
    }
    let sources: HashSet<_> = edges.iter().map(Edge::source).collect();
    let mut ends = Vec::new();
    for node in nodes {
        if !sources.contains(node.id()) {
            if node.shape() != NodeShape::Terminal {
                return Err("a branch ends without a rounded outcome node".into());
            }
            ends.push(node.id());
        }
        if node.shape() == NodeShape::Decision {
            let mut branches = edges.iter().filter(|edge| edge.source() == node.id());
            let valid = branches
                .next()
                .zip(branches.next())
                .is_some_and(|(first, second)| {
                    let a = first.label().trim();
                    let b = second.label().trim();
                    first.target() != second.target()
                        && ((a.eq_ignore_ascii_case("yes") && b.eq_ignore_ascii_case("no"))
                            || (a.eq_ignore_ascii_case("no") && b.eq_ignore_ascii_case("yes")))
                });
            if !valid || branches.next().is_some() {
                return Err("a decision needs distinct yes and no branches".into());
            }
        }
    }
    if reachable(diagram, ends, true) != nodes.len() {
        return Err("some flowchart steps cannot reach an outcome".into());
    }
    Ok(())
}

fn reachable<'a>(diagram: &'a Diagram, mut pending: Vec<&'a str>, reverse: bool) -> usize {
    let mut visited: HashSet<_> = pending.iter().copied().collect();
    while let Some(node) = pending.pop() {
        for edge in diagram.edges() {
            let (from, to) = if reverse {
                (edge.target(), edge.source())
            } else {
                (edge.source(), edge.target())
            };
            if from == node && visited.insert(to) {
                pending.push(to);
            }
        }
    }
    visited.len()
}

pub(super) fn project_outline(script: &str) -> Result<Diagram, String> {
    let mut line_count = 0;
    let files: Vec<_> = script
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            line_count = index + 1;
            let path = line.strip_prefix("=== File: ")?.strip_suffix(" ===")?;
            Some((line_count, path))
        })
        .collect();
    if files.is_empty() {
        return Err("The retained project source has no selected files.".into());
    }
    let mut diagram = Diagram::new(Direction::TopToBottom);
    let build = || -> Result<Diagram, DiagramError> {
        diagram.add_node(Node::new(
            "project",
            "Selected project files",
            NodeShape::Terminal,
        )?)?;
        let group_size = files.len().div_ceil(24);
        for (index, group) in files.chunks(group_size).enumerate() {
            let (start, path) = group[0];
            let end = files
                .get((index + 1) * group_size)
                .map_or(line_count, |&(line, _)| line - 1);
            let label = if group.len() == 1 {
                path.to_owned()
            } else {
                format!("{} files beginning with {path}", group.len())
            };
            let id = format!("file{index}");
            let mut node = Node::new(
                &id,
                label.chars().take(MAX_LABEL_CHARS).collect::<String>(),
                NodeShape::Action,
            )?;
            if let Some(range) = SourceRange::new(start as u32, end as u32) {
                node = node.with_source_range(range);
            }
            diagram.add_node(node)?;
            diagram.add_edge(Edge::new("project", id, "")?)?;
        }
        Ok(diagram)
    };
    build().map_err(|error| error.to_string())
}

pub(super) fn workflow_outline(script: &str) -> Result<Diagram, DiagramError> {
    let lines = script.lines().enumerate().filter_map(|(index, line)| {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with(['#', ';'])
            || line.starts_with("//")
            || line.starts_with("--")
            || line.eq_ignore_ascii_case("@echo off")
            || line
                .get(..4)
                .is_some_and(|start| start.eq_ignore_ascii_case("rem "))
            || line == "---"
        {
            return None;
        }
        Some((index as u32 + 1, line))
    });
    let mut first = None;
    let mut last = None;
    let mut configuration = None;
    let mut condition = None;
    for (line_number, line) in lines {
        first.get_or_insert(line_number);
        last = Some(line_number);
        if configuration.is_none()
            && let Some((name, _)) = line.split_once('=')
        {
            let name = name.trim().trim_start_matches('$');
            if name
                .starts_with(|character: char| character.is_ascii_alphabetic() || character == '_')
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                configuration = Some(line_number);
            }
        }
        let prefix = line
            .split(|character: char| character.is_whitespace() || character == '(')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if condition.is_none()
            && (matches!(
                prefix.as_str(),
                "if" | "elif" | "elseif" | "unless" | "case" | "when:"
            ) || line.contains(" && ")
                || line.contains(" || "))
        {
            condition = Some(line_number);
        }
    }

    let mut chart = Diagram::new(Direction::TopToBottom);
    chart.add_node(Node::new("start", "Start", NodeShape::Terminal)?)?;
    chart.add_node(Node::new("stop", "Stop", NodeShape::Terminal)?)?;
    let body = Node::new("work", "Run main script logic", NodeShape::Action)?;
    chart.add_node(
        if let Some(range) = first
            .zip(last)
            .and_then(|(first, last)| SourceRange::new(first, last))
        {
            body.with_source_range(range)
        } else {
            body
        },
    )?;
    if let Some(range) = configuration.and_then(|line| SourceRange::new(line, line)) {
        chart.add_node(
            Node::new("setup", "Read configured values", NodeShape::Action)?
                .with_source_range(range),
        )?;
        chart.add_edge(Edge::new("start", "setup", "")?)?;
        chart.add_edge(Edge::new("setup", "work", "")?)?;
    } else {
        chart.add_edge(Edge::new("start", "work", "")?)?;
    }
    if let Some(range) = condition.and_then(|line| SourceRange::new(line, line)) {
        chart.add_node(
            Node::new("decision", "Does the condition hold?", NodeShape::Decision)?
                .with_source_range(range),
        )?;
        chart.add_node(Node::new(
            "yes",
            "Follow the matching path",
            NodeShape::Action,
        )?)?;
        chart.add_node(Node::new("no", "Follow the other path", NodeShape::Action)?)?;
        for (from, to, label) in [
            ("work", "decision", ""),
            ("decision", "yes", "yes"),
            ("decision", "no", "no"),
            ("yes", "stop", ""),
            ("no", "stop", ""),
        ] {
            chart.add_edge(Edge::new(from, to, label)?)?;
        }
    } else {
        chart.add_edge(Edge::new("work", "stop", "")?)?;
    }
    Ok(chart)
}

#[cfg(test)]
mod tests {
    use kortty_diagram::parse_mermaid;

    use super::*;

    #[test]
    fn accepts_distinct_outcomes_but_rejects_unreachable_or_trapped_steps() {
        let valid = "flowchart TD\nstart([\"Start\"]) --> a{\"Ready?\"}\na -->|yes| end1([\"Success\"])\na -->|no| end2([\"Failure\"])";
        assert!(validate(&parse_mermaid(valid).unwrap(), 20).is_ok());
        for suffix in [
            "\nisolated[Isolated]",
            "\nstart --> loop1[Loop]\nloop1 --> loop1",
        ] {
            assert!(validate(&parse_mermaid(&format!("{valid}{suffix}")).unwrap(), 20).is_err());
        }
    }

    #[test]
    fn project_fallback_uses_selected_file_boundaries() {
        let script = "Project folder: Ops\n=== File: tools/check.sh ===\n1 | echo check\n\n=== File: setup.sh ===\n1 | echo setup\n";
        let graph = project_outline(script).unwrap();
        assert_eq!(graph.nodes().len(), 3);
        assert_eq!(graph.nodes()[1].label(), "tools/check.sh");
        assert_eq!(graph.nodes()[1].source_range(), SourceRange::new(2, 4));
        assert_eq!(graph.nodes()[2].source_range(), SourceRange::new(5, 6));
    }
}
