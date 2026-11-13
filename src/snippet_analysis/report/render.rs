use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, Write};
use std::sync::atomic::AtomicBool;

use base64::{engine::general_purpose::STANDARD, write::EncoderWriter};
use serde::ser::{SerializeSeq as _, Serializer as _};
use serde_json::{Value, json};

use crate::session_journal::{
    export_branding::{PdfBranding, REPOSITORY_URL},
    is_display_character,
};
use crate::snippet_analysis::diagram::AnalysisDiagram;
use crate::snippet_analysis::model::{ApplyRun, ApplyStatus};
use crate::snippet_window::compare::{ExportDiff, ExportHunk};

use super::markdown::{Link as DiagramLink, href as diagram_href};
use super::post::{Delta, ReportVerification};
use super::{Format, Item, Options, Packing, Subject, check_cancelled, timestamp};

mod pdf;

const SCHEMA: &str = "kortty.snippetAnalysisReport/1";
const SECTIONS: [(&str, &str); 3] = [
    ("security", "Security"),
    ("optimization", "Optimization"),
    ("design", "Design"),
];

pub(super) fn write(
    output: &mut impl Write,
    items: &[Item],
    options: &Options,
    packing: Packing,
    markdown_diagrams: &[Option<DiagramLink>],
    cancelled: &AtomicBool,
    rendered: &mut impl FnMut(&Item),
) -> io::Result<usize> {
    match options.format {
        Format::Json => return write_json(output, items, options, packing, cancelled, rendered),
        Format::Pdf => return pdf::write(output, items, options, packing, cancelled, rendered),
        Format::Html | Format::Markdown => {}
    }
    let combined = packing == Packing::Combined;
    let title = if combined {
        "Code analysis reports"
    } else {
        title(&items[0])
    };
    let mut document = Document {
        output,
        html: options.format == Format::Html,
        markdown_diagram: None,
        report_index: 1,
    };
    document.begin(title, &options.locale, &options.branding)?;
    if combined {
        document.contents(items, &options.skipped)?;
    }
    let mut failed_diagrams = 0;
    for (index, item) in items.iter().enumerate() {
        check_cancelled(cancelled)?;
        document.markdown_diagram = markdown_diagrams.get(index).and_then(Option::as_ref);
        failed_diagrams +=
            usize::from(document.report(item, options, combined.then_some(index + 1))?);
        rendered(item);
    }
    document.finish(&options.branding)?;
    Ok(failed_diagrams)
}

fn write_json(
    output: &mut impl Write,
    items: &[Item],
    options: &Options,
    packing: Packing,
    cancelled: &AtomicBool,
    rendered: &mut impl FnMut(&Item),
) -> io::Result<usize> {
    let mut failed_diagrams = 0;
    if packing == Packing::Combined {
        let mut serializer = serde_json::Serializer::pretty(&mut *output);
        let mut sequence = serializer.serialize_seq(Some(items.len()))?;
        for item in items {
            check_cancelled(cancelled)?;
            let (report, failed) = report_json(item, options);
            failed_diagrams += usize::from(failed);
            sequence.serialize_element(&report)?;
            rendered(item);
        }
        sequence.end()?;
    } else {
        let (report, failed) = report_json(&items[0], options);
        failed_diagrams += usize::from(failed);
        serde_json::to_writer_pretty(&mut *output, &report)?;
        rendered(&items[0]);
    }
    output.write_all(b"\n")?;
    Ok(failed_diagrams)
}

fn report_json(item: &Item, options: &Options) -> (Value, bool) {
    let record = &item.record;
    let source = &record.source;
    let provenance = &record.provenance;
    let run = item.post_run();
    let diff = run.map(|run| ExportDiff::for_apply(record, run));
    let after = run.zip(diff.as_ref());
    let snippet_id = match item.subject {
        Subject::Snippet(id) => String::from(id),
        Subject::Folder(_) => String::new(),
    };
    let findings: Vec<_> = record
        .analysis
        .improvements
        .iter()
        .map(|finding| {
            let mut item = json!({
                "id": finding.id,
                "section": section(&finding.category),
                "category": finding.category,
                "severity": severity(&finding.severity),
                "severityLabel": finding.severity,
                "title": finding.title,
                "detail": finding.detail,
                "recommendation": finding.recommendation,
                "line": finding.line.filter(|line| *line > 0),
                "selected": item.selected(&finding.id),
                "changes": after.map_or_else(Vec::new, |(run, diff)| changes(run, diff, Some(&finding.id))),
            });
            if let Some((run, diff)) = after {
                item["status"] = json!(finding_status(run, &finding.id));
                let hunks: Vec<_> = diff.hunks_for(&finding.id).collect();
                if !hunks.is_empty() {
                    item["hunks"] = json!(hunks);
                }
            }
            item
        })
        .collect();
    let dependencies: Vec<_> = record
        .analysis
        .dependencies
        .iter()
        .map(|dependency| {
            let mut item = json!({
                "id": dependency.id,
                "name": dependency.name,
                "kind": dependency.kind,
                "purpose": dependency.purpose,
                "suggestion": dependency.suggestion,
                "selected": item.selected(&dependency.id),
                "changes": after.map_or_else(Vec::new, |(run, diff)| changes(run, diff, Some(&dependency.id))),
            });
            if let Some(run) = run {
                item["status"] = json!(finding_status(run, &dependency.id));
            }
            item
        })
        .collect();
    let diagram = record.diagram.as_ref().and_then(|saved| {
        let diagram = saved.report_diagram().ok()?;
        Some(json!({
            "type": "logical-structure",
            "fallback": saved.fallback,
            "mermaid": diagram.to_mermaid(),
        }))
    });
    let failed_diagram = record.diagram.is_some() && diagram.is_none();
    (
        json!({
            "schema": SCHEMA,
            "kind": if run.is_some() { "POST_APPLY" } else { "PRE_APPLY" },
            "exportedAt": iso_date(options.exported_at),
            "locale": options.locale,
            "header": {
                "scriptName": source.snippet_name,
                "snippetId": snippet_id,
                "scriptLanguage": source.language,
                "aiProfile": provenance.profile_name,
                "aiModel": provenance.model,
                "skills": provenance.skill_names,
                "analysisLanguage": source.report_language_code,
                "codeTextLanguage": run.and_then(ApplyRun::code_text_language_code).unwrap_or(&source.code_text_language_code),
                "analysedAt": iso_date(record.analyzed_at),
                "appliedAt": run.and_then(|run| iso_date(run.started_at)),
                "sourceSha256": source.sha256,
                "stale": item.stale(),
                "runNumber": run.map_or(0, |run| item.run_number(run.id)),
                "runCount": record.apply_runs.len(),
            },
            "summary": record.analysis.summary,
            "findings": findings,
            "dependencies": dependencies,
            "diagram": diagram,
            "apply": after.map(|(run, diff)| apply_json(run, diff)),
            "verification": item.post.as_ref().and_then(|post| post.verification.as_ref()).map(verification_json),
        }),
        failed_diagram,
    )
}

fn changes(run: &ApplyRun, diff: &ExportDiff, selected: Option<&str>) -> Vec<Value> {
    run.reasons
        .iter()
        .filter(|reason| selected.is_none_or(|id| reason.id == id))
        .map(|reason| {
            let anchor = if diff.hunks_for(&reason.id).next().is_some() {
                reason.anchor.as_deref().unwrap_or_default()
            } else {
                ""
            };
            json!({ "finding": reason.id, "anchor": anchor, "reason": reason.reason })
        })
        .collect()
}

fn finding_status(run: &ApplyRun, id: &str) -> &'static str {
    if !run.selected.contains(id) {
        return "not-selected";
    }
    match run.status {
        ApplyStatus::Proposed => "proposed",
        ApplyStatus::Rejected => "rejected",
        ApplyStatus::Accepted if run.reasons.iter().any(|reason| reason.id == id) => "applied",
        ApplyStatus::Accepted => "unconfirmed",
    }
}

fn apply_json(run: &ApplyRun, diff: &ExportDiff) -> Value {
    let usage = run.provenance.usage;
    let tokens = (usage.total_tokens != 0).then(|| {
        json!({
            "prompt": usage.prompt_tokens,
            "completion": usage.completion_tokens,
            "total": usage.total_tokens,
            "cachedPrompt": usage.cached_prompt_tokens,
        })
    });
    let mut value = json!({
        "outcome": run.status,
        "partial": run.partial.is_some(),
        "appliedAt": iso_date(run.started_at),
        "decidedAt": run.decided_at.and_then(iso_date),
        "profile": run.provenance.profile_name,
        "elapsedSeconds": run.provenance.duration_millis / 1000,
        "tokens": tokens,
        "completedItems": run.reasons.len(),
        "totalItems": run.partial.map_or_else(|| run.selected.len(), |partial| partial.total_items),
        "changes": changes(run, diff, None),
        "diff": diff,
    });
    if let Some(partial) = run.partial {
        value["completedStages"] = json!(partial.completed_stages);
        value["totalStages"] = json!(partial.total_stages);
    }
    value
}

fn verification_json(verification: &ReportVerification) -> Value {
    fn delta(item: &Delta) -> Value {
        let mut value = json!({
            "id": item.id,
            "title": item.title,
            "severity": severity(&item.severity),
            "section": section(&item.category),
        });
        if let Some(previous) = &item.previous_id {
            value["previousId"] = json!(previous);
        }
        value
    }
    let deltas = |items: &[Delta]| items.iter().map(delta).collect::<Vec<_>>();
    json!({
        "verifiedAt": iso_date(verification.verified_at),
        "method": "title-similarity",
        "resolved": deltas(&verification.resolved),
        "new": deltas(&verification.new),
        "persisting": deltas(&verification.persisting),
    })
}

struct Document<'a, W> {
    output: &'a mut W,
    html: bool,
    markdown_diagram: Option<&'a DiagramLink>,
    report_index: usize,
}

impl<W: Write> Document<'_, W> {
    fn begin(&mut self, title: &str, locale: &str, branding: &PdfBranding) -> io::Result<()> {
        if self.html {
            writeln!(
                self.output,
                "<!doctype html>\n<html lang=\"{}\"><head><meta charset=\"utf-8\">\
                 <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
                 <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\
                 <title>{}</title><style>{CSS}</style></head><body>",
                html(locale),
                html(title),
            )?;
            if let Some(watermark) = &branding.watermark {
                let [red, green, blue] = watermark.color;
                writeln!(
                    self.output,
                    "<div class=\"watermark\" aria-hidden=\"true\" style=\"color:#{red:02x}{green:02x}{blue:02x}\">{}</div>",
                    html(&watermark.brand.text),
                )?;
            }
            writeln!(self.output, "<main>")?;
        }
        Ok(())
    }

    fn finish(&mut self, branding: &PdfBranding) -> io::Result<()> {
        if self.html {
            writeln!(self.output, "</main>")?;
        }
        if let Some(footer) = &branding.footer {
            let label = footer.footer_label();
            if self.html {
                write!(self.output, "<footer>{}", html(label))?;
                if footer.repository_link {
                    write!(
                        self.output,
                        " · <a href=\"{REPOSITORY_URL}\" rel=\"noreferrer\">{REPOSITORY_URL}</a>"
                    )?;
                }
                writeln!(self.output, "</footer>")?;
            } else {
                write!(self.output, "\n---\n\n_{}_", markdown(&single_line(label)))?;
                if footer.repository_link {
                    write!(self.output, " · <{REPOSITORY_URL}>")?;
                }
                writeln!(self.output)?;
            }
        }
        if self.html {
            writeln!(self.output, "</body></html>")?;
        }
        Ok(())
    }

    fn contents(&mut self, items: &[Item], skipped: &[String]) -> io::Result<()> {
        self.heading(1, "Code analysis reports")?;
        if self.html {
            writeln!(self.output, "<nav aria-label=\"Contents\"><ol>")?;
            for (index, item) in items.iter().enumerate() {
                writeln!(
                    self.output,
                    "<li><a href=\"#r{}\">{}</a></li>",
                    index + 1,
                    html(&label(item)),
                )?;
            }
            writeln!(self.output, "</ol></nav>")?;
        } else {
            for (index, item) in items.iter().enumerate() {
                writeln!(
                    self.output,
                    "{}. [{}](#r{})",
                    index + 1,
                    markdown(&label(item)),
                    index + 1,
                )?;
            }
            writeln!(self.output)?;
        }
        if !skipped.is_empty() {
            self.heading(2, "Snippets without a stored analysis")?;
            for name in skipped {
                self.text(&single_line(name))?;
            }
        }
        Ok(())
    }

    fn report(&mut self, item: &Item, options: &Options, index: Option<usize>) -> io::Result<bool> {
        self.report_index = index.unwrap_or(1);
        let depth = 1 + usize::from(index.is_some());
        if self.html {
            if let Some(index) = index {
                writeln!(self.output, "<article id=\"r{index}\">")?;
            } else {
                writeln!(self.output, "<article>")?;
            }
        } else if let Some(index) = index {
            writeln!(self.output, "<a id=\"r{index}\"></a>\n")?;
        }
        let failed_diagram = self.report_body(item, options, depth)?;
        if self.html {
            writeln!(self.output, "</article>")?;
        }
        Ok(failed_diagram)
    }

    fn finding_fragment(&self, id: &str) -> String {
        let mut fragment = format!("r{}-f-", self.report_index);
        for byte in id.bytes() {
            let _ = write!(fragment, "{byte:02x}");
        }
        fragment
    }

    fn markdown_diagram(&mut self, saved: &AnalysisDiagram) -> io::Result<bool> {
        let diagram = match saved.report_diagram() {
            Ok(diagram) => diagram,
            Err(error) => {
                self.text(&format!(
                    "The saved flowchart could not be rendered ({error})."
                ))?;
                return Ok(true);
            }
        };
        let failed = match self.markdown_diagram {
            Some(DiagramLink::Image(name)) => {
                writeln!(
                    self.output,
                    "![Analysis flowchart](<{}>)\n",
                    diagram_href(name)
                )?;
                false
            }
            Some(DiagramLink::Failed(error)) => {
                self.text(&format!(
                    "The saved flowchart could not be rendered ({error})."
                ))?;
                true
            }
            None => false,
        };
        self.code(&diagram.to_mermaid(), "mermaid")?;
        Ok(failed)
    }
}

trait ReportBody {
    fn heading(&mut self, depth: usize, value: &str) -> io::Result<()>;
    fn text(&mut self, value: &str) -> io::Result<()>;
    fn field(&mut self, name: &str, value: &str) -> io::Result<()>;
    fn code(&mut self, content: &str, language: &str) -> io::Result<()>;
    fn diagram(&mut self, depth: usize, saved: &AnalysisDiagram) -> io::Result<bool>;
    fn metadata_fields(&mut self, entries: &[(&str, &str)]) -> io::Result<()>;

    fn diff(&mut self, diff: &ExportDiff, run: &ApplyRun) -> io::Result<()>;

    fn finding_anchor(&mut self, _id: &str) -> io::Result<()> {
        Ok(())
    }

    fn hunk_notes(&mut self, index: usize, hunk: &ExportHunk, run: &ApplyRun) -> io::Result<()> {
        if !hunk.findings.is_empty() {
            self.field(
                &format!("Changed region {}", index + 1),
                &hunk.findings.join(", "),
            )?;
            for reason in hunk.reasons(run) {
                self.field(&reason.id, &reason.reason)?;
            }
        }
        Ok(())
    }

    fn diagram_note(&mut self, saved: &AnalysisDiagram) -> io::Result<()> {
        if saved.fallback {
            self.text(AnalysisDiagram::FALLBACK_NOTICE)?;
            if let Some(reason) = &saved.fallback_reason {
                self.field("Fallback reason", reason)?;
            }
        }
        Ok(())
    }

    fn report_body(&mut self, item: &Item, options: &Options, depth: usize) -> io::Result<bool> {
        self.heading(depth, &label(item))?;
        let run = item.post_run();
        self.text(if run.is_some() {
            "After applying changes"
        } else {
            "Before applying changes"
        })?;
        self.metadata(item, options)?;
        if item.stale() {
            self.text("The source has changed since this analysis.")?;
        }

        let analysis = &item.record.analysis;
        if !analysis.summary.is_empty() {
            self.heading(depth + 1, "Summary")?;
            self.text(&analysis.summary)?;
        }
        self.findings(item, depth + 1)?;
        if !analysis.dependencies.is_empty() {
            self.heading(
                depth + 1,
                &format!("Dependencies ({})", analysis.dependencies.len()),
            )?;
            for dependency in &analysis.dependencies {
                self.finding_anchor(&dependency.id)?;
                self.heading(depth + 2, &dependency.name)?;
                self.finding_selection(item, &dependency.id)?;
                self.field("Kind", &dependency.kind)?;
                self.field("Purpose", &dependency.purpose)?;
                self.field("Suggestion", &dependency.suggestion)?;
            }
        }
        let failed_diagram = if let Some(saved) = &item.record.diagram {
            self.diagram(depth + 1, saved)?
        } else {
            self.heading(depth + 1, "Diagram")?;
            self.text("No diagram available.")?;
            false
        };
        if let Some(run) = run {
            self.apply_summary(item, run, depth + 1)?;
            if let Some(verification) = item
                .post
                .as_ref()
                .and_then(|post| post.verification.as_ref())
            {
                self.verification(verification, depth + 1)?;
            }
        }
        if options.include_code {
            let source = &item.record.source;
            if let Some(run) = run {
                self.heading(depth + 1, "Resulting code")?;
                self.code(&run.after, &source.language)?;
            } else if let Some(content) = &source.content {
                self.heading(depth + 1, "Analysed code")?;
                self.code(content, &source.language)?;
            } else {
                self.heading(depth + 1, "Analysed code")?;
                self.text("The analysed code was not stored.")?;
            }
        }
        Ok(failed_diagram)
    }

    fn finding_selection(&mut self, item: &Item, id: &str) -> io::Result<()> {
        self.field("ID", id)?;
        let Some(run) = item.post_run() else {
            return self.field(
                "Selected for apply",
                if item.selected(id) { "Yes" } else { "No" },
            );
        };
        let status = match finding_status(run, id) {
            "applied" => "Applied",
            "rejected" => "Rejected",
            "proposed" => "Proposed",
            "unconfirmed" => "Unconfirmed",
            _ => "Not selected",
        };
        self.field("Apply status", status)?;
        if let Some(reason) = run.reasons.iter().find(|reason| reason.id == id) {
            self.field("Change reason", &reason.reason)?;
        }
        Ok(())
    }

    fn apply_summary(&mut self, item: &Item, run: &ApplyRun, depth: usize) -> io::Result<()> {
        self.heading(depth, "Apply run")?;
        self.field("Outcome", run.status.label())?;
        if let Some(partial) = run.partial {
            self.field("Partial apply", &partial.progress())?;
        }
        self.field("AI profile", &run.provenance.profile_name)?;
        self.field("Started at", &iso_date(run.started_at).unwrap_or_default())?;
        if let Some(decided) = run.decided_at.and_then(iso_date) {
            self.field("Decided at", &decided)?;
        }
        let count = run
            .partial
            .map_or_else(|| run.selected.len(), |partial| partial.total_items);
        self.field(
            "Confirmed work items",
            &format!("{} of {count}", run.reasons.len()),
        )?;
        let usage = run.provenance.usage;
        self.field(
            "Token usage",
            &if usage.total_tokens == 0 {
                "Not reported".into()
            } else {
                format!(
                    "{} prompt, {} completion, {} total",
                    usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
                )
            },
        )?;
        if run.provenance.duration_millis > 0 {
            self.field(
                "Duration",
                &format!(
                    "{:.1} seconds",
                    run.provenance.duration_millis as f64 / 1000.0
                ),
            )?;
        }
        self.heading(depth + 1, "Changes to the script")?;
        let diff = ExportDiff::for_apply(&item.record, run);
        if diff.too_large {
            self.text("The changed section exceeds the comparison work limit.")
        } else if diff.hunks.is_empty() {
            self.text("The exact texts differ only in line endings.")
        } else {
            self.diff(&diff, run)
        }
    }

    fn verification(&mut self, verification: &ReportVerification, depth: usize) -> io::Result<()> {
        self.heading(depth, "Verification")?;
        self.field(
            "Verified at",
            &iso_date(verification.verified_at).unwrap_or_default(),
        )?;
        self.text("Findings are matched heuristically by title and description.")?;
        for (label, items) in [
            ("Resolved", &verification.resolved),
            ("Still open", &verification.persisting),
            ("New", &verification.new),
        ] {
            self.heading(depth + 1, &format!("{label} ({})", items.len()))?;
            for item in items {
                let previous = item
                    .previous_id
                    .as_ref()
                    .map_or_else(String::new, |id| format!(" (was {id})"));
                self.field(&format!("{}{previous}", item.id), &item.title)?;
            }
        }
        Ok(())
    }

    fn metadata(&mut self, item: &Item, options: &Options) -> io::Result<()> {
        let record = &item.record;
        let source = &record.source;
        let provenance = &record.provenance;
        let skills = provenance.skill_names.join(", ");
        let analyzed_at = iso_date(record.analyzed_at).unwrap_or_default();
        let exported_at = iso_date(options.exported_at).unwrap_or_default();
        let finding_count = record.analysis.improvements.len().to_string();
        let dependency_count = record.analysis.dependencies.len().to_string();
        let code_text_language = item
            .post_run()
            .and_then(ApplyRun::code_text_language_code)
            .unwrap_or(&source.code_text_language_code);
        let entries = [
            ("Script", source.snippet_name.as_str()),
            ("Language", source.language.as_str()),
            ("AI profile", provenance.profile_name.as_str()),
            ("AI model", provenance.model.as_str()),
            ("AI skills", skills.as_str()),
            ("Analysis language", source.report_language_code.as_str()),
            ("Code text language", code_text_language),
            ("Analysed at", analyzed_at.as_str()),
            ("Exported at", exported_at.as_str()),
            ("Source SHA-256", source.sha256.as_str()),
            ("Findings", finding_count.as_str()),
            ("Dependencies", dependency_count.as_str()),
        ];
        self.metadata_fields(&entries)
    }

    fn findings(&mut self, item: &Item, depth: usize) -> io::Result<()> {
        let source = &item.record.source;
        let excerpts = excerpt_lines(item);
        let findings = &item.record.analysis.improvements;
        for (key, label) in SECTIONS {
            let group = findings
                .iter()
                .filter(|finding| section(&finding.category) == key);
            let count = group.clone().count();
            if count == 0 {
                continue;
            }
            self.heading(depth, &format!("{label} ({count})"))?;
            for finding in group {
                self.finding_anchor(&finding.id)?;
                self.heading(depth + 1, &finding.title)?;
                self.finding_selection(item, &finding.id)?;
                self.field("Severity", severity(&finding.severity))?;
                if !finding.category.trim().eq_ignore_ascii_case(key) {
                    self.field("Category", &finding.category)?;
                }
                if let Some(file) = &finding.file {
                    self.field("File", file)?;
                }
                if let Some(line) = finding.line.filter(|line| *line > 0) {
                    self.field("Line", &line.to_string())?;
                    if finding.file.is_none() && excerpts.get(&line).is_some_and(Option::is_some) {
                        let excerpt = excerpts
                            .range(line.saturating_sub(3)..=line.saturating_add(3))
                            .filter_map(|(_, text)| *text)
                            .collect::<Vec<_>>()
                            .join("\n");
                        self.code(&excerpt, &source.language)?;
                    }
                }
                self.text(&finding.detail)?;
                self.field("Recommendation", &finding.recommendation)?;
            }
        }
        Ok(())
    }
}

impl<W: Write> ReportBody for Document<'_, W> {
    fn finding_anchor(&mut self, id: &str) -> io::Result<()> {
        if self.html {
            let fragment = self.finding_fragment(id);
            writeln!(self.output, "<div id=\"{fragment}\"></div>")?;
        }
        Ok(())
    }

    fn diff(&mut self, diff: &ExportDiff, run: &ApplyRun) -> io::Result<()> {
        if !self.html {
            for (index, hunk) in diff.hunks.iter().enumerate() {
                self.hunk_notes(index, hunk, run)?;
                let lines: Vec<_> = std::iter::once(hunk.header.as_str())
                    .chain(hunk.lines.iter().map(String::as_str))
                    .collect();
                self.code(&lines.join("\n"), "diff")?;
            }
            return Ok(());
        }
        for hunk in &diff.hunks {
            writeln!(
                self.output,
                "<section class=\"diff-region\"><div class=\"hunk\"><code>{}</code>",
                html(&hunk.header)
            )?;
            for finding in &hunk.findings {
                let fragment = self.finding_fragment(finding);
                writeln!(self.output, "<a href=\"#{fragment}\">{}</a>", html(finding))?;
            }
            writeln!(self.output, "</div>")?;
            for reason in hunk.reasons(run) {
                self.field(&reason.id, &reason.reason)?;
            }
            writeln!(self.output, "<pre class=\"diff\"><code>")?;
            for line in &hunk.lines {
                let class = if line.starts_with('+') {
                    "add"
                } else if line.starts_with('-') {
                    "remove"
                } else {
                    "context"
                };
                writeln!(self.output, "<span class=\"{class}\">{}</span>", html(line))?;
            }
            writeln!(self.output, "</code></pre></section>")?;
        }
        Ok(())
    }

    fn diagram(&mut self, depth: usize, saved: &AnalysisDiagram) -> io::Result<bool> {
        self.heading(depth, "Diagram")?;
        self.diagram_note(saved)?;
        if !self.html {
            return self.markdown_diagram(saved);
        }
        let result = saved.report_png().map(|png| {
            write!(
                self.output,
                "<figure><img alt=\"Analysis flowchart\" src=\"data:image/png;base64,"
            )?;
            let mut encoder = EncoderWriter::new(&mut *self.output, &STANDARD);
            encoder.write_all(&png)?;
            writeln!(encoder.finish()?, "\"></figure>")
        });
        match result {
            Ok(write) => {
                write?;
                Ok(false)
            }
            Err(error) => {
                self.text(&format!(
                    "The saved flowchart could not be rendered ({error})."
                ))?;
                Ok(true)
            }
        }
    }

    fn metadata_fields(&mut self, entries: &[(&str, &str)]) -> io::Result<()> {
        if self.html {
            writeln!(self.output, "<dl>")?;
            for (label, value) in entries {
                writeln!(
                    self.output,
                    "<dt>{}</dt><dd>{}</dd>",
                    html(label),
                    html(&single_line(value)),
                )?;
            }
            writeln!(self.output, "</dl>")?;
        } else {
            writeln!(self.output, "| | |\n|---|---|")?;
            for (label, value) in entries {
                writeln!(
                    self.output,
                    "| {} | {} |",
                    markdown(label),
                    markdown(&single_line(value)),
                )?;
            }
            writeln!(self.output)?;
        }
        Ok(())
    }

    fn heading(&mut self, depth: usize, value: &str) -> io::Result<()> {
        let value = single_line(value);
        if self.html {
            writeln!(self.output, "<h{depth}>{}</h{depth}>", html(&value))
        } else {
            writeln!(self.output, "{} {}\n", "#".repeat(depth), markdown(&value),)
        }
    }

    fn text(&mut self, value: &str) -> io::Result<()> {
        if value.trim().is_empty() {
            return Ok(());
        }
        if self.html {
            writeln!(self.output, "<p>{}</p>", html(value))
        } else {
            for line in value.lines() {
                writeln!(self.output, "{}", markdown(line.trim()))?;
            }
            writeln!(self.output)
        }
    }

    fn field(&mut self, name: &str, value: &str) -> io::Result<()> {
        if value.trim().is_empty() {
            return Ok(());
        }
        if self.html {
            writeln!(
                self.output,
                "<p><strong>{}</strong> {}</p>",
                html(name),
                html(value)
            )
        } else {
            writeln!(
                self.output,
                "**{}** {}\n",
                markdown(name),
                markdown(&single_line(value)),
            )
        }
    }

    fn code(&mut self, content: &str, language: &str) -> io::Result<()> {
        if self.html {
            return writeln!(self.output, "<pre><code>{}</code></pre>", html(content));
        }
        let longest = content
            .split(|character| character != '`')
            .map(str::len)
            .max()
            .unwrap_or_default();
        let fence = "`".repeat(longest.saturating_add(1).max(3));
        let language: String = language
            .bytes()
            .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'+'))
            .take(40)
            .map(char::from)
            .collect();
        writeln!(self.output, "{fence}{language}")?;
        for line in content.split_terminator('\n') {
            writeln!(
                self.output,
                "{}",
                line.chars()
                    .filter(|&character| is_display_character(character))
                    .collect::<String>(),
            )?;
        }
        writeln!(self.output, "{fence}\n")
    }
}

fn title(item: &Item) -> &str {
    let name = item.record.source.snippet_name.trim();
    if name.is_empty() {
        "Code analysis"
    } else {
        name
    }
}

fn label(item: &Item) -> String {
    if item.folder_path.is_empty() {
        title(item).to_owned()
    } else {
        format!("{} / {}", item.folder_path, title(item))
    }
}

fn excerpt_lines(item: &Item) -> BTreeMap<usize, Option<&str>> {
    let mut excerpts = BTreeMap::new();
    let source = &item.record.source;
    if !matches!(item.subject, Subject::Snippet(_)) {
        return excerpts;
    }
    let Some(content) = &source.content else {
        return excerpts;
    };
    for finding in &item.record.analysis.improvements {
        if let Some(line) = finding.line
            && finding.file.is_none()
            && (1..=source.line_count).contains(&line)
        {
            for number in
                line.saturating_sub(3).max(1)..=line.saturating_add(3).min(source.line_count)
            {
                excerpts.insert(number, None);
            }
        }
    }
    let mut lines = content.lines();
    let mut previous = 0;
    for (&number, text) in &mut excerpts {
        *text = lines.nth(number - previous - 1);
        previous = number;
    }
    excerpts
}

fn section(category: &str) -> &'static str {
    match category.trim().to_ascii_lowercase().as_str() {
        "security" => "security",
        "optimization" => "optimization",
        _ => "design",
    }
}

fn severity(label: &str) -> &'static str {
    match label.trim().to_ascii_lowercase().as_str() {
        "critical" | "crit" => "critical",
        "high" => "high",
        "medium" | "moderate" | "med" => "medium",
        "low" => "low",
        _ => "info",
    }
}

fn iso_date(millis: u64) -> Option<String> {
    timestamp(millis).map(|date| date.to_string())
}

pub(super) fn single_line(value: &str) -> String {
    let mut text = String::with_capacity(value.len());
    for word in value.split_whitespace() {
        if !text.is_empty() {
            text.push(' ');
        }
        text.extend(
            word.chars()
                .filter(|&character| is_display_character(character)),
        );
    }
    text
}

fn html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value
        .chars()
        .filter(|&character| is_display_character(character))
    {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            character => escaped.push(character),
        }
    }
    escaped
}

fn markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value
        .chars()
        .filter(|&character| is_display_character(character))
    {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '['
                | ']'
                | '<'
                | '>'
                | '|'
                | '~'
                | '#'
                | '&'
                | '!'
                | ':'
                | '-'
                | '+'
                | '.'
                | ')'
                | '='
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

const CSS: &str = "
body{margin:0;font:16px/1.55 system-ui,sans-serif;color:#1f2937;background:#f1f5f9}
main{max-width:68rem;margin:0 auto;padding:clamp(1rem,4vw,3rem)}
article{background:#fff;padding:clamp(1rem,4vw,2.5rem);margin:1rem 0;border:1px solid #dbe2eb;border-radius:.5rem}
h1,h2,h3,h4{line-height:1.25;color:#123b65;overflow-wrap:anywhere}
a{color:#075caa}p{white-space:pre-wrap;overflow-wrap:anywhere}
dl{display:grid;grid-template-columns:minmax(7rem,auto) minmax(0,1fr);gap:.35rem 1rem}
dt{font-weight:600}dd{margin:0;overflow-wrap:anywhere}
pre{padding:1rem;overflow:auto;background:#f1f5f9;border-radius:.35rem}
.diff .add{color:#166534;background:#dcfce7}.diff .remove{color:#991b1b;background:#fee2e2}
.diff-region .hunk{display:flex;gap:.5rem;flex-wrap:wrap;color:#1e3a8a;font-weight:600}
figure{margin:1rem 0}figure img{display:block;max-width:100%;height:auto}
footer{max-width:68rem;margin:0 auto;padding:1rem clamp(1rem,4vw,3rem) 2rem;color:#5e6e82;font-size:.85em;border-top:1px solid #dbe2eb;overflow-wrap:anywhere}
.watermark{position:fixed;inset:45% 10% auto;text-align:center;font-size:clamp(1.2rem,6vw,2.75rem);font-weight:700;opacity:.12;transform:rotate(-30deg);pointer-events:none;z-index:10;overflow-wrap:anywhere}
@media print{body{background:#fff}article{border:0;padding:0}pre{white-space:pre-wrap}}
";
