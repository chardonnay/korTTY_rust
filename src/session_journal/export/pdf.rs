//! Local A4 journal reports over the same validated snapshot and filters as other exports.

use std::io;
use std::sync::LazyLock;

use jiff::civil::Date;
use jiff::tz::Offset;
use regex::Regex;

use crate::session_journal::export_branding::PdfBranding;

use super::markdown::{describe_filter, duration};
use super::{Agent, Chat, Entry, Options, Preview, Selection, Snapshot, strip};

mod document;
mod layout;
mod layout_chat;
mod layout_image;
mod layout_text;

use document::Font;
use layout::{
    AMBER, BLUE, BODY_WIDTH, BODY_X, HEADING, Layout, MARGIN, MUTED, PROSE, Panel, Style, WHITE,
    WIDTH, check_cancelled,
};
use layout_image::Screenshots;

#[cfg(test)]
#[path = "pdf/layout_tests.rs"]
mod tests;

pub(crate) struct PdfExport {
    pub bytes: Vec<u8>,
    pub skipped_assets: usize,
}

#[cfg(test)]
impl Snapshot {
    pub(crate) fn render_pdf(
        &self,
        options: &Options,
        branding: &PdfBranding,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<PdfExport> {
        check_cancelled(cancelled)?;
        self.select_for_export(&options.filter, None, cancelled)?
            .render_pdf(options, branding, cancelled)
    }
}

impl Selection<'_> {
    /// Renders a bounded, searchable PDF. The publisher owns any filesystem writes.
    pub(crate) fn render_pdf(
        &self,
        options: &Options,
        branding: &PdfBranding,
        cancelled: &dyn Fn() -> bool,
    ) -> io::Result<PdfExport> {
        check_cancelled(cancelled)?;
        self.require_matches(&options.filter)?;
        let snapshot = self.snapshot;
        let source = snapshot.open_current_source()?;
        let mut images = Screenshots::new(&source);
        let mut layout = Layout::new(cancelled)?;
        layout.header(snapshot, options, self.preview, self.ai_applied)?;
        let zone = &options.filter.zone;
        let mut day: Option<Date> = None;
        for &entry in &self.entries {
            layout.check_cancelled()?;
            let date = entry
                .created_at
                .map(|date| date.timestamp().to_zoned(zone.clone()));
            if let Some(date) = &date
                && day != Some(date.date())
            {
                day = Some(date.date());
                layout.divider(&date.strftime("%F").to_string())?;
            }
            let time = date.map_or_else(String::new, |date| date.strftime("%H:%M").to_string());
            layout.entry(snapshot, entry, &time, options, &mut images)?;
        }
        if options.include_chats {
            layout.chats(&self.entries, zone)?;
        }
        let bytes = layout.finish(branding)?;
        check_cancelled(cancelled)?;
        snapshot.identity.require_document(source.file())?;
        snapshot.open_current_source()?;
        Ok(PdfExport {
            bytes,
            skipped_assets: images.skipped,
        })
    }
}

impl Layout<'_> {
    fn header(
        &mut self,
        snapshot: &Snapshot,
        options: &Options,
        preview: Preview,
        ai_applied: bool,
    ) -> io::Result<()> {
        let header = &snapshot.header;
        const HERO_X: f32 = MARGIN + 16.0;
        const HERO_WIDTH: f32 = WIDTH - 32.0;
        const LIGHT_BLUE: [u8; 3] = [0xd9, 0xea, 0xff];
        self.document
            .fill_rect(self.page, [MARGIN, self.y - 78.0, WIDTH, 78.0], BLUE)?;
        self.fitted(
            [HERO_X, self.y - 28.0],
            HERO_WIDTH,
            Style::new(Font::Bold, 18.0, WHITE),
            &header.title,
        )?;
        if !header.connection.is_empty() {
            self.fitted(
                [HERO_X, self.y - 46.0],
                HERO_WIDTH,
                Style::new(Font::Regular, 9.5, LIGHT_BLUE),
                &header.connection,
            )?;
        }
        let started = header.started_at.map_or_else(
            || "?".to_owned(),
            |date| {
                let zone = Offset::from_seconds(date.offset_seconds())
                    .unwrap_or(Offset::UTC)
                    .to_time_zone();
                date.timestamp()
                    .to_zoned(zone)
                    .strftime("%F %H:%M")
                    .to_string()
            },
        );
        let meta = format!(
            "Started: {started}  ·  Duration: {}  ·  Commands: {}  ·  Errors: {}  ·  Screenshots: {}",
            duration(header),
            header.commands,
            header.errors,
            header.screenshots
        );
        let meta_y = self.y
            - if header.connection.is_empty() {
                50.0
            } else {
                62.0
            };
        self.fitted(
            [HERO_X, meta_y],
            HERO_WIDTH,
            Style::new(Font::Regular, 9.0, LIGHT_BLUE),
            &meta,
        )?;
        self.y -= 92.0;
        if !strip(&header.description).is_empty() {
            self.paragraph(
                [MARGIN, WIDTH],
                Style::new(Font::Regular, 10.0, MUTED),
                &header.description,
                6.0,
            )?;
        }
        if options.filter.is_active() {
            let filter = describe_filter(&options.filter, snapshot, ai_applied);
            let label = format!(
                "Excerpt: {filter} · {} of {} entries",
                preview.kept_entries, preview.total_entries
            );
            self.paragraph(
                [MARGIN, WIDTH],
                Style::new(Font::Bold, 9.5, AMBER),
                &label,
                8.0,
            )?;
        }
        Ok(())
    }

    fn entry<'a>(
        &mut self,
        snapshot: &Snapshot,
        entry: &'a Entry,
        time: &str,
        options: &Options,
        images: &mut Screenshots<'a>,
    ) -> io::Result<()> {
        self.ensure_space(36.0)?;
        let body = [BODY_X, BODY_WIDTH];
        let title = entry_title(entry);
        self.text(
            [MARGIN, self.y - 10.0],
            Style::new(Font::Mono, 9.5, [0x6e, 0x77, 0x81]),
            time,
        )?;
        let mut badge = 0.0;
        if let Some(marker) = entry.marker {
            badge = self.badge(&snapshot.markers[marker].name, entry.marker_color)?;
        }
        if !strip(&title).is_empty() {
            self.fitted(
                [BODY_X + badge, self.y - 12.0],
                BODY_WIDTH - badge,
                Style::new(Font::Bold, 11.5, HEADING),
                &title,
            )?;
        }
        self.y -= 18.0;
        if let Some(agent) = &entry.agent {
            let meta = agent_meta(agent);
            if !meta.is_empty() {
                self.paragraph(body, Style::new(Font::Regular, 8.8, MUTED), &meta, 2.0)?;
            }
        }
        if !strip(&entry.text).is_empty() {
            let prose = if entry.kind == "USER_NOTE" {
                std::borrow::Cow::Borrowed(entry.text.as_str())
            } else {
                std::borrow::Cow::Owned(strip_summary_markup(&entry.text))
            };
            self.paragraph(body, Style::new(Font::Regular, 10.2, PROSE), &prose, 3.0)?;
        }
        if !entry.input.is_empty() {
            self.panel(
                body,
                Panel::Input,
                entry.input.iter().map(|line| ("$ ", line.as_str())),
            )?;
        }
        if !entry.output.is_empty() {
            self.panel(
                body,
                Panel::Output,
                entry.output.iter().map(|line| ("", line.as_str())),
            )?;
        }
        if !strip(&entry.note).is_empty() {
            self.paragraph(
                body,
                Style::new(Font::Regular, 9.8, AMBER),
                &format!("Note: {}", entry.note),
                3.0,
            )?;
        }
        if options.include_chats && chat(entry).is_some() {
            self.paragraph(
                body,
                Style::new(Font::Bold, 9.6, BLUE),
                "AI chat: see the AI chats appendix",
                3.0,
            )?;
        }
        if let Some(screenshot) = &entry.screenshot {
            if options.include_screenshots {
                images.draw(self, &screenshot.reference)?;
            }
            if !strip(&screenshot.ai_description).is_empty() {
                self.paragraph(
                    body,
                    Style::new(Font::Regular, 9.8, PROSE),
                    &format!("AI description: {}", screenshot.ai_description),
                    3.0,
                )?;
            }
            if !screenshot.tags.is_empty() {
                self.paragraph(
                    body,
                    Style::new(Font::Regular, 9.2, [0x6b, 0x72, 0x80]),
                    &format!("Tags: {}", screenshot.tags.join(", ")),
                    3.0,
                )?;
            }
        }
        self.y -= 10.0;
        Ok(())
    }

    fn badge(&mut self, name: &str, color: [u8; 3]) -> io::Result<f32> {
        let style = Style::new(Font::Bold, 7.5, WHITE);
        let label = layout_text::fit(
            &self.document,
            style,
            BODY_WIDTH / 2.0,
            &name.to_uppercase(),
        );
        let width = self.document.text_width(style.font, style.size, &label) + 10.0;
        let y = self.y - 12.5;
        self.document
            .fill_rect(self.page, [BODY_X, y - 2.5, width, 11.0], color)?;
        self.text([BODY_X + 5.0, y], style, &label)?;
        Ok(width + 6.0)
    }
}

fn entry_title(entry: &Entry) -> String {
    let title = &entry.title;
    let mut title = if entry.kind == "SESSION_SUMMARY" {
        if strip(title).is_empty() {
            "Session summary".to_owned()
        } else {
            format!("Session summary: {title}")
        }
    } else {
        title.to_owned()
    };
    if entry.run_as_root && !strip(&title).is_empty() {
        title.push_str(" · ROOT");
    }
    title
}

fn strip_summary_markup(value: &str) -> String {
    static COMMAND: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"`([^`\n]{1,80})`").expect("static PDF command expression"));
    static BULLET: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^[-*] ").expect("static PDF bullet expression"));
    BULLET
        .replace_all(&COMMAND.replace_all(value, "$1"), "• ")
        .into_owned()
}

fn agent_meta(agent: &Agent) -> String {
    let mut parts = Vec::new();
    let model = strip(&agent.model);
    if !model.is_empty() {
        parts.push(model.to_owned());
    }
    if agent.duration_millis != 0 {
        let seconds = (agent.duration_millis.saturating_add(500) / 1000).max(1);
        parts.push(match seconds {
            3600.. => format!("{} h {} min", seconds / 3600, (seconds / 60) % 60),
            60.. => format!("{} min {} s", seconds / 60, seconds % 60),
            _ => format!("{seconds} s"),
        });
    }
    if agent.tokens != 0 {
        let tokens = if agent.tokens < 1000 {
            agent.tokens.to_string()
        } else {
            format!("{:.1}k", agent.tokens as f64 / 1000.0)
        };
        parts.push(format!("{tokens} tokens"));
    }
    parts.join(" · ")
}

fn chat(entry: &Entry) -> Option<&Chat> {
    entry
        .agent
        .as_ref()?
        .chat
        .as_ref()
        .filter(|chat| !chat.messages.is_empty())
}
