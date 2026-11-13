//! Local exports of the bounded, masked activity retained by terminal Agent.

use std::fmt::{self, Write as _};
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::private_files::{Directory, EntryIdentity};
use crate::session_journal::export::pdf::{
    Font, HEADING, Layout, MARGIN, Metadata, PROSE, Style, WIDTH,
};
use crate::session_journal::export_branding::PdfBranding;
use crate::session_journal::export_output::valid_component;

mod usage;

pub(crate) use usage::{ModelAttempt, ModelUsage, TokenCount};

const TITLE: &str = "Terminal AI Agent activity";
const MAX_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_RUNS: usize = 5;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Format {
    #[default]
    Markdown,
    Text,
    Yaml,
    Xml,
    Json,
    Pdf,
    Asciidoctor,
}

impl Format {
    pub(crate) const ALL: [Self; 7] = [
        Self::Markdown,
        Self::Text,
        Self::Yaml,
        Self::Xml,
        Self::Json,
        Self::Pdf,
        Self::Asciidoctor,
    ];

    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Text => "txt",
            Self::Yaml => "yaml",
            Self::Xml => "xml",
            Self::Json => "json",
            Self::Pdf => "pdf",
            Self::Asciidoctor => "adoc",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Markdown => "Markdown (.md)",
            Self::Text => "Plain text (.txt)",
            Self::Yaml => "YAML (.yaml)",
            Self::Xml => "XML (.xml)",
            Self::Json => "JSON (.json)",
            Self::Pdf => "PDF (.pdf)",
            Self::Asciidoctor => "Asciidoctor (.adoc)",
        })
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Run {
    pub(crate) mode: &'static str,
    pub(crate) status: &'static str,
    pub(crate) destination: String,
    pub(crate) profile_and_model: String,
    pub(crate) prompt: String,
    pub(crate) started_at: String,
    pub(crate) finished_at: String,
    pub(crate) elapsed_seconds: u64,
    pub(crate) activity: Vec<String>,
    pub(crate) model_usage: Option<ModelUsage>,
    pub(crate) model_attempts: Vec<ModelAttempt>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Document<'a> {
    title: &'static str,
    exported_at: String,
    runs: &'a [Run],
}

pub(crate) fn render(
    format: Format,
    exported_at: jiff::Timestamp,
    runs: &[Run],
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Vec<u8>> {
    if runs.is_empty() || runs.len() > MAX_RUNS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "No eligible agent activity.",
        ));
    }
    let document = Document {
        title: TITLE,
        exported_at: exported_at.to_string(),
        runs,
    };
    let bytes = match format {
        Format::Markdown | Format::Text | Format::Asciidoctor => {
            prose(&document, format).into_bytes()
        }
        Format::Yaml => yaml(&document)?.into_bytes(),
        Format::Xml => xml(&document).into_bytes(),
        Format::Json => serde_json::to_vec_pretty(&document)?,
        Format::Pdf => pdf(&document, exported_at, cancelled)?,
    };
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::other(
            "The agent activity export exceeds its size limit.",
        ));
    }
    Ok(bytes)
}

impl Run {
    fn fields(&self) -> [(&str, &str); 7] {
        [
            ("mode", self.mode),
            ("status", self.status),
            ("destination", &self.destination),
            ("profileAndModel", &self.profile_and_model),
            ("prompt", &self.prompt),
            ("startedAt", &self.started_at),
            ("finishedAt", &self.finished_at),
        ]
    }

    fn usage_label(&self) -> String {
        self.model_usage
            .as_ref()
            .map_or_else(|| "Reported tokens unavailable".into(), ModelUsage::label)
    }
}

fn prose(document: &Document<'_>, format: Format) -> String {
    let (heading, subheading, indent) = match format {
        Format::Markdown => ("# ", "## ", "    "),
        Format::Asciidoctor => ("= ", "== ", " "),
        _ => ("", "", "  "),
    };
    let mut text = format!("{heading}{TITLE}\n\nExported at {}\n", document.exported_at);
    for (index, run) in document.runs.iter().enumerate() {
        let _ = writeln!(text, "\n{subheading}Run {}\n", index + 1);
        for (label, value) in run.fields() {
            let _ = writeln!(text, "{label}\n");
            literal(&mut text, indent, value, format);
        }
        let _ = writeln!(text, "Elapsed seconds: {}\n", run.elapsed_seconds);
        let _ = writeln!(text, "{}\n", run.usage_label());
        for (index, attempt) in run.model_attempts.iter().enumerate() {
            let _ = writeln!(text, "Model attempt {}\n", index + 1);
            literal(&mut text, indent, &attempt.label(), format);
        }
        for (step, activity) in run.activity.iter().enumerate() {
            let _ = writeln!(text, "Activity {}\n", step + 1);
            literal(&mut text, indent, activity, format);
        }
    }
    text
}

fn literal(output: &mut String, indent: &str, value: &str, format: Format) {
    for line in value.split('\n') {
        output.push_str(indent);
        if format == Format::Asciidoctor
            && ["include::", "ifdef::", "ifndef::", "ifeval::", "endif::"]
                .iter()
                .any(|directive| line.trim_start().starts_with(directive))
        {
            output.push('\\');
        }
        output.push_str(line);
        output.push('\n');
    }
    output.push('\n');
}

fn yaml(document: &Document<'_>) -> serde_json::Result<String> {
    // JSON quoted strings and flow mappings are valid YAML 1.2.
    let mut text = format!(
        "title: {TITLE:?}\nexportedAt: {:?}\nruns:\n",
        document.exported_at
    );
    for run in document.runs {
        for (index, (name, value)) in run.fields().into_iter().enumerate() {
            let prefix = if index == 0 { "  - " } else { "    " };
            let value = serde_json::Value::String(value.into());
            let _ = writeln!(text, "{prefix}{name}: {value}");
        }
        let _ = writeln!(text, "    elapsedSeconds: {}", run.elapsed_seconds);
        let summary = serde_json::to_string(&run.model_usage)?;
        let _ = writeln!(text, "    modelUsage: {summary}");
        if run.model_attempts.is_empty() {
            text.push_str("    modelAttempts: []\n");
        } else {
            text.push_str("    modelAttempts:\n");
            for attempt in &run.model_attempts {
                let attempt = serde_json::to_string(attempt)?;
                let _ = writeln!(text, "      - {attempt}");
            }
        }
        if run.activity.is_empty() {
            text.push_str("    activity: []\n");
        } else {
            text.push_str("    activity:\n");
            for entry in &run.activity {
                let value = serde_json::Value::String(entry.clone());
                let _ = writeln!(text, "      - {value}");
            }
        }
    }
    Ok(text)
}

fn xml(document: &Document<'_>) -> String {
    let mut text =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<terminalAgentExport>\n");
    element(&mut text, "  ", "title", TITLE);
    element(&mut text, "  ", "exportedAt", &document.exported_at);
    text.push_str("  <runs>\n");
    for run in document.runs {
        text.push_str("    <run>\n");
        for (name, value) in run.fields() {
            element(&mut text, "      ", name, value);
        }
        let _ = writeln!(
            text,
            "      <elapsedSeconds>{}</elapsedSeconds>",
            run.elapsed_seconds
        );
        if let Some(usage) = &run.model_usage {
            usage.write_xml(&mut text);
        } else {
            text.push_str("      <modelUsage unavailable=\"true\"/>\n");
        }
        text.push_str("      <modelAttempts>\n");
        for attempt in &run.model_attempts {
            attempt.write_xml(&mut text);
        }
        text.push_str("      </modelAttempts>\n");
        text.push_str("      <activity>\n");
        for entry in &run.activity {
            element(&mut text, "        ", "entry", entry);
        }
        text.push_str("      </activity>\n    </run>\n");
    }
    text.push_str("  </runs>\n</terminalAgentExport>\n");
    text
}

fn element(output: &mut String, indent: &str, name: &str, value: &str) {
    let _ = write!(output, "{indent}<{name}>");
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            _ => output.push(character),
        }
    }
    let _ = writeln!(output, "</{name}>");
}

fn pdf(
    document: &Document<'_>,
    created: jiff::Timestamp,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Vec<u8>> {
    let mut layout = Layout::new(cancelled)?;
    let body = Style::new(Font::Regular, 10.0, PROSE);
    let heading = Style::new(Font::Bold, 14.0, HEADING);
    let text_width = [MARGIN, WIDTH];
    layout.paragraph(text_width, heading, TITLE, 10.0)?;
    layout.paragraph(
        text_width,
        body,
        &format!("Exported at {}", document.exported_at),
        10.0,
    )?;
    for (index, run) in document.runs.iter().enumerate() {
        layout.ensure_space(54.0)?;
        layout.paragraph(text_width, heading, &format!("Run {}", index + 1), 6.0)?;
        for (label, value) in run.fields() {
            layout.paragraph(text_width, body, &format!("{label}: {value}"), 2.0)?;
        }
        layout.paragraph(
            text_width,
            body,
            &format!("Elapsed seconds: {}", run.elapsed_seconds),
            8.0,
        )?;
        layout.paragraph(text_width, body, &run.usage_label(), 8.0)?;
        for (step, activity) in run.activity.iter().enumerate() {
            layout.paragraph(
                text_width,
                body,
                &format!("Activity {}\n{activity}", step + 1),
                6.0,
            )?;
        }
    }
    layout.finish_with_metadata(
        &PdfBranding::default(),
        Some(Metadata {
            title: TITLE,
            subject: "Retained terminal AI Agent activity",
            keywords: "korTTY, terminal, agent",
            created,
        }),
    )
}

pub(crate) fn valid_path(path: &Path, format: Format) -> bool {
    path.is_absolute()
        && !path.components().any(|part| part == Component::ParentDir)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                valid_component(name)
                    && name.rsplit_once('.').is_some_and(|(stem, extension)| {
                        !stem.is_empty() && extension.eq_ignore_ascii_case(format.extension())
                    })
            })
}

pub(crate) struct Destination {
    path: PathBuf,
    parent: EntryIdentity,
}

impl Destination {
    pub(crate) fn review(path: PathBuf, format: Format) -> io::Result<Self> {
        if !valid_path(&path, format) {
            return Err(io::Error::other(
                "Choose an absolute new export path with the selected format's extension.",
            ));
        }
        let parent = open_parent(&path)?
            .identity()
            .map_err(|_| directory_unavailable())?;
        Ok(Self { path, parent })
    }

    fn current_parent(&self) -> io::Result<Directory> {
        let directory = open_parent(&self.path)?;
        if directory.identity().map_err(|_| directory_unavailable())? != self.parent {
            return Err(io::Error::other(
                "The selected export directory changed. Review the path again.",
            ));
        }
        Ok(directory)
    }

    pub(crate) fn publish(&self, bytes: &[u8], cancelled: &dyn Fn() -> bool) -> io::Result<()> {
        if bytes.len() > MAX_BYTES {
            return Err(io::Error::other(
                "The agent activity export exceeds its size limit.",
            ));
        }
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("The export filename is invalid."))?;
        let check_cancelled = || {
            if cancelled() {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "The agent activity export was cancelled.",
                ))
            } else {
                Ok(())
            }
        };
        check_cancelled()?;
        let parent = self.current_parent()?;
        parent
            .publish_checked(
                name,
                |file| {
                    for chunk in bytes.chunks(64 * 1024) {
                        check_cancelled()?;
                        file.write_all(chunk)?;
                    }
                    Ok(())
                },
                || {
                    self.current_parent()?;
                    check_cancelled()
                },
            )
            .map_err(|error| match error.kind() {
                io::ErrorKind::AlreadyExists => {
                    io::Error::other("The export file already exists. Choose a new filename.")
                }
                io::ErrorKind::Interrupted => {
                    io::Error::other("The agent activity export was cancelled.")
                }
                _ => io::Error::other(
                    "Could not save the agent activity export to the reviewed directory.",
                ),
            })
    }
}

fn open_parent(path: &Path) -> io::Result<Directory> {
    let parent = path.parent().ok_or_else(directory_unavailable)?;
    Directory::open(parent, false).map_err(|_| directory_unavailable())
}

fn directory_unavailable() -> io::Error {
    io::Error::other("The export directory is unavailable.")
}

#[cfg(test)]
mod tests;
