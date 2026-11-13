use super::*;

fn segment(text: impl Into<String>, role: Role) -> Segment {
    Segment {
        text: text.into(),
        role,
    }
}

fn line(parts: Vec<Segment>, width: usize) -> Line {
    let mut result = Line::default();
    let total: usize = parts.iter().map(|part| columns(&part.text)).sum();
    let limit = width.saturating_sub(usize::from(total > width));
    let mut used = 0;
    for part in parts {
        let mut text = String::new();
        let mut clipped = false;
        for ch in part.text.chars() {
            let cells = kortty_terminal::character_cell_width(ch).unwrap_or(0);
            if used + cells > limit {
                clipped = true;
                break;
            }
            if cells == 0 && used == 0 {
                continue;
            }
            text.push(ch);
            used += cells;
        }
        if !text.is_empty() {
            result.segments.push(segment(text, part.role));
        }
        if clipped || used >= limit {
            break;
        }
    }
    if total > width && width > 0 {
        result.segments.push(segment("…", Role::Muted));
    }
    result
}

fn justify(left: Vec<Segment>, right: Vec<Segment>, width: usize) -> Line {
    let right = line(right, width.saturating_sub(4));
    let left_width = width.saturating_sub(right.columns() + 1);
    let mut left = line(left, left_width);
    let padding = width.saturating_sub(left.columns() + right.columns());
    left.segments.push(segment(" ".repeat(padding), Role::Text));
    left.segments.extend(right.segments);
    left
}

fn wrap(value: &str, width: usize, maximum: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for ch in clean(value, true).chars() {
        let cells = kortty_terminal::character_cell_width(ch).unwrap_or(0);
        if ch == '\n' || used + cells > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
            if lines.len() == maximum {
                return lines;
            }
            if ch == '\n' {
                continue;
            }
        }
        if cells <= width && (cells > 0 || used > 0) {
            current.push(ch);
            used += cells;
        }
    }
    if !current.is_empty() && lines.len() < maximum {
        lines.push(current);
    }
    lines
}

fn elapsed(milliseconds: u64) -> String {
    let seconds = milliseconds / 1000;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

fn meta(milliseconds: u64, tokens: Option<u64>) -> String {
    let mut text = elapsed(milliseconds);
    if let Some(tokens) = tokens {
        text.push_str(&format!(" · {tokens} tokens"));
    }
    text
}

fn marker(step: &Step, spinner: &str) -> Segment {
    if step.state == StepState::Failed || step.kind == Kind::Error {
        return segment("✕", Role::Error);
    }
    if step.state == StepState::Cancelled {
        return segment("–", Role::Muted);
    }
    let running = step.state == StepState::Running;
    match step.kind {
        Kind::Action => segment(
            if running { spinner } else { "✓" },
            if running { Role::Accent } else { Role::Ok },
        ),
        Kind::Thinking => segment(if running { spinner } else { "◇" }, Role::Think),
        Kind::Question => segment(
            if running { "?" } else { "✓" },
            if running { Role::Warn } else { Role::Ok },
        ),
        Kind::Message => segment("•", Role::Muted),
        Kind::Error => segment("✕", Role::Error),
    }
}

fn steps(model: &Transcript) -> impl Iterator<Item = &Step> {
    model
        .steps
        .iter()
        .rev()
        .take(MAX_STEPS)
        .rev()
        .filter(|step| {
            !(model.prompt.is_some()
                && step.kind == Kind::Question
                && step.state == StepState::Running)
                && !(step.kind == Kind::Message
                    && (step.title.trim().is_empty()
                        || step.title.trim() == model.goal.trim()
                        || model.state.finished() && !model.expanded))
        })
}

fn keys(always: bool) -> Vec<Segment> {
    let mut parts = vec![
        segment("1 ", Role::Key),
        segment("Approve once", Role::Text),
    ];
    if always {
        parts.extend([
            segment(" · 2 ", Role::Key),
            segment("Always allow", Role::Text),
        ]);
    }
    parts.extend([
        segment(if always { " · 3 " } else { " · 2 " }, Role::Key),
        segment("Cancel", Role::Text),
    ]);
    parts
}

fn prompt(lines: &mut Vec<Line>, prompt: &Prompt, style: Style, width: usize) {
    // Retain the complete bounded review footer even when expanded activity is long.
    if lines.len() > MAX_LINES - 21 {
        lines.truncate(MAX_LINES - 22);
        lines.push(line(
            vec![segment(
                "… Earlier activity is available in the Activity window.",
                Role::Muted,
            )],
            width,
        ));
    }
    let prefix = if style == Style::Inline { "│ " } else { "  " };
    match prompt {
        Prompt::Approval {
            message,
            commands,
            allow_always,
            keyboard,
        } => {
            for (index, text) in wrap(message, width.saturating_sub(4), 3)
                .into_iter()
                .enumerate()
            {
                lines.push(line(
                    vec![
                        segment(prefix, Role::Border),
                        segment(if index == 0 { "? " } else { "  " }, Role::Warn),
                        segment(text, Role::Text),
                    ],
                    width,
                ));
            }
            for command in commands.iter().take(16) {
                lines.push(line(
                    vec![
                        segment(prefix, Role::Border),
                        segment("$ ", Role::Accent),
                        segment(clean(command, false), Role::Title),
                    ],
                    width,
                ));
            }
            let mut footer = vec![segment(
                if style == Style::Inline {
                    "╰─ "
                } else {
                    "  "
                },
                Role::Border,
            )];
            if *keyboard {
                footer.extend(keys(*allow_always));
            } else {
                footer.push(segment("Review in the Activity window", Role::Muted));
            }
            lines.push(line(footer, width));
        }
        Prompt::Password { summary } => {
            lines.push(line(
                vec![
                    segment(prefix, Role::Border),
                    segment("? ", Role::Warn),
                    segment(clean(summary, false), Role::Text),
                ],
                width,
            ));
            lines.push(line(
                vec![
                    segment(
                        if style == Style::Inline {
                            "╰─ "
                        } else {
                            "  "
                        },
                        Role::Border,
                    ),
                    segment(
                        "Enter the password in the owned password dialog.",
                        Role::Muted,
                    ),
                ],
                width,
            ));
        }
    }
}

fn answer(model: &Transcript) -> &str {
    if !model.final_answer.trim().is_empty() {
        return &model.final_answer;
    }
    match model.state {
        State::Cancelled => "Run cancelled.",
        State::Failed => "Run stopped with an error.",
        _ => "Run completed.",
    }
}

pub(crate) fn render(style: Style, model: &Transcript, columns: usize, spinner: &str) -> Vec<Line> {
    // Narrow panes never produce lines wider than their physical grid.
    let available = columns.clamp(1, 512);
    let width = match style {
        Style::Inline => available.saturating_sub(6).clamp(32, 120).min(available),
        Style::Focus => available.saturating_sub(2).max(32).min(available),
    };
    let spinner = clean(spinner, false);
    let spinner = if spinner.is_empty() { "…" } else { &spinner };
    let mut lines = match style {
        Style::Inline => inline(model, width, spinner),
        Style::Focus => focus(model, width, spinner),
    };
    if lines.len() > MAX_LINES {
        lines.truncate(MAX_LINES - 1);
        lines.push(line(
            vec![segment(
                "… Further activity is available in the activity window.",
                Role::Muted,
            )],
            width,
        ));
    }
    lines
}

fn inline(model: &Transcript, width: usize, spinner: &str) -> Vec<Line> {
    let mut status = Vec::new();
    if model.state.finished() {
        let (label, role) = match model.state {
            State::Failed => ("✕ Failed", Role::Error),
            State::Cancelled => ("– Cancelled", Role::Muted),
            _ => ("✓", Role::Ok),
        };
        status.push(segment(label, role));
        let commands = model
            .steps
            .iter()
            .filter(|step| step.command.is_some())
            .count();
        status.push(segment(format!(" {commands} commands · "), Role::Muted));
    } else {
        status.push(segment(
            if model.state == State::Paused {
                "⏸ Paused"
            } else {
                spinner
            },
            Role::Accent,
        ));
        status.push(segment(" ", Role::Text));
    }
    status.push(segment(meta(model.elapsed_ms, model.tokens), Role::Muted));
    let mut lines = vec![justify(
        vec![
            segment("╭─ ", Role::Border),
            segment("◆ ", Role::Accent),
            segment("AI Agent", Role::Title),
            segment(format!(" · {}", clean(&model.goal, false)), Role::Text),
        ],
        status,
        width,
    )];
    if !model.state.finished() || model.expanded {
        for step in steps(model) {
            let mut left = vec![
                segment("│ ", Role::Border),
                marker(step, spinner),
                segment(" ", Role::Text),
            ];
            left.push(segment(
                clean(&step.title, false),
                if step.kind == Kind::Message {
                    Role::Muted
                } else {
                    Role::Text
                },
            ));
            lines.push(justify(
                left,
                vec![segment(meta(step.elapsed_ms, step.tokens), Role::Muted)],
                width,
            ));
            if model.expanded
                && !model.state.finished()
                && matches!(step.kind, Kind::Thinking | Kind::Action)
            {
                for detail in wrap(&step.detail, width.saturating_sub(4), 6) {
                    lines.push(line(
                        vec![segment("│   ", Role::Border), segment(detail, Role::Muted)],
                        width,
                    ));
                }
            }
        }
    }
    if model.state.finished() {
        for text in wrap(answer(model), width.saturating_sub(2), MAX_LINES) {
            lines.push(line(
                vec![segment("│ ", Role::Border), segment(text, Role::Text)],
                width,
            ));
        }
        lines.push(line(vec![segment("╰─", Role::Border)], width));
    } else if let Some(review) = &model.prompt {
        prompt(&mut lines, review, Style::Inline, width);
    } else {
        lines.push(line(
            vec![
                segment("╰─ ", Role::Border),
                segment("Esc cancels · Ctrl+R details", Role::Muted),
            ],
            width,
        ));
    }
    lines
}

fn focus(model: &Transcript, width: usize, spinner: &str) -> Vec<Line> {
    let commands = model
        .steps
        .iter()
        .filter(|step| step.command.is_some())
        .count();
    let mut summary = vec![
        segment("  ◆ ", Role::Accent),
        segment("AI Agent", Role::Title),
        segment(format!(" · {commands} commands"), Role::Muted),
    ];
    if let Some(exit) = model.steps.iter().rev().find_map(|step| {
        (step.command.is_some() && step.state != StepState::Running)
            .then_some(step.exit_code)
            .flatten()
    }) {
        summary.push(segment(
            format!(" · Exit {exit}"),
            if exit == 0 { Role::Ok } else { Role::Error },
        ));
    }
    summary.push(segment(
        format!(" · {}", meta(model.elapsed_ms, model.tokens)),
        Role::Muted,
    ));
    if !model.state.finished() {
        summary.push(segment(
            format!(
                "  {}",
                if model.state == State::Paused {
                    "⏸ Paused"
                } else {
                    spinner
                }
            ),
            Role::Accent,
        ));
    }
    let mut lines = vec![line(summary, width)];
    if !model.goal.trim().is_empty() {
        lines.push(line(
            vec![
                segment("  › ", Role::Accent),
                segment(clean(&model.goal, false), Role::Text),
            ],
            width,
        ));
    }
    for step in steps(model) {
        if let Some(command) = &step.command {
            let mut parts = vec![
                segment("  $ ", Role::Accent),
                segment(clean(command, false), Role::Title),
            ];
            if step.state == StepState::Running {
                parts.push(segment(
                    format!("  {spinner} {}", meta(step.elapsed_ms, step.tokens)),
                    Role::Muted,
                ));
            }
            lines.push(line(parts, width));
            if step.state != StepState::Running {
                for output in step.output.iter().rev().take(3).rev() {
                    lines.push(line(
                        vec![segment(
                            format!("    {}", clean(output, false)),
                            Role::Muted,
                        )],
                        width,
                    ));
                }
                if let Some(exit) = step.exit_code.filter(|&exit| exit != 0) {
                    lines.push(line(
                        vec![segment(format!("    Exit {exit}"), Role::Error)],
                        width,
                    ));
                }
            }
        } else {
            lines.push(line(
                vec![
                    segment("  ", Role::Text),
                    marker(step, spinner),
                    segment(
                        format!(" {}", clean(&step.title, false)),
                        if step.kind == Kind::Thinking {
                            Role::Think
                        } else {
                            Role::Text
                        },
                    ),
                ],
                width,
            ));
        }
    }
    if !model.state.finished()
        && let Some(review) = &model.prompt
    {
        prompt(&mut lines, review, Style::Focus, width);
    }
    if model.state.finished() {
        for (index, text) in wrap(answer(model), width.saturating_sub(4), MAX_LINES)
            .into_iter()
            .enumerate()
        {
            lines.push(line(
                vec![
                    segment(if index == 0 { "  ◆ " } else { "    " }, Role::Accent),
                    segment(text, Role::Text),
                ],
                width,
            ));
        }
    }
    lines
}
