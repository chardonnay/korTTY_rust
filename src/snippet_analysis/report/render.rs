use std::collections::BTreeMap;
use std::io::{self, Write};

use serde::ser::{SerializeSeq as _, Serializer as _};
use serde_json::{Value, json};

use crate::session_journal::is_display_character;

use super::{Format, Item, Options, Packing, Subject, timestamp};

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
) -> io::Result<()> {
    if options.format == Format::Json {
        return write_json(output, items, options, packing);
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
    };
    document.begin(title, &options.locale)?;
    if combined {
        document.contents(items, &options.skipped)?;
    }
    for (index, item) in items.iter().enumerate() {
        document.report(item, options, combined.then_some(index + 1))?;
    }
    document.finish()
}

fn write_json(
    output: &mut impl Write,
    items: &[Item],
    options: &Options,
    packing: Packing,
) -> io::Result<()> {
    if packing == Packing::Combined {
        let mut serializer = serde_json::Serializer::pretty(&mut *output);
        let mut sequence = serializer.serialize_seq(Some(items.len()))?;
        for item in items {
            sequence.serialize_element(&report_json(item, options))?;
        }
        sequence.end()?;
    } else {
        serde_json::to_writer_pretty(&mut *output, &report_json(&items[0], options))?;
    }
    output.write_all(b"\n")
}

fn report_json(item: &Item, options: &Options) -> Value {
    let record = &item.record;
    let source = &record.source;
    let provenance = &record.provenance;
    let snippet_id = match item.subject {
        Subject::Snippet(id) => String::from(id),
        Subject::Folder(_) => String::new(),
    };
    let findings: Vec<_> = record
        .analysis
        .improvements
        .iter()
        .map(|finding| {
            json!({
                "id": finding.id,
                "section": section(&finding.category),
                "category": finding.category,
                "severity": severity(&finding.severity),
                "severityLabel": finding.severity,
                "title": finding.title,
                "detail": finding.detail,
                "recommendation": finding.recommendation,
                "line": finding.line.filter(|line| *line > 0),
                "selected": false,
                "changes": [],
            })
        })
        .collect();
    let dependencies: Vec<_> = record
        .analysis
        .dependencies
        .iter()
        .map(|dependency| {
            json!({
                "id": dependency.id,
                "name": dependency.name,
                "kind": dependency.kind,
                "purpose": dependency.purpose,
                "suggestion": dependency.suggestion,
                "selected": false,
                "changes": [],
            })
        })
        .collect();
    json!({
        "schema": SCHEMA,
        "kind": "PRE_APPLY",
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
            "codeTextLanguage": source.code_text_language_code,
            "analysedAt": iso_date(record.analyzed_at),
            "appliedAt": null,
            "sourceSha256": source.sha256,
            "stale": item.stale(),
            "runNumber": 0,
            "runCount": 0,
        },
        "summary": record.analysis.summary,
        "findings": findings,
        "dependencies": dependencies,
        "diagram": null,
        "apply": null,
        "verification": null,
    })
}

struct Document<'a, W> {
    output: &'a mut W,
    html: bool,
}

impl<W: Write> Document<'_, W> {
    fn begin(&mut self, title: &str, locale: &str) -> io::Result<()> {
        if self.html {
            writeln!(
                self.output,
                "<!doctype html>\n<html lang=\"{}\"><head><meta charset=\"utf-8\">\
                 <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
                 <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\
                 <title>{}</title><style>{CSS}</style></head><body><main>",
                html(locale),
                html(title),
            )?;
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        if self.html {
            writeln!(self.output, "</main></body></html>")?;
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

    fn report(&mut self, item: &Item, options: &Options, index: Option<usize>) -> io::Result<()> {
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
        self.heading(depth, &label(item))?;
        self.text("Before applying changes")?;
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
                self.heading(depth + 2, &dependency.name)?;
                self.field("Kind", &dependency.kind)?;
                self.field("Purpose", &dependency.purpose)?;
                self.field("Suggestion", &dependency.suggestion)?;
            }
        }
        self.heading(depth + 1, "Diagram")?;
        self.text("No diagram available.")?;
        if options.include_code {
            self.heading(depth + 1, "Analysed code")?;
            let source = &item.record.source;
            if let Some(content) = &source.content {
                self.code(content, &source.language)?;
            } else {
                self.text("The analysed code was not stored.")?;
            }
        }
        if self.html {
            writeln!(self.output, "</article>")?;
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
        let entries = [
            ("Script", source.snippet_name.as_str()),
            ("Language", source.language.as_str()),
            ("AI profile", provenance.profile_name.as_str()),
            ("AI model", provenance.model.as_str()),
            ("AI skills", skills.as_str()),
            ("Analysis language", source.report_language_code.as_str()),
            (
                "Code text language",
                source.code_text_language_code.as_str(),
            ),
            ("Analysed at", analyzed_at.as_str()),
            ("Exported at", exported_at.as_str()),
            ("Source SHA-256", source.sha256.as_str()),
            ("Findings", finding_count.as_str()),
            ("Dependencies", dependency_count.as_str()),
        ];
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
                self.heading(depth + 1, &finding.title)?;
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
@media print{body{background:#fff}article{border:0;padding:0}pre{white-space:pre-wrap}}
";
