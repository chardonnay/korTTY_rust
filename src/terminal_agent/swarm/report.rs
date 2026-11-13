//! Exports masked Swarm results to an explicitly selected new file.

use std::fmt;
use std::fmt::Write as _;
use std::io;
use std::io::Write as _;
use std::path::{Component, Path};

use crate::private_files::Directory;
use crate::session_journal::export::pdf::{Font, HEADING, Layout, MARGIN, PROSE, Style, WIDTH};
use crate::session_journal::export_branding::PdfBranding;
use crate::session_journal::export_output::valid_component;

use super::{Queue, cell};

const MAX_EXPORT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub enum Format {
    #[default]
    Markdown,
    Text,
    Pdf,
}

impl Format {
    pub(crate) const ALL: [Self; 3] = [Self::Markdown, Self::Text, Self::Pdf];

    fn extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Text => "txt",
            Self::Pdf => "pdf",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Markdown => "Markdown",
            Self::Text => "Plain text",
            Self::Pdf => "PDF",
        })
    }
}

pub(crate) fn round(
    queue: &Queue,
    format: Format,
    comparison: Option<&str>,
) -> Result<String, &'static str> {
    if !queue.complete() || queue.rows().iter().any(|row| row.target.private) {
        return Err("Only completed, non-private swarm rounds can be exported.");
    }
    let mut report = if format == Format::Markdown {
        format!("# Swarm report\n\n{}", queue.report())
    } else {
        String::from("Swarm report\n")
    };
    for row in queue.rows() {
        if format == Format::Markdown {
            if row.transcript.is_empty() {
                continue;
            }
            let _ = writeln!(report, "\n## {}\n", cell(&row.target.label));
        } else {
            let _ = writeln!(report, "\n{} · {}", row.target.label, row.status.label());
            if !row.detail.is_empty() {
                let _ = writeln!(report, "{}", row.detail);
            }
        }
        append_body(&mut report, format, &row.transcript);
    }
    if let Some(comparison) = comparison {
        let heading = if format == Format::Markdown {
            "## AI comparison"
        } else {
            "AI comparison"
        };
        let _ = writeln!(report, "\n{heading}\n\n{comparison}");
    }
    Ok(report)
}

pub(crate) fn append_body(report: &mut String, format: Format, text: &str) {
    if text.is_empty() {
        return;
    }
    if format == Format::Markdown {
        for line in text.lines() {
            let _ = writeln!(report, "    {line}");
        }
        report.push('\n');
    } else {
        let _ = writeln!(report, "{text}\n");
    }
}

pub(crate) fn publish(
    path: &Path,
    format: Format,
    text: &str,
    check: impl Fn() -> io::Result<()>,
) -> io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str());
    let Some(name) = name.filter(|name| {
        path.is_absolute()
            && !path.components().any(|part| part == Component::ParentDir)
            && valid_component(name)
            && name.rsplit_once('.').is_some_and(|(stem, extension)| {
                !stem.is_empty() && extension.eq_ignore_ascii_case(format.extension())
            })
    }) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Choose an absolute report filename with the selected format extension.",
        ));
    };
    if text.len() > MAX_EXPORT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The swarm report exceeds its size limit.",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "The report directory is unavailable.",
        )
    })?;
    check()?;
    let pdf;
    let bytes = if format == Format::Pdf {
        pdf = render_pdf(text)?;
        pdf.as_slice()
    } else {
        text.as_bytes()
    };
    if bytes.len() > MAX_EXPORT_BYTES {
        return Err(io::Error::other("The swarm report exceeds its size limit."));
    }
    Directory::open(parent, false)?.publish_checked(name, |file| file.write_all(bytes), check)
}

fn render_pdf(text: &str) -> io::Result<Vec<u8>> {
    let mut layout = Layout::new(&|| false)?;
    let (title, body) = text.split_once('\n').unwrap_or((text, ""));
    layout.paragraph(
        [MARGIN, WIDTH],
        Style::new(Font::Bold, 18.0, HEADING),
        title,
        12.0,
    )?;
    layout.paragraph(
        [MARGIN, WIDTH],
        Style::new(Font::Regular, 10.0, PROSE),
        body,
        0.0,
    )?;
    layout.finish(&PdfBranding::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use crate::snippets::tests::Profile;
    use crate::terminal_agent::swarm::{Key, Status, Target};

    #[test]
    fn swarm_report_keeps_each_target_result_and_never_replaces_an_existing_file() {
        let profile = Profile::new();
        let path = profile.0.canonicalize().unwrap().join("swarm.md");
        let mut queue = Queue::new(
            1,
            vec![Target {
                key: Key::Live {
                    session: SessionId(1),
                    stream: 1,
                },
                label: "server | <tag>".into(),
                private: false,
            }],
            1,
        )
        .unwrap();
        let ticket = queue.reserve()[0];
        queue.change(
            ticket,
            Status::Succeeded,
            "Completed",
            "\n```\n# Output\n```",
        );
        let report = round(&queue, Format::Markdown, None).unwrap();
        assert!(report.contains("server \\| \\<tag\\>"));
        assert!(report.contains("    ```\n    # Output"));
        publish(&path, Format::Markdown, &report, || Ok(())).unwrap();
        assert!(publish(&path, Format::Markdown, "replacement", || Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), report);
        let plain = round(&queue, Format::Text, None).unwrap();
        assert!(plain.contains("server | <tag> · Completed"));
        let pdf = profile.0.canonicalize().unwrap().join("swarm.pdf");
        publish(&pdf, Format::Pdf, &plain, || Ok(())).unwrap();
        assert!(std::fs::read(pdf).unwrap().starts_with(b"%PDF-"));
    }
}
