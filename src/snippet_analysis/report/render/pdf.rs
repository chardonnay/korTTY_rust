use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::session_journal::export::pdf::{
    Font, HEADING, Layout, MARGIN, PROSE, Panel, Style, WIDTH,
};
use crate::snippet_analysis::diagram::AnalysisDiagram;
use crate::snippet_analysis::model::ApplyRun;
use crate::snippet_window::compare::ExportDiff;

use super::{Item, Options, Packing, ReportBody, check_cancelled, label, single_line};

mod cover;

const BODY: [f32; 2] = [MARGIN, WIDTH];

pub(super) fn write(
    output: &mut impl Write,
    items: &[Item],
    options: &Options,
    packing: Packing,
    cancelled: &AtomicBool,
    rendered: &mut impl FnMut(&Item),
) -> io::Result<usize> {
    let is_cancelled = || cancelled.load(Ordering::Relaxed);
    let mut document = Layout::new(&is_cancelled)?;
    let combined = packing == Packing::Combined;
    let mut contents = Vec::new();
    if combined {
        document.heading(1, "Code analysis reports")?;
        document.heading(2, "Contents")?;
        for (index, item) in items.iter().enumerate() {
            contents.push(document.contents_row(&format!(
                "{}. {}",
                index + 1,
                single_line(&label(item))
            ))?);
        }
        if !options.skipped.is_empty() {
            document.heading(2, "Snippets without a stored analysis")?;
            for name in &options.skipped {
                document.text(&single_line(name))?;
            }
        }
    }
    let mut contents = contents.into_iter();
    let mut failed_diagrams = 0;
    for item in items {
        check_cancelled(cancelled)?;
        if let Some(row) = contents.next() {
            document.new_page()?;
            document.link_contents_row(row)?;
        }
        document.bookmark(&single_line(&label(item)))?;
        cover::draw(&mut document, item)?;
        document.new_page()?;
        failed_diagrams += usize::from(document.report_body(item, options, 1)?);
        rendered(item);
    }
    output.write_all(&document.finish(&options.branding)?)?;
    Ok(failed_diagrams)
}

impl ReportBody for Layout<'_> {
    fn heading(&mut self, depth: usize, value: &str) -> io::Result<()> {
        self.ensure_portrait()?;
        let style = heading_style(depth);
        self.ensure_space(style.size + 24.0)?;
        self.paragraph(BODY, style, &single_line(value), 4.0)
    }

    fn text(&mut self, value: &str) -> io::Result<()> {
        if value.trim().is_empty() {
            return Ok(());
        }
        self.paragraph(BODY, Style::new(Font::Regular, 10.2, PROSE), value, 4.0)
    }

    fn field(&mut self, name: &str, value: &str) -> io::Result<()> {
        if value.trim().is_empty() {
            return Ok(());
        }
        self.text(&format!("{name}: {value}"))
    }

    fn code(&mut self, content: &str, _language: &str) -> io::Result<()> {
        self.panel(BODY, Panel::Output, content.lines().map(|line| ("", line)))
    }

    fn diff(&mut self, diff: &ExportDiff, run: &ApplyRun) -> io::Result<()> {
        for (index, hunk) in diff.hunks.iter().enumerate() {
            self.hunk_notes(index, hunk, run)?;
            let lines = std::iter::once(("", hunk.header.as_str())).chain(
                hunk.lines
                    .iter()
                    .map(|line| line.split_at_checked(1).unwrap_or(("", line))),
            );
            self.panel(BODY, Panel::Diff, lines)?;
        }
        Ok(())
    }

    fn diagram(&mut self, depth: usize, saved: &AnalysisDiagram) -> io::Result<bool> {
        let image = saved.report_png().and_then(|png| {
            image::load_from_memory_with_format(&png, image::ImageFormat::Png)
                .map_err(|error| error.to_string())
        });
        match image {
            Ok(image) => {
                self.report_image(
                    &image.into_rgb8(),
                    ["Diagram", "Diagram (continued)"],
                    heading_style(depth),
                )?;
                self.diagram_note(saved)?;
                Ok(false)
            }
            Err(error) => {
                self.heading(depth, "Diagram")?;
                self.diagram_note(saved)?;
                self.text(&format!(
                    "The saved flowchart could not be rendered ({error})."
                ))?;
                Ok(true)
            }
        }
    }

    fn metadata_fields(&mut self, entries: &[(&str, &str)]) -> io::Result<()> {
        for (name, value) in entries {
            self.field(name, &single_line(value))?;
        }
        Ok(())
    }
}

fn heading_style(depth: usize) -> Style {
    let size = match depth {
        1 => 18.0,
        2 => 13.0,
        _ => 11.0,
    };
    Style::new(Font::Bold, size, HEADING)
}
