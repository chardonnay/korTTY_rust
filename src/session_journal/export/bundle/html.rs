//! Generates the journal bundle's offline page from the validated export snapshot.

use std::collections::{HashSet, VecDeque};
use std::fmt::{self, Write as _};
use std::io;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::Serialize;

use crate::session_journal::capture::bundle::{Ranges, ScreenshotRef};
use crate::session_journal::export_branding::REPOSITORY_URL;
use crate::session_journal::links::{self, Source};
use crate::session_journal::{
    CaptureKind, CaptureRecord, display_text, invalid, is_display_character,
};

use super::super::{Agent, Entry, Options, Snapshot, strip};
use super::BundleStats;

const MAX_HTML_BYTES: usize = 64 * 1024 * 1024;
const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_ROWS: usize = 16_384;
const STYLE: &str = include_str!("html/view.css");
const SCRIPT: &str = include_str!("html/view.js");

#[cfg(test)]
mod tests;

pub(super) struct LogPreview {
    ranges: Ranges,
    rows: VecDeque<LogRow>,
    text_bytes: usize,
    limited: bool,
    incomplete: bool,
}

struct LogRow {
    sequence: u64,
    timestamp: Timestamp,
    kind: CaptureKind,
    text: String,
    repeat: u32,
}

impl LogPreview {
    pub(super) fn new(selection: &[&Entry]) -> Self {
        Self {
            ranges: Ranges::new(
                selection
                    .iter()
                    .map(|entry| (entry.log_start, entry.log_end)),
            ),
            rows: VecDeque::new(),
            text_bytes: 0,
            limited: false,
            incomplete: false,
        }
    }

    pub(super) fn push(&mut self, record: &CaptureRecord, screenshot: Option<&str>) {
        if self.limited && !self.ranges.contains(record.sequence) {
            return;
        }
        let text = if record.redacted {
            "(hidden input)".into()
        } else if record.kind == CaptureKind::Screenshot {
            safe_screenshot(screenshot.unwrap_or_default())
                .map_or_else(|| "Screenshot".into(), |path| format!("Screenshot {path}"))
        } else {
            display_text(&record.text)
        };
        self.text_bytes += text.len();
        self.rows.push_back(LogRow {
            sequence: record.sequence,
            timestamp: record.timestamp,
            kind: record.kind,
            text,
            repeat: record.repeat,
        });
        if !self.limited && (self.text_bytes > MAX_LOG_BYTES || self.rows.len() > MAX_LOG_ROWS) {
            self.limited = true;
            self.rows.retain(|row| self.ranges.contains(row.sequence));
            self.text_bytes = self.rows.iter().map(|row| row.text.len()).sum();
        }
        while self.text_bytes > MAX_LOG_BYTES || self.rows.len() > MAX_LOG_ROWS {
            let row = self
                .rows
                .pop_front()
                .expect("an over-limit log preview is nonempty");
            self.text_bytes -= row.text.len();
        }
    }

    pub(super) fn mark_incomplete(&mut self) {
        self.incomplete = true;
    }

    fn write_data(&self, html: &mut Html, zone: &TimeZone) -> fmt::Result {
        #[derive(Serialize)]
        struct Data<'a> {
            sequence: String,
            time: String,
            kind: &'static str,
            text: &'a str,
            repeat: u32,
        }
        // A template is inert and normal HTML text escaping protects its end tag.
        html.write_str("<template id=\"log-data\">[")?;
        for (index, row) in self.rows.iter().enumerate() {
            if index > 0 {
                html.write_char(',')?;
            }
            let data = Data {
                sequence: row.sequence.to_string(),
                time: row
                    .timestamp
                    .to_zoned(zone.clone())
                    .strftime("%T")
                    .to_string(),
                kind: match row.kind {
                    CaptureKind::Output => "Output",
                    CaptureKind::Input => "Input",
                    CaptureKind::Seed => "Scrollback",
                    CaptureKind::Screenshot => "Screenshot",
                    CaptureKind::Note => "Note",
                },
                text: &row.text,
                repeat: row.repeat,
            };
            let json = serde_json::to_string(&data).map_err(|_| fmt::Error)?;
            write!(html, "{}", Escaped(&json))?;
        }
        html.write_str("]</template>\n")
    }
}

fn log_range(entry: &Entry) -> Option<(u64, u64)> {
    let start = entry.log_start.or(entry.log_end)?;
    let end = entry.log_end.unwrap_or(start);
    Some((start.min(end), start.max(end)))
}

pub(super) fn render(
    snapshot: &Snapshot,
    selection: &[&Entry],
    options: &Options,
    log: &LogPreview,
    screenshots: &HashSet<String>,
    links_allowed: bool,
    counts: &BundleStats,
) -> io::Result<String> {
    let nonce = crate::private_files::nonce()?;
    let mut html = Html(String::new());
    Page {
        snapshot,
        selection,
        options,
        log,
        screenshots,
        links_allowed,
        counts,
    }
    .write(&mut html, &nonce)
    .map_err(|_| invalid("Journal HTML exceeds its size limit"))?;
    Ok(html.0)
}

struct Page<'a> {
    snapshot: &'a Snapshot,
    selection: &'a [&'a Entry],
    options: &'a Options,
    log: &'a LogPreview,
    screenshots: &'a HashSet<String>,
    links_allowed: bool,
    counts: &'a BundleStats,
}

impl Page<'_> {
    fn write(&self, html: &mut Html, nonce: &str) -> fmt::Result {
        let title = strip(&self.snapshot.header.title);
        let title = if title.is_empty() {
            "Session Journal"
        } else {
            title
        };
        write!(
            html,
            r#"<!doctype html>
<html lang="en" data-theme="system" data-scale="0"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="referrer" content="no-referrer">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src 'self' file:; style-src 'nonce-{nonce}'; script-src 'nonce-{nonce}'; base-uri 'none'; form-action 'none'">
<title>{}</title><style nonce="{nonce}">{STYLE}</style></head><body>
<header class="session-head"><h1>{}</h1>"#,
            Escaped(title),
            Escaped(title)
        )?;
        self.header(html)?;
        html.write_str("</header>\n")?;
        self.controls(html)?;
        self.timeline(html)?;
        if let Some(footer) = &self.options.branding.footer {
            write!(html, "<footer>{}", Escaped(footer.footer_label()))?;
            if footer.repository_link {
                write!(
                    html,
                    " · <a href=\"{REPOSITORY_URL}\" rel=\"noreferrer\">{REPOSITORY_URL}</a>"
                )?;
            }
            html.write_str("</footer>\n")?;
        }
        html.write_str(LOG_PANEL)?;
        self.log.write_data(html, &self.options.filter.zone)?;
        writeln!(
            html,
            "<script nonce=\"{nonce}\">{SCRIPT}</script></body></html>"
        )
    }

    fn header(&self, html: &mut Html) -> fmt::Result {
        let header = &self.snapshot.header;
        if !strip(&header.connection).is_empty() {
            writeln!(
                html,
                "<p class=\"connection\">{}</p>",
                Escaped(&header.connection)
            )?;
        }
        if !strip(&header.description).is_empty() {
            writeln!(
                html,
                "<p class=\"prose\">{}</p>",
                Escaped(&header.description)
            )?;
        }
        let zone = &self.options.filter.zone;
        let started = header.started_at.map(|date| date.timestamp());
        let duration = started.map_or_else(
            || "—".into(),
            |started| {
                let seconds = header
                    .ended_at
                    .unwrap_or_else(Timestamp::now)
                    .duration_since(started)
                    .as_secs()
                    .max(0);
                duration(seconds)
            },
        );
        let started = started.map_or_else(
            || "—".into(),
            |date| date.to_zoned(zone.clone()).strftime("%F %H:%M").to_string(),
        );
        write!(
            html,
            r#"<dl class="stats"><div><dt>Started</dt><dd>{}</dd></div>
<div><dt>Duration</dt><dd>{}</dd></div><div><dt>Entries</dt><dd>{}</dd></div>
<div><dt>Commands</dt><dd>{}</dd></div><div><dt>Errors</dt><dd>{}</dd></div>
<div><dt>Screenshots</dt><dd>{}</dd></div></dl>"#,
            Escaped(&started),
            Escaped(&duration),
            self.selection.len(),
            self.counts.command_count,
            self.counts.error_count,
            self.counts.screenshot_count
        )?;
        if self.options.filter.is_active() {
            writeln!(
                html,
                "<p class=\"notice\">Excerpt · {} of {} entries</p>",
                self.selection.len(),
                self.snapshot.entries.len()
            )?;
        }
        if self.log.limited || self.log.incomplete || self.counts.skipped_assets != 0 {
            html.write_str("<p class=\"notice\">")?;
            if self.log.limited {
                html.write_str("The embedded capture preview is limited. The available capture logs are in the bundle. ")?;
            }
            if self.log.incomplete {
                html.write_str("Some capture data could not be included. ")?;
            }
            if self.counts.skipped_assets != 0 {
                write!(
                    html,
                    "{} screenshot(s) could not be included.",
                    self.counts.skipped_assets
                )?;
            }
            html.write_str("</p>\n")?;
        }
        Ok(())
    }

    fn controls(&self, html: &mut Html) -> fmt::Result {
        html.write_str(PAGE_CONTROLS)?;
        let used: HashSet<_> = self
            .selection
            .iter()
            .filter_map(|entry| entry.marker)
            .collect();
        if !used.is_empty() {
            html.write_str(
                r#"<details id="marker-controls"><summary>Markers</summary><div class="controls">
<label for="marker">Marker</label><select id="marker"><option value="">All markers</option>"#,
            )?;
            for (index, marker) in self.snapshot.markers.iter().enumerate() {
                if used.contains(&index) {
                    write!(
                        html,
                        "<option value=\"{}\">{}</option>",
                        Escaped(&marker.id),
                        Escaped(&marker.name)
                    )?;
                }
            }
            html.write_str(r#"</select><button type="button" id="marker-prev">Previous marker</button>
<button type="button" id="marker-next">Next marker</button><output id="marker-count" aria-live="polite"></output>
</div></details>"#)?;
        }
        html.write_str("</nav>\n")
    }

    fn timeline(&self, html: &mut Html) -> fmt::Result {
        html.write_str("<main id=\"timeline\">\n")?;
        if self.selection.is_empty() {
            html.write_str("<p>No journal entries yet.</p>\n")?;
        }
        let mut previous_day = None;
        for (index, entry) in self.selection.iter().enumerate() {
            let local = entry
                .created_at
                .map(|date| date.timestamp().to_zoned(self.options.filter.zone.clone()));
            let day = local.as_ref().map(jiff::Zoned::date);
            if index == 0 || day != previous_day {
                if index != 0 {
                    html.write_str("</section>\n")?;
                }
                html.write_str("<section class=\"day\">")?;
                if let Some(day) = day {
                    writeln!(html, "<h2>{day}</h2>")?;
                }
            }
            previous_day = day;
            self.entry(html, entry, index, local.as_ref())?;
        }
        if !self.selection.is_empty() {
            html.write_str("</section>\n")?;
        }
        html.write_str("</main>\n")
    }

    fn entry(
        &self,
        html: &mut Html,
        entry: &Entry,
        index: usize,
        local: Option<&jiff::Zoned>,
    ) -> fmt::Result {
        let marker = entry
            .marker
            .and_then(|index| self.snapshot.markers.get(index));
        let (class, label) = match entry.kind.as_str() {
            "SCREENSHOT" => ("screenshot-entry", "Screenshot"),
            "USER_NOTE" => ("note-entry", "Note"),
            "SESSION_SUMMARY" => ("final-entry", "Session summary"),
            "AGENT" => ("agent-entry", "AI agent"),
            _ => ("summary-entry", ""),
        };
        write!(
            html,
            "<article class=\"entry {class}\" id=\"entry-{index}\" tabindex=\"-1\""
        )?;
        if let Some(marker) = marker {
            write!(html, " data-marker=\"{}\"", Escaped(&marker.id))?;
        }
        if let Some(date) = local {
            write!(html, " data-time=\"{}\"", date.strftime("%F %T"))?;
        }
        html.write_str("><div class=\"card-head\">")?;
        if let Some(date) = local {
            write!(
                html,
                "<time datetime=\"{}\">{}</time>",
                date.timestamp(),
                date.strftime("%H:%M")
            )?;
        }
        if let Some(marker) = marker {
            write!(
                html,
                "<span class=\"badge\" data-search>{}</span>",
                Escaped(&marker.name)
            )?;
        }
        if !label.is_empty() {
            write!(html, "<span class=\"badge\">{label}</span>")?;
        }
        html.write_str("</div>\n")?;
        if !strip(&entry.title).is_empty() {
            writeln!(html, "<h3 data-search>{}</h3>", Escaped(&entry.title))?;
        }
        if let Some(agent) = &entry.agent {
            agent_meta(html, agent)?;
        }
        self.screenshot(html, entry)?;
        self.prose(html, entry)?;
        excerpts(html, entry)?;
        if self.options.include_chats
            && let Some(agent) = &entry.agent
        {
            chat(html, agent, &self.options.filter.zone)?;
        }
        if !self.log.rows.is_empty()
            && let Some((from, to)) = log_range(entry)
        {
            writeln!(
                html,
                "<button type=\"button\" class=\"entry-log\" data-from=\"{from}\" data-to=\"{to}\">Show capture excerpt</button>"
            )?;
        }
        html.write_str("</article>\n")
    }

    fn screenshot(&self, html: &mut Html, entry: &Entry) -> fmt::Result {
        let Some(screenshot) = &entry.screenshot else {
            return Ok(());
        };
        if let Some(reference) =
            safe_screenshot(&screenshot.reference).filter(|path| self.screenshots.contains(*path))
        {
            writeln!(
                html,
                r#"<button type="button" class="screenshot" data-image="{reference}">
<img loading="lazy" src="{reference}" alt="Open screenshot"></button>"#
            )?;
        } else {
            html.write_str("<p class=\"notice\">Screenshot unavailable in this export.</p>\n")?;
        }
        if !strip(&screenshot.ai_description).is_empty() {
            writeln!(
                html,
                "<p class=\"prose\" data-search>{}</p>",
                Escaped(&screenshot.ai_description)
            )?;
        }
        for tag in &screenshot.tags {
            if !strip(tag).is_empty() {
                writeln!(
                    html,
                    "<button type=\"button\" class=\"tag\" data-search>{}</button>",
                    Escaped(tag)
                )?;
            }
        }
        Ok(())
    }

    fn prose(&self, html: &mut Html, entry: &Entry) -> fmt::Result {
        let source = match entry.kind.as_str() {
            "USER_NOTE" => Some(Source::Note),
            "AI_SUMMARY" | "SESSION_SUMMARY" => Some(Source::Summary),
            _ => None,
        };
        let collapse = entry.agent.is_some() && entry.text.chars().nth(500).is_some();
        if !strip(&entry.text).is_empty() {
            if collapse {
                html.write_str("<details><summary>Show full answer</summary>")?;
            }
            html.write_str("<p class=\"prose\" data-search>")?;
            write_links(html, &entry.text, source.filter(|_| self.links_allowed))?;
            html.write_str("</p>\n")?;
            if collapse {
                html.write_str("</details>\n")?;
            }
        }
        if !strip(&entry.note).is_empty() {
            html.write_str("<blockquote class=\"prose\" data-search>")?;
            write_links(
                html,
                &entry.note,
                self.links_allowed.then_some(Source::Note),
            )?;
            html.write_str("</blockquote>\n")?;
        }
        Ok(())
    }
}

fn duration(seconds: i64) -> String {
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, seconds / 60 % 60)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn safe_screenshot(path: &str) -> Option<&str> {
    ScreenshotRef::parse(path).map(|_| path)
}

fn write_links(html: &mut Html, value: &str, source: Option<Source>) -> fmt::Result {
    let Some(source) = source else {
        return write!(html, "{}", Escaped(value));
    };
    let (text, links) = links::load(value, Some(source));
    for span in links::spans(&text, &links, usize::MAX) {
        if let Some(target) = span.target {
            write!(
                html,
                "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>",
                Escaped(target.uri()),
                Escaped(span.text)
            )?;
        } else {
            write!(html, "{}", Escaped(span.text))?;
        }
    }
    Ok(())
}

fn excerpts(html: &mut Html, entry: &Entry) -> fmt::Result {
    for (class, name, prefix, lines) in [
        ("input", "Input excerpt", "$ ", &entry.input),
        ("output", "Output excerpt", "", &entry.output),
    ] {
        if !lines.is_empty() {
            write!(
                html,
                "<pre class=\"{class}\" aria-label=\"{name}\" data-search>"
            )?;
            for line in lines {
                writeln!(html, "{prefix}{}", Escaped(line))?;
            }
            html.write_str("</pre>\n")?;
        }
    }
    Ok(())
}

fn agent_meta(html: &mut Html, agent: &Agent) -> fmt::Result {
    let mut parts = Vec::new();
    if !strip(&agent.model).is_empty() {
        parts.push(agent.model.clone());
    }
    if agent.duration_millis > 0 {
        let seconds = (agent.duration_millis.saturating_add(500) / 1000).max(1);
        parts.push(duration(i64::try_from(seconds).unwrap_or(i64::MAX)));
    }
    if agent.tokens > 0 {
        parts.push(format!("{} tokens", agent.tokens));
    }
    if !parts.is_empty() {
        writeln!(
            html,
            "<p class=\"muted\" data-search>{}</p>",
            Escaped(&parts.join(" · "))
        )?;
    }
    Ok(())
}

fn chat(html: &mut Html, agent: &Agent, zone: &TimeZone) -> fmt::Result {
    let Some(chat) = &agent.chat else {
        return Ok(());
    };
    write!(
        html,
        "<details class=\"chat\"><summary>Show AI chat ({} messages)",
        chat.messages.len()
    )?;
    if let Some(captured) = chat.captured_at {
        write!(
            html,
            " · as of {}",
            captured.to_zoned(zone.clone()).strftime("%F %H:%M")
        )?;
    }
    if chat.frozen {
        html.write_str(" · final")?;
    }
    html.write_str("</summary>\n")?;
    if !strip(&chat.title).is_empty() {
        writeln!(html, "<h4 data-search>{}</h4>", Escaped(&chat.title))?;
    }
    for message in &chat.messages {
        let speaker = if message.user { "You" } else { "Assistant" };
        write!(html, "<div class=\"message\"><h4>{speaker}")?;
        if !strip(&message.profile).is_empty() {
            write!(html, " · {}", Escaped(&message.profile))?;
        }
        writeln!(
            html,
            "</h4><p class=\"prose\" data-search>{}</p>",
            Escaped(&message.content)
        )?;
        if !message.steps.is_empty() {
            writeln!(
                html,
                "<details><summary>Steps ({})</summary>",
                message.steps.len()
            )?;
            for step in &message.steps {
                html.write_str("<div class=\"step\">")?;
                for part in [&step.title, &step.status, &step.summary] {
                    if !strip(part).is_empty() {
                        writeln!(html, "<p class=\"prose\" data-search>{}</p>", Escaped(part))?;
                    }
                }
                for part in [&step.command, &step.output] {
                    if !strip(part).is_empty() {
                        writeln!(html, "<pre data-search>{}</pre>", Escaped(part))?;
                    }
                }
                if let Some(code) = step.exit_code {
                    writeln!(html, "<p>Exit code {code}</p>")?;
                }
                if step.truncated {
                    html.write_str("<p class=\"notice\">Output truncated.</p>")?;
                }
                html.write_str("</div>\n")?;
            }
            html.write_str("</details>\n")?;
        }
        html.write_str("</div>\n")?;
    }
    html.write_str("</details>\n")
}

struct Html(String);

impl fmt::Write for Html {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.len() > MAX_HTML_BYTES - self.0.len() {
            return Err(fmt::Error);
        }
        self.0.push_str(text);
        Ok(())
    }
}

struct Escaped<'a>(&'a str);

impl fmt::Display for Escaped<'_> {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut start = 0;
        for (index, character) in self.0.char_indices() {
            let replacement = match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                '\'' => "&#39;",
                character if !is_display_character(character) => "",
                _ => continue,
            };
            output.write_str(&self.0[start..index])?;
            output.write_str(replacement)?;
            start = index + character.len_utf8();
        }
        output.write_str(&self.0[start..])
    }
}

const PAGE_CONTROLS: &str = r#"<nav id="page-controls" aria-label="Journal tools">
<div class="controls"><button id="theme" type="button">Theme: system</button>
<button id="font-smaller" type="button" aria-label="Smaller font">A−</button>
<button id="font-reset" type="button" aria-label="Reset font">A</button>
<button id="font-larger" type="button" aria-label="Larger font">A+</button></div>
<details id="search-controls"><summary>Search journal</summary><div class="controls">
<label for="journal-search">Search</label><input id="journal-search" type="search" maxlength="512">
<button id="journal-prev" type="button">Previous match</button><button id="journal-next" type="button">Next match</button>
<label><input id="only-matches" type="checkbox"> Only matches</label><output id="journal-count" aria-live="polite"></output>
</div></details>
<details id="time-controls"><summary>Jump to a time</summary><div class="controls">
<label for="time-search">Time</label><input id="time-search" type="text" maxlength="32" placeholder="19:00 or 2026-11-07 19:00">
<button id="time-go" type="button">Jump</button><output id="time-status" aria-live="polite"></output>
</div></details>
"#;

const LOG_PANEL: &str = r#"<aside id="capture-panel" hidden aria-label="Capture log">
<div class="controls"><strong id="capture-title">Capture log</strong><button type="button" id="capture-close">Close log</button></div>
<div class="controls"><label for="capture-search">Search log</label><input id="capture-search" type="search" maxlength="512">
<button type="button" id="capture-prev">Previous match</button><button type="button" id="capture-next">Next match</button>
<output id="capture-count" aria-live="polite"></output></div>
<div class="controls"><button type="button" id="capture-prev-page">Previous page</button>
<button type="button" id="capture-next-page">Next page</button><output id="capture-page"></output></div>
<ol id="capture-rows"></ol></aside>
<dialog id="image-dialog"><button type="button" id="image-close">Close screenshot</button><img id="large-image" alt="Screenshot"></dialog>
"#;
