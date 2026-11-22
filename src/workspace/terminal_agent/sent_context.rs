//! A run-owned view of the masked context included in a confirmed model turn.

use iced::widget::{button, column, container, scrollable, text};
use iced::{Element, Length};
use zeroize::Zeroizing;

use crate::ai_manager::{MemoryOrigin, MemoryPrompt, SkillGuidance};
use crate::local_ai::Tokenizer;
use crate::terminal_agent::usage::{AttemptKind, AttemptOutcome, Report};
use crate::workspace::chrome::popup;

use super::{Message, Run, design};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Surface {
    Terminal,
    Activity,
}

pub(super) struct Snapshot {
    rows: Vec<Entry>,
    omitted: usize,
}

struct Entry {
    kind: Kind,
    name: Zeroizing<String>,
    body: Zeroizing<String>,
    tokens: u64,
    partial: bool,
}

#[derive(Clone, Copy)]
enum Kind {
    Memory(MemoryOrigin),
    Skill { pinned: bool },
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Memory(MemoryOrigin::Organization) => "Organization memory",
            Self::Memory(MemoryOrigin::Global) => "Global memory",
            Self::Memory(MemoryOrigin::Connection) => "Connection memory",
            Self::Memory(MemoryOrigin::External) => "External memory file",
            Self::Skill { pinned: true } => "Pinned skill",
            Self::Skill { pinned: false } => "Automatic skill",
        }
    }
}

fn memory_text(origin: MemoryOrigin, wire: &str) -> String {
    if origin == MemoryOrigin::Organization {
        return wire.trim_end().into();
    }
    wire.lines()
        .map(|line| serde_json::from_str::<String>(line).unwrap_or_else(|_| line.to_owned()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl Snapshot {
    pub(super) fn capture(
        memory: Option<&MemoryPrompt>,
        guidance: &SkillGuidance,
        system: &str,
        tokenizer: Tokenizer,
        mask: impl Fn(&str) -> Result<String, String>,
    ) -> Option<Self> {
        let mut rows = Vec::new();
        if let Some(memory) = memory {
            for (origin, wire, file, partial) in memory.pieces() {
                if !system.contains(wire) {
                    continue;
                }
                rows.push(Entry {
                    kind: Kind::Memory(origin),
                    name: Zeroizing::new(file.and_then(|name| mask(name).ok()).unwrap_or_default()),
                    body: Zeroizing::new(memory_text(origin, wire)),
                    tokens: tokenizer.count_text(wire),
                    partial,
                });
            }
        }
        for (_, pinned, original) in guidance.pieces() {
            let Ok(wire) = mask(original).map(Zeroizing::new) else {
                continue;
            };
            let Some((name, body)) = wire
                .trim_start()
                .strip_prefix("## ")
                .and_then(|block| block.split_once('\n'))
            else {
                continue;
            };
            if !system.contains(wire.as_str()) {
                continue;
            }
            rows.push(Entry {
                kind: Kind::Skill { pinned },
                name: Zeroizing::new(name.into()),
                body: Zeroizing::new(body.into()),
                tokens: tokenizer.count_text(&wire),
                partial: false,
            });
        }
        (!rows.is_empty()).then(|| Self {
            rows,
            omitted: memory.map_or(0, MemoryPrompt::omitted),
        })
    }

    pub(super) fn remask(mut self, redactor: &crate::terminal_logging::Redactor) -> Option<Self> {
        for row in &mut self.rows {
            row.name = Zeroizing::new(super::mask::prompt(redactor, &row.name).ok()?);
            row.body = Zeroizing::new(super::mask::prompt(redactor, &row.body).ok()?);
        }
        Some(self)
    }

    pub(super) fn summary(&self) -> String {
        let mut counts = [0; 3];
        let mut tokens = 0u64;
        for row in &self.rows {
            counts[match row.kind {
                Kind::Memory(MemoryOrigin::External) => 2,
                Kind::Memory(_) => 0,
                Kind::Skill { .. } => 1,
            }] += 1;
            tokens = tokens.saturating_add(row.tokens);
        }
        let labels = [
            ("memory entry", "memory entries"),
            ("skill", "skills"),
            ("file", "files"),
        ];
        let parts = counts
            .into_iter()
            .zip(labels)
            .filter(|(count, _)| *count > 0)
            .map(|(count, (one, many))| format!("{count} {}", if count == 1 { one } else { many }))
            .chain(std::iter::once(format!("~{tokens} tokens")))
            .collect::<Vec<_>>();
        format!("Sent along · {}", parts.join(" · "))
    }
}

fn confirmed<T>(report: &Report<T>) -> bool {
    report.attempts.iter().any(|attempt| {
        attempt.kind == AttemptKind::Initial
            && matches!(
                attempt.outcome,
                AttemptOutcome::Completed
                    | AttemptOutcome::InvalidReply
                    | AttemptOutcome::TokenLimit
                    | AttemptOutcome::LikelyTokenLimit
            )
    })
}

impl Run {
    pub(super) fn record_sent_context<T>(&mut self, report: &Report<T>) {
        if confirmed(report)
            && let Some(snapshot) = self
                .model_review
                .as_mut()
                .and_then(|review| review.sent_context.take())
        {
            self.presentation.sent_context = Some(snapshot);
        }
    }
}

pub(super) fn view(run: &Run, surface: Surface) -> Option<Element<'_, Message>> {
    let snapshot = run.presentation.sent_context.as_ref()?;
    let open = run.presentation.sent_context_open == Some(surface);
    let message = |show| Message::Design(design::Message::SentContext(surface, show));
    let trigger = button(text(snapshot.summary()).size(12))
        .style(button::text)
        .on_press(message(!open))
        .into();
    let contents = open.then(|| {
        let mut entries =
            column![text("Context sent with the latest reviewed request").size(15)].spacing(8);
        if snapshot.omitted > 0 {
            entries = entries.push(
                text(format!(
                    "{} memory fragments omitted from the request",
                    snapshot.omitted
                ))
                .size(12),
            );
        }
        for entry in &snapshot.rows {
            let label = entry.kind.label();
            let name = if entry.name.is_empty() {
                String::new()
            } else {
                format!(" · {}", entry.name.as_str())
            };
            let partial = if entry.partial { " · Excerpt" } else { "" };
            entries = entries
                .push(text(format!("{label}{name}{partial} · ~{} tokens", entry.tokens)).size(12));
            entries = entries.push(text(entry.body.as_str()).size(13));
        }
        container(scrollable(entries).height(Length::Fill))
            .padding(10)
            .width(520)
            .height(322)
            .style(container::bordered_box)
            .into()
    });
    Some(popup::popover(trigger, contents, message(false)))
}
