use std::str::FromStr;

use crate::workspace::terminal_ai::agent_follow_up::{Step, StepKind, StepStatus};

use super::{
    Fields, Node, Redactor, Write, children, element, fields, identifier_is_safe, invalid, io,
    masked_text, safe_hint, store, value,
};

const ORIGIN: &str = "AGENT_RUN";
const LINK_FIELDS: &[&str] = &[
    "sourceTerminalSessionId",
    "sourceConnectionId",
    "sourceJournalId",
    "sourceJournalEntryId",
];
const MAX_STEPS: usize = 24;
const MAX_OUTPUT_LINES: usize = 24;
const MAX_OUTPUT_LINE_BYTES: usize = 2048;
const MAX_OUTPUT_BYTES: usize = 32_000;
const MAX_TITLE_BYTES: usize = 256;
const MAX_DETAIL_BYTES: usize = 4096;
const MAX_COMMAND_BYTES: usize = 4096;

pub(super) enum Origin {
    Ordinary,
    Agent(String, Option<store::JavaAgentReferences>),
    Unsupported,
}

pub(super) fn read_origin(chat: &Fields<'_, '_>, redactor: &Redactor) -> io::Result<Origin> {
    let origin = value(chat, "origin")?;
    let id = value(chat, "sourceRunId")?;
    if origin.trim().is_empty() {
        if !id.trim().is_empty() {
            return Ok(Origin::Unsupported);
        }
        for field in LINK_FIELDS {
            if !value(chat, field)?.trim().is_empty() {
                return Ok(Origin::Unsupported);
            }
        }
        return Ok(Origin::Ordinary);
    }
    if origin.trim() != ORIGIN {
        return Ok(Origin::Unsupported);
    }
    let id = safe_hint(redactor, id.trim(), store::valid_agent_source_run_id)?
        .ok_or_else(|| invalid("Saved Agent chat has an invalid source run identifier."))?;
    let reference = |field| -> io::Result<Option<String>> {
        let text = value(chat, field)?;
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }
        if !store::valid_agent_reference(text) {
            return Err(invalid("Saved Agent chat has an invalid source reference."));
        }
        safe_hint(redactor, text, store::valid_agent_reference)
    };
    let references = store::JavaAgentReferences {
        terminal_session_id: reference("sourceTerminalSessionId")?,
        connection_id: reference("sourceConnectionId")?,
        journal_id: reference("sourceJournalId")?,
        journal_entry_id: reference("sourceJournalEntryId")?,
    };
    let has_references = references
        .fields()
        .into_iter()
        .any(|(_, value)| value.is_some());
    Ok(Origin::Agent(id, has_references.then_some(references)))
}

pub(super) fn write_origin(
    xml: &mut impl Write,
    record: &store::Record,
    redactor: &Redactor,
) -> io::Result<()> {
    let Some(id) = record.portable_agent_run_id() else {
        return Ok(());
    };
    if !identifier_is_safe(redactor, id)? {
        return Err(invalid(
            "Saved Agent chat has an unsafe source run identifier.",
        ));
    }
    element(xml, "        ", "origin", ORIGIN)?;
    element(xml, "        ", "sourceRunId", id)?;
    let mut write_reference = |name, value: Option<&str>| -> io::Result<()> {
        if let Some(value) = value
            && let Some(value) = safe_hint(redactor, value, store::valid_agent_reference)?
        {
            element(xml, "        ", name, &value)?;
        }
        Ok(())
    };
    if let Some(references) = record
        .java
        .as_ref()
        .and_then(|origin| origin.agent_refs.as_ref())
    {
        for (name, value) in references.fields() {
            write_reference(name, value)?;
        }
    }
    if let Some(reference) = record.agent_journal() {
        write_reference("sourceJournalId", Some(&reference.journal_id))?;
        write_reference("sourceJournalEntryId", Some(&reference.entry_id))?;
    }
    Ok(())
}

pub(super) fn read_steps(node: Node<'_, '_>, redactor: &Redactor) -> io::Result<Vec<Step>> {
    children(node, "step", MAX_STEPS)?
        .into_iter()
        .map(|step| read_step(step, redactor))
        .collect()
}

fn read_step(node: Node<'_, '_>, redactor: &Redactor) -> io::Result<Step> {
    let step = fields(
        node,
        &[
            "type",
            "status",
            "title",
            "summary",
            "command",
            "exitCode",
            "output",
            "elapsedSeconds",
            "truncated",
        ],
    )?;
    let (kind, fallback) = match value(&step, "type")?.trim() {
        "ACTION" => (StepKind::Command, "Agent action"),
        "MESSAGE" => (StepKind::Message, "Agent message"),
        "QUESTION" => (StepKind::Question, "Agent question"),
        "ERROR" => (StepKind::Warning, "Agent warning"),
        _ => {
            return Err(invalid(
                "Saved chat contains an unsupported Agent step type.",
            ));
        }
    };
    let status = match value(&step, "status")?.trim() {
        "" => None,
        "RUNNING" => Some(StepStatus::Running),
        "COMPLETED" => Some(StepStatus::Completed),
        "CANCELLED" => Some(StepStatus::Cancelled),
        "FAILED" => Some(StepStatus::Failed),
        _ => return Err(invalid("Saved chat contains an invalid Agent step status.")),
    };
    let text = |name, limit| masked_text(redactor, &value(&step, name)?, limit);
    let title = text("title", MAX_TITLE_BYTES)?;
    let title = if title.trim().is_empty() {
        fallback.into()
    } else {
        title
    };
    let detail = text("summary", MAX_DETAIL_BYTES)?;
    let command = text("command", MAX_COMMAND_BYTES)?;
    let command = (!command.is_empty()).then_some(command);
    let exit_code = optional_number::<i32>(&step, "exitCode")?.map(i64::from);
    let elapsed_seconds = optional_number::<i64>(&step, "elapsedSeconds")?
        .map(|value| {
            u64::try_from(value)
                .map_err(|_| invalid("Saved chat has a negative Agent step duration."))
        })
        .transpose()?;
    let truncated = match value(&step, "truncated")?.trim() {
        "" => None,
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => {
            return Err(invalid(
                "Saved chat has an invalid Agent step truncation flag.",
            ));
        }
    };
    // Mask the whole output before separating lines or clipping retained evidence.
    let output = text("output", MAX_OUTPUT_BYTES)?;
    if kind != StepKind::Command
        && (command.is_some() || exit_code.is_some() || !output.is_empty() || truncated.is_some())
    {
        return Err(invalid(
            "Saved chat has command evidence on a non-action step.",
        ));
    }
    let (output, clipped) = split_output(&output);
    Ok(Step {
        kind,
        title,
        detail,
        command,
        output,
        exit_code,
        status,
        elapsed_seconds,
        truncated: if clipped { Some(true) } else { truncated },
    })
}

fn optional_number<T: FromStr>(fields: &Fields<'_, '_>, name: &str) -> io::Result<Option<T>> {
    let text = value(fields, name)?;
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    text.parse()
        .map(Some)
        .map_err(|_| invalid("Saved chat contains an invalid Agent step number."))
}

fn split_output(text: &str) -> (Vec<String>, bool) {
    if text.is_empty() {
        return (Vec::new(), false);
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    let mut clipped = lines.len() > MAX_OUTPUT_LINES;
    if clipped {
        let half = MAX_OUTPUT_LINES / 2;
        lines.drain(half..lines.len() - half);
    }
    let lines = lines
        .into_iter()
        .map(|line| {
            if line.len() <= MAX_OUTPUT_LINE_BYTES {
                return line.into();
            }
            clipped = true;
            let end = line.floor_char_boundary(MAX_OUTPUT_LINE_BYTES - '…'.len_utf8());
            format!("{}…", &line[..end])
        })
        .collect();
    (lines, clipped)
}

pub(super) fn write_steps(
    xml: &mut impl Write,
    steps: &[Step],
    redactor: &Redactor,
) -> io::Result<()> {
    if steps.is_empty() {
        return Ok(());
    }
    xml.write_all(b"                <agentSteps>\n")?;
    for step in steps {
        write_step(xml, step, redactor)?;
    }
    xml.write_all(b"                </agentSteps>\n")
}

fn write_step(xml: &mut impl Write, step: &Step, redactor: &Redactor) -> io::Result<()> {
    let kind = match step.kind {
        StepKind::Decision | StepKind::Message => "MESSAGE",
        StepKind::Command => "ACTION",
        StepKind::Question => "QUESTION",
        StepKind::Warning => "ERROR",
    };
    let status = step.status.map(|status| match status {
        StepStatus::Running => "RUNNING",
        StepStatus::Completed => "COMPLETED",
        StepStatus::Cancelled => "CANCELLED",
        StepStatus::Failed => "FAILED",
    });
    let title = masked_text(redactor, &step.title, MAX_TITLE_BYTES)?;
    let detail = masked_text(redactor, &step.detail, MAX_DETAIL_BYTES)?;
    let command = step
        .command
        .as_deref()
        .map(|command| masked_text(redactor, command, MAX_COMMAND_BYTES))
        .transpose()?;
    let output = masked_text(redactor, &step.output.join("\n"), MAX_OUTPUT_BYTES)?;
    let exit_code = step
        .exit_code
        .map(|code| {
            i32::try_from(code)
                .or_else(|_| u32::try_from(code).map(|code| code as i32))
                .map_err(|_| invalid("Saved chat contains an invalid Agent step exit code."))
        })
        .transpose()?;
    let elapsed = i64::try_from(step.elapsed_seconds.unwrap_or_default())
        .map_err(|_| invalid("Saved chat contains an invalid Agent step duration."))?;
    xml.write_all(b"                    <step>\n")?;
    let indent = "                        ";
    element(xml, indent, "type", kind)?;
    if let Some(status) = status {
        element(xml, indent, "status", status)?;
    }
    element(xml, indent, "title", &title)?;
    element(xml, indent, "summary", &detail)?;
    if let Some(command) = command {
        element(xml, indent, "command", &command)?;
    }
    if let Some(exit_code) = exit_code {
        element(xml, indent, "exitCode", &exit_code.to_string())?;
    }
    if !output.is_empty() {
        element(xml, indent, "output", &output)?;
    }
    element(xml, indent, "elapsedSeconds", &elapsed.to_string())?;
    if let Some(truncated) = step.truncated {
        element(
            xml,
            indent,
            "truncated",
            if truncated { "true" } else { "false" },
        )?;
    }
    xml.write_all(b"                    </step>\n")
}
