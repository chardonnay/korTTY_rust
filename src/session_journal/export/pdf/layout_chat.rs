use std::fmt::Write as _;
use std::io;

use jiff::tz::TimeZone;

use super::super::{Chat, Message, Step};
use super::document::Font;
use super::layout::{AMBER, BLUE, HEADING, Layout, MARGIN, MUTED, PROSE, Panel, Style, WIDTH};
use super::{Entry, chat, strip};
const APPENDIX: &str = "AI chats";
const BODY: [f32; 2] = [MARGIN, WIDTH];

impl Layout<'_> {
    pub(super) fn chats(&mut self, selected: &[&Entry], zone: &TimeZone) -> io::Result<()> {
        let mut chats = selected
            .iter()
            .filter_map(|&entry| chat(entry).map(|chat| (entry, chat)))
            .peekable();
        if chats.peek().is_none() {
            return Ok(());
        }
        self.new_page()?;
        self.divider(APPENDIX)?;
        self.ensure_space(60.0)?;
        let parent = self.document.bookmark(self.page, self.y, APPENDIX, None)?;
        for (entry, chat) in chats {
            self.check_cancelled()?;
            self.ensure_space(60.0)?;
            let title = title(entry, chat);
            self.document
                .bookmark(self.page, self.y, title, Some(parent))?;
            self.paragraph(BODY, Style::new(Font::Bold, 12.5, HEADING), title, 2.0)?;
            let captured = chat.captured_at.map_or_else(
                || "?".to_owned(),
                |date| date.to_zoned(zone.clone()).strftime("%F %H:%M").to_string(),
            );
            let suffix = if chat.frozen { " (final)" } else { "" };
            self.paragraph(
                BODY,
                Style::new(Font::Regular, 9.0, MUTED),
                &format!("As of: {captured}{suffix}"),
                8.0,
            )?;
            for message in &chat.messages {
                self.chat_message(message)?;
            }
            self.y -= 14.0;
        }
        Ok(())
    }

    fn chat_message(&mut self, message: &Message) -> io::Result<()> {
        self.ensure_space(30.0)?;
        let profile = strip(&message.profile);
        let (role, color) = if message.user {
            ("You", BLUE)
        } else if profile.is_empty() {
            ("AI:", HEADING)
        } else {
            (profile, HEADING)
        };
        self.paragraph(BODY, Style::new(Font::Bold, 10.0, color), role, 2.0)?;
        if !strip(&message.content).is_empty() {
            self.paragraph(
                BODY,
                Style::new(Font::Regular, 9.8, PROSE),
                &message.content,
                4.0,
            )?;
        }
        for step in &message.steps {
            self.check_cancelled()?;
            let headline = step_headline(step);
            self.paragraph(BODY, Style::new(Font::Regular, 8.8, MUTED), &headline, 2.0)?;
            let command = strip(&step.command);
            let lines = [("$ ", command), ("", step.output.as_str())];
            self.panel(
                BODY,
                Panel::Output,
                lines.into_iter().filter(|(_, text)| !text.is_empty()),
            )?;
            if step.truncated {
                self.paragraph(
                    BODY,
                    Style::new(Font::Regular, 8.4, AMBER),
                    "Output truncated",
                    4.0,
                )?;
            }
        }
        self.y -= 6.0;
        Ok(())
    }
}

fn title<'a>(entry: &'a Entry, chat: &'a Chat) -> &'a str {
    [strip(&chat.title), strip(&entry.title)]
        .into_iter()
        .find(|title| !title.is_empty())
        .unwrap_or("AI chat")
}

fn step_headline(step: &Step) -> String {
    let mut text = String::new();
    let status = strip(&step.status);
    if !status.is_empty() {
        write!(text, "[{status}] ").expect("writing to a String");
    }
    text.push_str(strip(&step.title));
    let summary = strip(&step.summary);
    if !summary.is_empty() {
        write!(text, ": {summary}").expect("writing to a String");
    }
    if let Some(code) = step.exit_code {
        write!(text, " · Exit {code}").expect("writing to a String");
    }
    text
}
