use std::collections::{HashMap, HashSet};
#[cfg(windows)]
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Component, Path};

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::{Offset, TimeZone};

use crate::private_files::FileStamp;
use crate::session_journal::capture::bundle::ScreenshotRef;
use crate::session_journal::export_branding::REPOSITORY_URL;
use crate::sftp_window::LocalSource;

use super::super::metadata::require_single_link;
use super::super::viewer::screenshot::require_published_image;
use super::super::{MAX_PNG_BYTES, MAX_PNG_PIXELS, is_display_character, screenshot_reference};
use super::{
    Agent, Chat, Entry, Filter, Header, MarkerMode, Options, Preview, Selection, Snapshot,
    TimeWindow,
};
use super::{display_text, invalid, strip};

const MAX_EXPORT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct Asset {
    pub name: String,
    pub bytes: Vec<u8>,
}

pub(crate) struct Markdown {
    pub text: String,
    pub assets: Vec<Asset>,
    pub skipped_assets: usize,
}

#[cfg(test)]
impl Snapshot {
    pub(crate) fn render(&self, options: &Options, asset_dir_name: &str) -> io::Result<Markdown> {
        self.select_for_export(&options.filter, None, &|| false)?
            .render(options, asset_dir_name)
    }
}

impl Selection<'_> {
    pub(crate) fn render(&self, options: &Options, asset_dir_name: &str) -> io::Result<Markdown> {
        if !is_component(asset_dir_name) {
            return Err(invalid("Invalid journal export asset directory name"));
        }
        self.require_matches(&options.filter)?;
        let snapshot = self.snapshot;
        let source = snapshot.open_current_source()?;
        let mut output = Output::new(asset_dir_name, self.preview);
        output.header(snapshot, &options.filter, self.ai_applied);
        let mut day = None;
        for (index, &entry) in self.entries.iter().enumerate() {
            output.entry(snapshot, &source, entry, index, options, &mut day)?;
            output.check_size()?;
        }
        if let Some(footer) = &options.branding.footer {
            write!(output.text, "---\n\n_{}_", inline(footer.footer_label()))
                .expect("writing to a String");
            if footer.repository_link {
                write!(output.text, " — <{REPOSITORY_URL}>").expect("writing to a String");
            }
            output.text.push('\n');
        }
        output.check_size()?;
        snapshot.identity.require_document(source.file())?;
        snapshot.open_current_source()?;
        Ok(Markdown {
            text: output.text,
            assets: output.assets,
            skipped_assets: output.skipped_assets,
        })
    }
}

struct Output {
    text: String,
    asset_dir: String,
    assets: Vec<Asset>,
    asset_bytes: usize,
    asset_names: HashSet<String>,
    next_asset_suffix: HashMap<String, usize>,
    screenshots: HashMap<String, Option<String>>,
    skipped_assets: usize,
    preview: Preview,
}

impl Output {
    fn new(asset_dir_name: &str, preview: Preview) -> Self {
        Self {
            text: String::new(),
            asset_dir: encode_component(asset_dir_name),
            assets: Vec::new(),
            asset_bytes: 0,
            asset_names: HashSet::new(),
            next_asset_suffix: HashMap::new(),
            screenshots: HashMap::new(),
            skipped_assets: 0,
            preview,
        }
    }

    fn check_size(&self) -> io::Result<()> {
        if self.text.len() + self.asset_bytes > MAX_EXPORT_BYTES {
            return Err(invalid(
                "Journal Markdown export exceeds the total size limit",
            ));
        }
        Ok(())
    }

    fn remaining(&self) -> usize {
        MAX_EXPORT_BYTES.saturating_sub(self.text.len() + self.asset_bytes)
    }

    fn header(&mut self, snapshot: &Snapshot, filter: &Filter, ai_applied: bool) {
        let header = &snapshot.header;
        writeln!(self.text, "# {}\n", inline(&header.title)).expect("writing to a String");
        if !header.connection.is_empty() {
            writeln!(
                self.text,
                "- **Connection:** {}",
                inline(&header.connection)
            )
            .expect("writing to a String");
        }
        let started = header.started_at.map_or_else(
            || "?".to_owned(),
            |date| {
                let offset = Offset::from_seconds(date.offset_seconds()).unwrap_or(Offset::UTC);
                date.timestamp()
                    .to_zoned(offset.to_time_zone())
                    .strftime("%F %H:%M")
                    .to_string()
            },
        );
        writeln!(
            self.text,
            "- **Started:** {started} · **Duration:** {}",
            duration(header)
        )
        .expect("writing to a String");
        writeln!(
            self.text,
            "- **Commands:** {} · **Errors:** {} · **Screenshots:** {}",
            header.commands, header.errors, header.screenshots
        )
        .expect("writing to a String");
        if !strip(&header.description).is_empty() {
            writeln!(
                self.text,
                "- **Description:** {}",
                inline(&header.description)
            )
            .expect("writing to a String");
        }
        if filter.is_active() {
            let description = describe_filter(filter, snapshot, ai_applied);
            writeln!(
                self.text,
                "- **Excerpt: {} · {} of {} entries**",
                inline(&description),
                self.preview.kept_entries,
                self.preview.total_entries
            )
            .expect("writing to a String");
        }
        self.text.push('\n');
    }

    fn entry(
        &mut self,
        snapshot: &Snapshot,
        source: &LocalSource,
        entry: &Entry,
        index: usize,
        options: &Options,
        day: &mut Option<Date>,
    ) -> io::Result<()> {
        let zone = &options.filter.zone;
        let mut time = String::new();
        if let Some(created_at) = entry.created_at {
            let zoned = created_at.timestamp().to_zoned(zone.clone());
            if *day != Some(zoned.date()) {
                *day = Some(zoned.date());
                writeln!(self.text, "## {}\n", zoned.strftime("%F")).expect("writing to a String");
            }
            time = zoned.strftime("%H:%M").to_string();
        }
        write!(self.text, "### {time}").expect("writing to a String");
        if !strip(&entry.title).is_empty() {
            write!(self.text, " — {}", inline(&entry.title)).expect("writing to a String");
        }
        if let Some(marker) = entry.marker {
            let name = inline_display(&snapshot.markers[marker].name);
            let fence = backticks(&name, 1);
            write!(self.text, "  {fence}[{name}]{fence}").expect("writing to a String");
        }
        self.text.push_str("\n\n");
        if let Some(agent) = &entry.agent {
            let meta = agent_meta(agent);
            if !meta.is_empty() {
                writeln!(self.text, "{meta}\n").expect("writing to a String");
            }
        }
        if let Some(screenshot) = &entry.screenshot {
            if options.include_screenshots {
                self.screenshot(source, &screenshot.reference);
            }
            if !strip(&screenshot.ai_description).is_empty() {
                writeln!(
                    self.text,
                    "**AI description:** {}\n",
                    inline(&screenshot.ai_description)
                )
                .expect("writing to a String");
            }
            if !screenshot.tags.is_empty() {
                writeln!(
                    self.text,
                    "**Tags:** {}\n",
                    inline(&screenshot.tags.join(", "))
                )
                .expect("writing to a String");
            }
        }
        if !strip(&entry.text).is_empty() {
            writeln!(self.text, "{}\n", escaped(&entry.text)).expect("writing to a String");
        }
        if !entry.input.is_empty() {
            self.text.push_str("**Input:**\n");
            let input = entry
                .input
                .iter()
                .map(|line| format!("$ {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            fenced(&mut self.text, "console", &input, "");
        }
        if !entry.output.is_empty() {
            self.text.push_str("**Output:**\n");
            fenced(&mut self.text, "text", &entry.output.join("\n"), "");
        }
        if !strip(&entry.note).is_empty() {
            writeln!(self.text, "> Note: {}\n", inline(&entry.note)).expect("writing to a String");
        }
        if options.include_chats
            && let Some(chat) = entry.agent.as_ref().and_then(|agent| agent.chat.as_ref())
            && !chat.messages.is_empty()
        {
            self.chat(chat, entry, index, zone)?;
        }
        Ok(())
    }

    fn screenshot(&mut self, source: &LocalSource, reference: &str) {
        let name = match self.screenshots.get(reference) {
            Some(name) => name.clone(),
            None => {
                let name = self.add_screenshot(source, reference).ok();
                if name.is_none() {
                    self.skipped_assets += 1;
                }
                self.screenshots.insert(reference.to_owned(), name.clone());
                name
            }
        };
        if let Some(name) = name {
            writeln!(
                self.text,
                "![Screenshot]({}/{})\n",
                self.asset_dir,
                encode_component(&name)
            )
            .expect("writing to a String");
        }
    }

    fn add_screenshot(&mut self, source: &LocalSource, reference: &str) -> io::Result<String> {
        let reference = screenshot_reference(reference)
            .ok_or_else(|| invalid("Invalid journal screenshot reference"))?;
        let name = reference.trim_start_matches("screenshots/");
        let bytes = load_screenshot(source, reference, self.remaining() as u64)?;
        let name = self.unique_asset_name(name);
        self.add_asset(name.clone(), bytes)?;
        Ok(name)
    }

    fn chat(
        &mut self,
        chat: &Chat,
        entry: &Entry,
        index: usize,
        zone: &TimeZone,
    ) -> io::Result<()> {
        let id = if entry.id.is_empty()
            || entry.id.len() > 64
            || !entry
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            format!("entry-{}", index + 1)
        } else {
            entry.id.clone()
        };
        let name = self.unique_asset_name(&format!("chat-{id}.md"));
        let title = [strip(&chat.title), strip(&entry.title), "AI chat"]
            .into_iter()
            .find(|title| !title.is_empty())
            .unwrap_or("AI chat")
            .replace('[', "(")
            .replace(']', ")");
        let mut transcript = format!(
            "# {}\n\n- **As of:** {}",
            inline(&title),
            chat.captured_at.map_or_else(
                || "?".to_owned(),
                |date| { date.to_zoned(zone.clone()).strftime("%F %H:%M").to_string() }
            )
        );
        if chat.frozen {
            transcript.push_str(" (final)");
        }
        transcript.push_str("\n\n");
        for (index, message) in chat.messages.iter().enumerate() {
            if index != 0 {
                transcript.push_str("\n\n");
            }
            let role = if message.user {
                "You:"
            } else if strip(&message.profile).is_empty() {
                "AI:"
            } else {
                &message.profile
            };
            writeln!(transcript, "## {}\n", inline(role)).expect("writing to a String");
            transcript.push_str(&escaped(
                message
                    .content
                    .trim_matches(|character: char| character <= ' '),
            ));
            if !message.steps.is_empty() {
                writeln!(transcript, "\n\n### Steps ({})\n", message.steps.len())
                    .expect("writing to a String");
            }
            for step in &message.steps {
                transcript.push_str("- ");
                if !strip(&step.status).is_empty() {
                    write!(transcript, "[{}] ", inline(strip(&step.status)))
                        .expect("writing to a String");
                }
                transcript.push_str(&inline(strip(&step.title)));
                if !strip(&step.summary).is_empty() {
                    write!(transcript, ": {}", inline(strip(&step.summary)))
                        .expect("writing to a String");
                }
                if let Some(code) = step.exit_code {
                    write!(transcript, " · Exit {code}").expect("writing to a String");
                }
                transcript.push('\n');
                let mut body = String::new();
                if !strip(&step.command).is_empty() {
                    write!(body, "$ {}", step.command).expect("writing to a String");
                }
                if !step.output.is_empty() {
                    if !body.is_empty() {
                        body.push('\n');
                    }
                    body.push_str(&step.output);
                }
                if !body.is_empty() {
                    transcript.push('\n');
                    fenced(&mut transcript, "text", &body, "  ");
                }
                if step.truncated {
                    transcript.push_str("\n  _Output truncated_\n");
                }
            }
        }
        transcript.push('\n');
        self.add_asset(name.clone(), transcript.into_bytes())?;
        writeln!(
            self.text,
            "**AI chat:** [{}]({}/{})\n",
            inline(&title),
            self.asset_dir,
            encode_component(&name)
        )
        .expect("writing to a String");
        Ok(())
    }

    fn unique_asset_name(&mut self, name: &str) -> String {
        let key = name.to_ascii_lowercase();
        if self.asset_names.insert(key.clone()) {
            return name.to_owned();
        }
        let (base, extension) = name
            .rsplit_once('.')
            .expect("generated asset names have an extension");
        let next = self.next_asset_suffix.entry(key).or_insert(2);
        loop {
            let name = format!("{base}-{next}.{extension}");
            *next += 1;
            if self.asset_names.insert(name.to_ascii_lowercase()) {
                return name;
            }
        }
    }

    fn add_asset(&mut self, name: String, bytes: Vec<u8>) -> io::Result<()> {
        if bytes.len() > self.remaining() {
            return Err(invalid(
                "Journal Markdown assets exceed the total size limit",
            ));
        }
        self.asset_bytes += bytes.len();
        self.assets.push(Asset { name, bytes });
        Ok(())
    }
}

pub(super) fn duration(header: &Header) -> String {
    let Some(started) = header.started_at else {
        return "?".to_owned();
    };
    let seconds = header
        .ended_at
        .unwrap_or_else(Timestamp::now)
        .duration_since(started.timestamp())
        .as_secs();
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds / 60) % 60)
    } else {
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

fn agent_meta(agent: &Agent) -> String {
    let mut parts = Vec::new();
    if !strip(&agent.model).is_empty() {
        parts.push(inline(strip(&agent.model)));
    }
    if agent.duration_millis > 0 {
        let seconds = (agent.duration_millis.saturating_add(500) / 1000).max(1);
        parts.push(match seconds {
            3600.. => format!("{} h {} min", seconds / 3600, (seconds / 60) % 60),
            60.. => format!("{} min {} s", seconds / 60, seconds % 60),
            _ => format!("{seconds} s"),
        });
    }
    if agent.tokens > 0 {
        parts.push(if agent.tokens < 1000 {
            format!("{} tokens", agent.tokens)
        } else {
            format!("{:.1}k tokens", agent.tokens as f64 / 1000.0)
        });
    }
    parts.join(" · ")
}

pub(super) fn describe_filter(filter: &Filter, snapshot: &Snapshot, ai_applied: bool) -> String {
    let mut parts = Vec::new();
    let windows = filter
        .windows
        .iter()
        .filter(|window| !window.is_empty())
        .map(describe_window)
        .collect::<Vec<_>>()
        .join(", ");
    if !windows.is_empty() {
        parts.push(windows);
    }
    if !filter.topic().is_empty() {
        let label = if ai_applied { "AI topic" } else { "Topic" };
        parts.push(format!("{label}: {}", filter.topic()));
    }
    match filter.marker_mode {
        MarkerMode::All => {}
        MarkerMode::Marked => parts.push("Markers: only marked entries".into()),
        MarkerMode::Selected => {
            let mut ids = filter.marker_ids.iter().collect::<Vec<_>>();
            ids.sort_unstable();
            let names = ids
                .into_iter()
                .map(|id| {
                    snapshot
                        .markers
                        .iter()
                        .find(|marker| marker.id == *id)
                        .map_or(id.as_str(), |marker| marker.name.as_str())
                })
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("Markers: {names}"));
        }
    }
    parts.join(" · ")
}

fn describe_window(window: &TimeWindow) -> String {
    let label = |date: Option<Date>, time: Option<jiff::civil::Time>| match (date, time) {
        (Some(date), Some(time)) => format!("{date} {}", time.strftime("%H:%M")),
        (Some(date), None) => date.to_string(),
        (None, Some(time)) => time.strftime("%H:%M").to_string(),
        (None, None) => String::new(),
    };
    let from = label(window.from_date, window.from_time);
    let to = label(window.to_date, window.to_time);
    if from.is_empty() {
        format!("until {to}")
    } else if to.is_empty() {
        format!("from {from}")
    } else {
        format!("{from}–{to}")
    }
}

fn inline_display(value: &str) -> String {
    display_text(value).replace(['\n', '\t'], " ")
}

fn inline(value: &str) -> String {
    escaped(&inline_display(value))
}

fn escaped(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value
        .chars()
        .filter(|&character| is_display_character(character))
    {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '~' | '=' => {
                output.push('\\');
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    output
}

fn fenced(output: &mut String, language: &str, body: &str, indent: &str) {
    let body = display_text(body);
    let fence = backticks(&body, 3);
    writeln!(output, "{indent}{fence}{language}").expect("writing to a String");
    for line in body.split('\n') {
        writeln!(output, "{indent}{line}").expect("writing to a String");
    }
    writeln!(output, "{indent}{fence}\n").expect("writing to a String");
}

fn backticks(value: &str, minimum: usize) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(minimum.max(longest + 1))
}

fn is_component(value: &str) -> bool {
    let mut components = Path::new(value).components();
    !value.is_empty()
        && !value.contains(['/', '\\'])
        && !value.chars().any(char::is_control)
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
}

fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a String");
        }
    }
    encoded
}

pub(in crate::session_journal) fn load_screenshot(
    source: &LocalSource,
    reference: &str,
    budget: u64,
) -> io::Result<Vec<u8>> {
    let reference = ScreenshotRef::parse(reference)
        .ok_or_else(|| invalid("Invalid journal screenshot reference"))?;
    let name = reference
        .as_str()
        .strip_prefix("screenshots/")
        .expect("validated screenshot reference");
    screenshot_bytes(|| open_screenshot(source, name), budget)
}

pub(in crate::session_journal) fn screenshot_bytes(
    open: impl Fn() -> io::Result<File>,
    budget: u64,
) -> io::Result<Vec<u8>> {
    let file = open()?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let limit = budget.min(MAX_PNG_BYTES);
    let size = file.metadata()?.len();
    if size > limit {
        return Err(invalid("Journal screenshot exceeds the export size limit"));
    }
    let mut bytes = Vec::new();
    (&file).take(size + 1).read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    let current = open()?;
    require_single_link(&current)?;
    stamp.require_unchanged(&current)?;
    if bytes.len() as u64 != size
        || bytes.len() < 33
        || !bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")
    {
        return Err(invalid("Invalid journal screenshot PNG header"));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("PNG width header"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("PNG height header"));
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PNG_PIXELS {
        return Err(invalid("Journal screenshot exceeds the image size limit"));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_screenshot(source: &LocalSource, name: &str) -> io::Result<File> {
    use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, statat};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let folder = openat(
        source.parent_file(),
        "screenshots",
        flags | OFlags::DIRECTORY,
        Mode::empty(),
    )?;
    require_published_image(name, |stage| {
        match statat(&folder, stage, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(rustix::io::Errno::NOENT) => Ok(false),
            Err(error) => Err(error.into()),
        }
    })?;
    let stat = statat(&folder, name, AtFlags::SYMLINK_NOFOLLOW)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(invalid("Expected a regular journal screenshot file"));
    }
    let file = openat(folder, name, flags | OFlags::NONBLOCK, Mode::empty())?;
    Ok(File::from(file))
}

#[cfg(windows)]
fn open_screenshot(source: &LocalSource, name: &str) -> io::Result<File> {
    use kortty_files::tree::Directory;
    let folder = source
        .parent_directory()
        .read_child(OsStr::new("screenshots"))?
        .ok_or_else(|| invalid("Invalid journal screenshot directory"))?;
    let folder = Directory::from_file(folder)?;
    require_published_image(name, |stage| match folder.read_child(OsStr::new(stage)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    })?;
    folder
        .read_child(OsStr::new(name))?
        .ok_or_else(|| invalid("Invalid journal screenshot file"))
}
