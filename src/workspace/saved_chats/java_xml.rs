//! Explicit interchange with the conversation records in Java kortty's ai-chats.xml.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::path::{Component, Path};

use pgp::crypto::hash::HashAlgorithm;
use roxmltree::{Document, Node, ParsingOptions};
use zeroize::Zeroizing;

use crate::ai_manager::ProfileRef;
use crate::private_files::{Directory, FileStamp, nonce, require_single_link};
use crate::terminal_logging::Redactor;

use super::super::terminal_ai::{
    MAX_ANSWER_BYTES, MAX_CONTEXT_BYTES, MAX_QUESTION_BYTES, Turn,
    agent_follow_up::Step,
    mask,
    web::{MAX_STORED_SOURCES, Source},
};
use super::store::{
    self, Attachment, AttachmentKind, Imported, JavaOrigin, MAX_IMPORTED_TURNS, MAX_TITLE_CHARS,
};

mod agent;

const MAX_XML_BYTES: usize = 32 * 1024 * 1024;
const MAX_XML_NODES: u32 = 262_144;
const MAX_XML_CHATS: usize = 256;
const CHAT_FIELDS: &[&str] = &[
    "id",
    "title",
    "createdAt",
    "updatedAt",
    "selectedText",
    "connectionDisplayName",
    "responseLanguageCode",
    "activeAiProfileId",
    "activeAiProfileName",
    "messages",
    "attachmentFileName",
    "attachmentSourcePath",
    "attachmentContent",
    "origin",
    "sourceRunId",
    "sourceTerminalSessionId",
    "sourceConnectionId",
    "sourceJournalId",
    "sourceJournalEntryId",
];
const MESSAGE_FIELDS: &[&str] = &[
    "role",
    "content",
    "createdAt",
    "aiProfileId",
    "aiProfileName",
    "reasoning",
    "webToolCalls",
    "agentSteps",
];

pub(super) struct ImportReport {
    pub(super) saved: Vec<store::Summary>,
    pub(super) duplicates: usize,
    pub(super) full: usize,
    pub(super) failed: usize,
    pub(super) unsupported: usize,
    pub(super) invalid: usize,
}

pub(super) struct ExportReport {
    pub(super) count: usize,
    pub(super) sync_failed: bool,
    pub(super) cleanup_failed: bool,
}

pub(super) fn import(root: &Path, path: &Path, redactor: &Redactor) -> io::Result<ImportReport> {
    let (directory, name) = selected_file(path)?;
    let file = directory.open_file(name)?;
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::new());
    (&file)
        .take(MAX_XML_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    let Parsed {
        chats,
        unsupported,
        invalid,
    } = parse(&bytes, redactor)?;
    let stored = store::import(root, chats)?;
    Ok(ImportReport {
        saved: stored.saved,
        duplicates: stored.duplicates,
        full: stored.full,
        failed: stored.failed,
        unsupported,
        invalid: invalid + stored.invalid,
    })
}

pub(super) fn export(root: &Path, path: &Path, redactor: &Redactor) -> io::Result<ExportReport> {
    if !path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
    {
        return Err(invalid("Choose an XML export filename ending in .xml."));
    }
    let (directory, name) = selected_file(path)?;
    if directory.has_entry(name)? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "The Saved Chats XML export file already exists.",
        ));
    }
    let mut count = 0;
    let cleanup_failed = publish_xml(&directory, name, |file| {
        let mut xml = BoundedXml {
            file: BufWriter::new(file),
            written: 0,
        };
        xml.write_all(
            b"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<aiChats>\n",
        )?;
        store::with_records(root, |record| {
            write_chat(&mut xml, &record, redactor)?;
            count += 1;
            Ok(())
        })?;
        xml.write_all(b"</aiChats>\n")?;
        xml.flush()
    })?;
    Ok(ExportReport {
        count,
        sync_failed: directory.sync().is_err(),
        cleanup_failed,
    })
}

fn publish_xml(
    destination: &Directory,
    name: &str,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<bool> {
    let stage_name = format!(".kortty-chat-export-{}", nonce()?);
    let stage_name = OsStr::new(&stage_name);
    let stage = destination.create_private_subdirectory(stage_name)?;
    let identity = stage.identity()?;
    let result = (|| {
        stage.publish_with(name, write)?;
        let stamp = stage.open_private_file(name).and_then(|file| {
            require_single_link(&file)?;
            FileStamp::read(&file)
        })?;
        stage.sync()?;
        if !stage.same_as(&destination.deletion_child(stage_name)?)? {
            return Err(invalid("The Saved Chats XML staging directory changed."));
        }
        stamp.require_unchanged(&stage.open_private_file(name)?)?;
        stage.move_entry(OsStr::new(name), destination)
    })();
    if result.is_err() {
        let _ = stage.remove_copy(name);
    }
    // Release the directory handle before removing its name on Windows.
    drop(stage);
    let cleanup = destination.remove_deletion_entry(stage_name, identity, true);
    result?;
    Ok(cleanup.is_err())
}

fn selected_file(path: &Path) -> io::Result<(Directory, &str)> {
    let Some((parent, name)) = path
        .parent()
        .zip(path.file_name().and_then(|name| name.to_str()))
    else {
        return Err(invalid("Choose a valid Saved Chats XML filename."));
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
        || path.as_os_str().as_encoded_bytes().len() > 4096
        || name.len() > 240
        || name.ends_with(['.', ' '])
        || name.contains(['/', '\\', ':', '"', '<', '>', '|', '?', '*'])
        || name.chars().any(char::is_control)
    {
        return Err(invalid("Choose a valid Saved Chats XML filename."));
    }
    Ok((Directory::open(parent, false)?, name))
}

#[derive(Default)]
pub(super) struct Parsed {
    pub(super) chats: Vec<Imported>,
    pub(super) unsupported: usize,
    pub(super) invalid: usize,
}

pub(super) fn parse(bytes: &[u8], redactor: &Redactor) -> io::Result<Parsed> {
    let document = document(bytes)?;
    let root = document.root_element();
    if !is_element(root, "aiChats") {
        return Err(invalid("Expected a Java kortty aiChats document."));
    }
    let chats = children(root, "chat", MAX_XML_CHATS)?;
    let mut result = Parsed::default();
    for chat in chats {
        match parse_chat(chat, redactor) {
            Ok(Some(chat)) => result.chats.push(chat),
            Ok(None) => result.unsupported += 1,
            Err(_) => result.invalid += 1,
        }
    }
    Ok(result)
}

pub(super) fn document(bytes: &[u8]) -> io::Result<Document<'_>> {
    if bytes.len() > MAX_XML_BYTES {
        return Err(invalid("Saved Chats XML exceeds the file size limit."));
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let xml = std::str::from_utf8(bytes).map_err(|_| invalid("Saved Chats XML must use UTF-8."))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Saved Chats XML is invalid or exceeds the XML node limit."))?;
    if document.root().children().any(|node| node.is_pi()) {
        return Err(invalid(
            "Saved Chats XML contains a processing instruction.",
        ));
    }
    Ok(document)
}

pub(super) type Fields<'a, 'input> = BTreeMap<&'input str, Node<'a, 'input>>;

pub(super) fn is_element(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn ignored(node: Node<'_, '_>) -> bool {
    node.is_comment() || (node.is_text() && node.text().is_none_or(|text| text.trim().is_empty()))
}

pub(super) fn children<'a, 'input>(
    parent: Node<'a, 'input>,
    name: &str,
    limit: usize,
) -> io::Result<Vec<Node<'a, 'input>>> {
    if parent.attributes().len() != 0 {
        return Err(invalid("Saved Chats XML contains unexpected attributes."));
    }
    let mut entries = Vec::new();
    for node in parent.children().filter(|&node| !ignored(node)) {
        if !is_element(node, name) || entries.len() >= limit {
            return Err(invalid(
                "Saved Chats XML contains unexpected or too many entries.",
            ));
        }
        entries.push(node);
    }
    Ok(entries)
}

pub(super) fn fields<'a, 'input>(
    parent: Node<'a, 'input>,
    allowed: &[&str],
) -> io::Result<Fields<'a, 'input>> {
    if parent.attributes().len() != 0 {
        return Err(invalid("Saved Chats XML contains unexpected attributes."));
    }
    let mut fields = BTreeMap::new();
    for node in parent.children().filter(|&node| !ignored(node)) {
        let tag = node.tag_name();
        if !node.is_element()
            || tag.namespace().is_some()
            || !allowed.contains(&tag.name())
            || fields.insert(tag.name(), node).is_some()
        {
            return Err(invalid(
                "Saved Chats XML contains an unknown or duplicate field.",
            ));
        }
    }
    Ok(fields)
}

pub(super) fn value(fields: &Fields<'_, '_>, name: &str) -> io::Result<Zeroizing<String>> {
    let mut value = Zeroizing::new(String::new());
    if let Some(&node) = fields.get(name) {
        if node.attributes().len() != 0 {
            return Err(invalid("Saved Chats XML contains unexpected attributes."));
        }
        for child in node.children() {
            if child.is_text() {
                value.push_str(child.text().unwrap_or_default());
            } else if !child.is_comment() {
                return Err(invalid("Saved Chats XML contains a non-text field."));
            }
        }
    }
    Ok(value)
}

#[derive(Default)]
struct ParsedMessage {
    user: bool,
    content: String,
    reasoning: Option<String>,
    sources: Vec<Source>,
    agent_steps: Vec<Step>,
}

fn parse_chat(node: Node<'_, '_>, redactor: &Redactor) -> io::Result<Option<Imported>> {
    let chat = fields(node, CHAT_FIELDS)?;
    let (agent_run_id, agent_refs) = match agent::read_origin(&chat, redactor)? {
        agent::Origin::Ordinary => (None, None),
        agent::Origin::Agent(id, references) => (Some(id), references),
        agent::Origin::Unsupported => return Ok(None),
    };
    let messages = chat
        .get("messages")
        .ok_or_else(|| invalid("Saved chat has no messages."))?;
    let messages = children(*messages, "message", MAX_IMPORTED_TURNS * 2)?;
    let mut parsed = Vec::with_capacity(messages.len());
    for message in messages {
        let fields = fields(message, MESSAGE_FIELDS)?;
        let Some(sources) = parse_web_sources(fields.get("webToolCalls").copied(), redactor)?
        else {
            return Ok(None);
        };
        let _created = timestamp(&fields, "createdAt")?;
        let _profile = value(&fields, "aiProfileId")?;
        let _profile_name = value(&fields, "aiProfileName")?;
        let role = value(&fields, "role")?;
        let user = match role.trim() {
            "USER" => true,
            "ASSISTANT" => false,
            _ => return Err(invalid("Saved chat has an unsupported message role.")),
        };
        let limit = if user {
            MAX_QUESTION_BYTES
        } else {
            MAX_ANSWER_BYTES
        };
        let content = masked_text(redactor, &value(&fields, "content")?, limit)?;
        if content.trim().is_empty() {
            return Err(invalid("Saved chat contains an empty message."));
        }
        let reasoning = masked_text(redactor, &value(&fields, "reasoning")?, MAX_ANSWER_BYTES)?;
        let reasoning = (!reasoning.trim().is_empty()).then_some(reasoning);
        let agent_steps = fields
            .get("agentSteps")
            .map(|node| agent::read_steps(*node, redactor))
            .transpose()?
            .unwrap_or_default();
        if user && (reasoning.is_some() || !sources.is_empty() || !agent_steps.is_empty()) {
            return Err(invalid(
                "A user message cannot contain assistant reasoning, web sources, or Agent steps.",
            ));
        }
        parsed.push(ParsedMessage {
            user,
            content,
            reasoning,
            sources,
            agent_steps,
        });
    }
    if parsed.is_empty() {
        return Err(invalid("Saved chat has no messages."));
    }

    let attachment = parse_attachment(&chat, redactor)?;
    let context = masked_text(redactor, &value(&chat, "selectedText")?, MAX_CONTEXT_BYTES)?;
    let context = match &attachment {
        Some(attachment) => attachment.append_to(&context),
        None => context,
    };
    if context.len() > MAX_CONTEXT_BYTES {
        return Err(invalid(
            "The selected text and attachment exceed the saved chat limit.",
        ));
    }
    let context = (!context.trim().is_empty()).then_some(context.as_str());
    let mut turns = Vec::new();
    let mut messages = parsed.into_iter().peekable();
    while let Some(message) = messages.next() {
        let (question, assistant) = if message.user {
            (
                message.content,
                messages
                    .next_if(|message| !message.user)
                    .unwrap_or_default(),
            )
        } else {
            (String::new(), message)
        };
        let first = turns.is_empty();
        let turn = if !assistant.agent_steps.is_empty() || (first && agent_run_id.is_some()) {
            if !first
                || context.is_some()
                || question.trim().is_empty()
                || assistant.content.trim().is_empty()
            {
                return Err(invalid(
                    "Saved Agent steps require an initial user and assistant pair without additional context.",
                ));
            }
            Turn::from_imported_agent(
                &question,
                &assistant.content,
                &assistant.agent_steps,
                redactor,
            )
            .map_err(io::Error::other)?
        } else {
            Turn::from_parts(context.filter(|_| first), question, assistant.content)
        };
        turns.push(
            turn.with_reasoning(assistant.reasoning)
                .with_web_sources(assistant.sources),
        );
        if turns.len() > MAX_IMPORTED_TURNS {
            return Err(invalid("Saved chat contains too many messages."));
        }
    }

    let title = masked_metadata(redactor, &value(&chat, "title")?, MAX_TITLE_CHARS)?;
    if !store::valid_title(&title) {
        return Err(invalid("Saved chat has an invalid title."));
    }
    let updated = timestamp(&chat, "updatedAt")?;
    let created = timestamp(&chat, "createdAt")?
        .or(updated)
        .unwrap_or_else(|| jiff::Timestamp::now().as_millisecond());
    let updated = updated.unwrap_or(created);
    if updated < created {
        return Err(invalid("Saved chat has an invalid update timestamp."));
    }
    Ok(Some(Imported {
        title,
        created,
        updated,
        profile_name: masked_metadata(redactor, &value(&chat, "activeAiProfileName")?, 256)?,
        connection: masked_metadata(redactor, &value(&chat, "connectionDisplayName")?, 120)?,
        turns,
        attachment,
        java: JavaOrigin {
            id: chat_id(redactor, value(&chat, "id")?.trim())?,
            profile_id: safe_hint(
                redactor,
                value(&chat, "activeAiProfileId")?.trim(),
                store::valid_java_profile_id,
            )?,
            response_language: safe_hint(
                redactor,
                value(&chat, "responseLanguageCode")?.trim(),
                store::valid_response_language,
            )?,
            agent_run_id,
            agent_refs,
        },
    }))
}

fn parse_web_sources(
    node: Option<Node<'_, '_>>,
    redactor: &Redactor,
) -> io::Result<Option<Vec<Source>>> {
    let mut sources = Vec::new();
    let Some(node) = node else {
        return Ok(Some(sources));
    };
    for call in children(node, "webToolCall", MAX_STORED_SOURCES)? {
        let call = fields(
            call,
            &[
                "kind",
                "tool",
                "input",
                "success",
                "message",
                "contentChars",
                "truncated",
                "sources",
            ],
        )?;
        if value(&call, "kind")?.trim() != "SEARCH"
            || value(&call, "tool")?.trim() != "web_search"
            || value(&call, "success")?.trim() != "true"
        {
            return Ok(None);
        }
        for field in ["input", "message", "contentChars", "truncated"] {
            value(&call, field)?;
        }
        let Some(&items) = call.get("sources") else {
            continue;
        };
        for item in children(items, "source", MAX_STORED_SOURCES)? {
            let item = fields(item, &["title", "url"])?;
            let title = value(&item, "title")?;
            let url = value(&item, "url")?;
            if let Some(source) = Source::checked(&title, &url, redactor)
                && !sources
                    .iter()
                    .any(|existing: &Source| existing.url() == source.url())
            {
                if sources.len() == MAX_STORED_SOURCES {
                    return Err(invalid("Saved chat has too many web sources."));
                }
                sources.push(source);
            }
        }
    }
    Ok(Some(sources))
}

fn parse_attachment(chat: &Fields<'_, '_>, redactor: &Redactor) -> io::Result<Option<Attachment>> {
    let name = value(chat, "attachmentFileName")?;
    let source = value(chat, "attachmentSourcePath")?;
    let content = value(chat, "attachmentContent")?;
    if [&name, &source, &content]
        .into_iter()
        .all(|field| field.trim().is_empty())
    {
        return Ok(None);
    }
    Attachment::masked(AttachmentKind::Imported, &name, &source, &content, redactor)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(super) fn timestamp(fields: &Fields<'_, '_>, name: &str) -> io::Result<Option<i64>> {
    let value = value(fields, name)?;
    if value.trim().is_empty() {
        return Ok(None);
    }
    let value = value
        .trim()
        .parse::<i64>()
        .ok()
        .filter(|&value| value >= 0 && jiff::Timestamp::from_millisecond(value).is_ok())
        .ok_or_else(|| invalid("Saved chat has an invalid timestamp."))?;
    Ok((value != 0).then_some(value))
}

pub(super) fn masked_text(redactor: &Redactor, value: &str, limit: usize) -> io::Result<String> {
    if value.len() > limit {
        return Err(invalid("Saved chat text exceeds its size limit."));
    }
    let text = mask::prompt(redactor, value).map_err(io::Error::other)?;
    if text.len() > limit {
        return Err(invalid("Masked saved chat text exceeds its size limit."));
    }
    Ok(text)
}

pub(super) fn masked_metadata(redactor: &Redactor, text: &str, limit: usize) -> io::Result<String> {
    let text = masked_text(redactor, text.trim(), 4 * limit)?;
    let text = text.replace(['\n', '\t'], " ").trim().to_owned();
    if text.chars().count() > limit {
        return Err(invalid("Saved chat metadata exceeds its size limit."));
    }
    Ok(text)
}

pub(super) fn safe_hint(
    redactor: &Redactor,
    value: &str,
    valid: impl FnOnce(&str) -> bool,
) -> io::Result<Option<String>> {
    if !valid(value) || mask::prompt(redactor, value).map_err(io::Error::other)? != value {
        return Ok(None);
    }
    Ok(Some(value.to_owned()))
}

pub(super) fn chat_id(redactor: &Redactor, id: &str) -> io::Result<String> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        return Err(invalid("Saved chat has an invalid identifier."));
    }
    let compact = store::java_id(id).or_else(|| {
        (id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| id.to_ascii_lowercase())
    });
    if let Some(compact) = compact {
        let canonical = uuid(&compact);
        if identifier_is_safe(redactor, id)? && identifier_is_safe(redactor, &canonical)? {
            return Ok(canonical);
        }
    }
    // Legacy identifiers need stable deduplication without retaining their original text.
    let input = Zeroizing::new(format!("kortty Java Saved Chat\0{id}"));
    let hash = HashAlgorithm::Sha256
        .digest(input.as_bytes())
        .map_err(|_| io::Error::other("Could not normalize saved chat identifier."))?;
    let mut compact = String::with_capacity(32);
    for byte in &hash[..16] {
        write!(compact, "{byte:02x}").expect("writing to a String");
    }
    let canonical = uuid(&compact);
    if !identifier_is_safe(redactor, &canonical)? {
        return Err(invalid("Saved chat has an unsafe identifier."));
    }
    Ok(canonical)
}

fn identifier_is_safe(redactor: &Redactor, id: &str) -> io::Result<bool> {
    Ok(mask::prompt(redactor, id).map_err(io::Error::other)? == id)
}

fn uuid(compact: &str) -> String {
    format!(
        "{}-{}-{}-{}-{}",
        &compact[..8],
        &compact[8..12],
        &compact[12..16],
        &compact[16..20],
        &compact[20..]
    )
}

struct BoundedXml<W> {
    file: W,
    written: usize,
}

impl<W: Write> Write for BoundedXml<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_XML_BYTES - self.written {
            return Err(invalid("Saved Chats XML exceeds the file size limit."));
        }
        let length = self.file.write(bytes)?;
        self.written += length;
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn write_chat(xml: &mut impl Write, record: &store::Record, redactor: &Redactor) -> io::Result<()> {
    let summary = &record.summary;
    let origin = record.java.as_ref();
    let id = origin.map_or(summary.key.id.as_str(), |origin| origin.id.as_str());
    let title = masked_metadata(redactor, &summary.title, MAX_TITLE_CHARS)?;
    if !store::valid_title(&title) {
        return Err(invalid("Saved chat has an invalid title."));
    }
    xml.write_all(b"    <chat>\n")?;
    element(xml, "        ", "id", &chat_id(redactor, id)?)?;
    element(xml, "        ", "title", &title)?;
    element(xml, "        ", "createdAt", &summary.created.to_string())?;
    element(xml, "        ", "updatedAt", &summary.updated.to_string())?;
    if !record.is_agent_origin()
        && let Some(first) = record.turns.first()
        && first.agent_steps().is_empty()
        && let Some(context) = first.context()
    {
        let context = match &record.attachment {
            Some(attachment) => attachment
                .without_attachment(context)
                .ok_or_else(|| invalid("The saved attachment and conversation text differ."))?,
            None => context,
        };
        element(
            xml,
            "        ",
            "selectedText",
            &masked_text(redactor, context, MAX_CONTEXT_BYTES)?,
        )?;
    }
    if let Some(attachment) = &record.attachment {
        let attachment = attachment.remasked(redactor).map_err(io::Error::other)?;
        element(xml, "        ", "attachmentFileName", &attachment.name)?;
        element(xml, "        ", "attachmentSourcePath", &attachment.source)?;
        element(xml, "        ", "attachmentContent", &attachment.content)?;
    }
    element(
        xml,
        "        ",
        "connectionDisplayName",
        &masked_metadata(redactor, &summary.connection, 120)?,
    )?;
    if let Some(language) = safe_hint(
        redactor,
        record.response_language().unwrap_or(""),
        store::valid_response_language,
    )? {
        element(xml, "        ", "responseLanguageCode", &language)?;
    }
    let portable_profile = match &summary.profile {
        ProfileRef::Policy(id) => Some(id.as_str()),
        ProfileRef::Numeric(0) => origin.and_then(|origin| origin.profile_id.as_deref()),
        ProfileRef::Numeric(_) => None,
    };
    if let Some(id) = portable_profile
        && let Some(profile) = safe_hint(redactor, id, store::valid_java_profile_id)?
    {
        element(xml, "        ", "activeAiProfileId", &profile)?;
    }
    element(
        xml,
        "        ",
        "activeAiProfileName",
        &masked_metadata(redactor, &summary.profile_name, 256)?,
    )?;
    agent::write_origin(xml, record, redactor)?;
    xml.write_all(b"        <messages>\n")?;
    // Use the recorded chat creation time for each exported message.
    let time = summary.created.to_string();
    for turn in &record.turns {
        write_message(xml, redactor, turn, &time, false)?;
        write_message(xml, redactor, turn, &time, true)?;
    }
    xml.write_all(b"        </messages>\n    </chat>\n")
}

fn write_message(
    xml: &mut impl Write,
    redactor: &Redactor,
    turn: &Turn,
    timestamp: &str,
    assistant: bool,
) -> io::Result<()> {
    let (role, text, limit) = if assistant {
        ("ASSISTANT", turn.answer(), MAX_ANSWER_BYTES)
    } else {
        ("USER", turn.question(), MAX_QUESTION_BYTES)
    };
    if text.is_empty() {
        return Ok(());
    }
    let text = masked_text(redactor, text, limit)?;
    if text.trim().is_empty() {
        return Err(invalid(
            "Saved chat contains an empty message after masking.",
        ));
    }
    xml.write_all(b"            <message>\n")?;
    element(xml, "                ", "role", role)?;
    element(xml, "                ", "content", &text)?;
    element(xml, "                ", "createdAt", timestamp)?;
    if assistant {
        if let Some(reasoning) = turn.reasoning() {
            let reasoning = masked_text(redactor, reasoning, MAX_ANSWER_BYTES)?;
            if !reasoning.trim().is_empty() {
                element(xml, "                ", "reasoning", &reasoning)?;
            }
        }
        write_web_sources(xml, turn.web_sources(), redactor)?;
        agent::write_steps(xml, turn.agent_steps(), redactor)?;
    }
    xml.write_all(b"            </message>\n")
}

fn write_web_sources(
    xml: &mut impl Write,
    sources: &[Source],
    redactor: &Redactor,
) -> io::Result<()> {
    let sources: Vec<_> = sources
        .iter()
        .filter_map(|source| source.remasked(redactor))
        .collect();
    if sources.is_empty() {
        return Ok(());
    }
    xml.write_all(b"                <webToolCalls>\n                    <webToolCall>\n")?;
    element(xml, "                        ", "kind", "SEARCH")?;
    element(xml, "                        ", "tool", "web_search")?;
    element(xml, "                        ", "success", "true")?;
    xml.write_all(b"                        <sources>\n")?;
    for source in sources {
        xml.write_all(b"                            <source>\n")?;
        element(
            xml,
            "                                ",
            "title",
            source.title(),
        )?;
        element(xml, "                                ", "url", source.url())?;
        xml.write_all(b"                            </source>\n")?;
    }
    xml.write_all(b"                        </sources>\n                    </webToolCall>\n                </webToolCalls>\n")
}

fn element(xml: &mut impl Write, indent: &str, name: &str, value: &str) -> io::Result<()> {
    write!(xml, "{indent}<{name}>")?;
    let mut from = 0;
    for (index, byte) in value.bytes().enumerate() {
        let escaped = match byte {
            b'&' => b"&amp;".as_slice(),
            b'<' => b"&lt;",
            b'>' => b"&gt;",
            _ => continue,
        };
        xml.write_all(&value.as_bytes()[from..index])?;
        xml.write_all(escaped)?;
        from = index + 1;
    }
    xml.write_all(&value.as_bytes()[from..])?;
    writeln!(xml, "</{name}>")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    fn redactor(secret: Option<&str>) -> Redactor {
        let secrets = Secrets::default();
        if let Some(secret) = secret {
            secrets.add(secret);
        }
        Redactor::for_journal(secrets.seal().unwrap(), Policy::default())
    }

    #[test]
    fn java_chat_xml_reads_the_pinned_jaxb_fixture() -> io::Result<()> {
        let fixture = include_bytes!("fixtures/java-2.26.0.xml");
        let redactor = redactor(None);
        let parsed = parse(fixture, &redactor)?;
        assert_eq!(
            (parsed.unsupported, parsed.invalid, parsed.chats.len()),
            (0, 0, 3)
        );
        let [first, unanswered, consecutive] = parsed.chats.as_slice() else {
            panic!("expected the three pinned JAXB chats");
        };
        assert_eq!(first.title, "Grüße 東京 & <review> 🧪");
        assert_eq!(first.java.profile_id.as_deref(), Some("profile-17"));
        assert_eq!(first.java.response_language.as_deref(), Some("de-DE"));
        assert_eq!(first.connection, "Fixture SSH & local");
        assert_eq!(
            first.turns[0].context(),
            Some("echo \"a & b < c > d\"\nline two: café")
        );
        assert_eq!(first.turns[0].question(), "");
        assert_eq!(first.turns[0].answer(), "État: <ok> & ready.");
        assert_eq!(first.turns[1].question(), "Was bedeutet <ok>?");
        assert_eq!(first.turns[1].answer(), "Alles bereit.");
        assert_eq!(unanswered.turns[0].context(), None);
        assert_eq!(unanswered.turns[0].question(), "Still waiting…");
        assert!(unanswered.turns[0].answer().is_empty());
        assert_eq!(
            consecutive
                .turns
                .iter()
                .map(|turn| (turn.question(), turn.answer()))
                .collect::<Vec<_>>(),
            [
                ("First question", ""),
                ("Second question", "一つ目の応答"),
                ("", "二つ目の応答"),
            ]
        );

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let input = root.join("pinned-java.xml");
        std::fs::write(&input, fixture)?;
        assert_eq!(import(&root, &input, &redactor)?.saved.len(), 3);
        let output = std::env::var_os("KORTTY_SAVE_JAVA_CHAT_XML_FIXTURE")
            .map_or_else(|| root.join("rust-export.xml"), std::path::PathBuf::from);
        let exported = export(&root, &output, &redactor)?;
        assert_eq!(exported.count, 3);
        assert!(!exported.cleanup_failed);
        let roundtrip = parse(&std::fs::read(&output)?, &redactor)?;
        assert_eq!(
            (
                roundtrip.unsupported,
                roundtrip.invalid,
                roundtrip.chats.len()
            ),
            (0, 0, 3)
        );
        for (expected, actual) in parsed.chats.iter().zip(&roundtrip.chats) {
            assert_eq!(expected.title, actual.title);
            assert_eq!(
                (expected.created, expected.updated),
                (actual.created, actual.updated)
            );
            assert!(expected.java == actual.java);
            assert!(expected.turns == actual.turns);
        }
        Ok(())
    }

    #[test]
    fn pinned_java_agent_chat_interchange_keeps_steps_and_inert_source_refs() -> io::Result<()> {
        use crate::workspace::terminal_ai::agent_follow_up::{StepKind, StepStatus};

        let fixture = include_bytes!("fixtures/java-agent-run.xml");
        let redactor = redactor(Some("local-hidden-value"));
        let parsed = parse(fixture, &redactor)?;
        assert_eq!(
            (parsed.unsupported, parsed.invalid, parsed.chats.len()),
            (0, 0, 1)
        );
        let expected = &parsed.chats[0];
        assert_eq!(
            expected.java.agent_run_id.as_deref(),
            Some("03684aee-2b4c-4bcf-be6d-cfe70e0d025d")
        );
        assert_eq!(
            expected
                .java
                .agent_refs
                .as_ref()
                .unwrap()
                .journal_entry_id
                .as_deref(),
            Some("agent-fixture")
        );
        assert_eq!(expected.turns.len(), 2);
        assert_eq!(expected.turns[0].question(), "Inspect the build");
        assert_eq!(expected.turns[0].answer(), "État: <failed> & reported.");
        let steps = expected.turns[0].agent_steps();
        assert_eq!(steps.len(), 2);
        assert!(steps[0].kind == StepKind::Message);
        assert!(steps[1].kind == StepKind::Command && steps[1].status == Some(StepStatus::Failed));
        assert_eq!(steps[1].exit_code, Some(-1));
        assert_eq!(steps[1].elapsed_seconds, Some(2));
        assert_eq!(steps[1].truncated, Some(true));
        assert_eq!(
            steps[1].command.as_deref(),
            Some("printf '%s' '*** & 東京'")
        );
        assert_eq!(steps[1].output, ["Grüße & <output>", "***"]);
        assert!(expected.turns[1].agent_steps().is_empty());

        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let input = root.join("java-agent.xml");
        std::fs::write(&input, fixture)?;
        let imported = import(&root, &input, &redactor)?;
        assert_eq!(imported.saved.len(), 1);
        let record = store::load(&root, &imported.saved[0].key)?;
        assert!(record.is_agent_origin());
        assert_eq!(record.agent_run_id(), None);
        assert!(record.agent_journal().is_none());
        let output = std::env::var_os("KORTTY_SAVE_JAVA_AGENT_CHAT_XML_FIXTURE").map_or_else(
            || root.join("rust-agent-export.xml"),
            std::path::PathBuf::from,
        );
        assert_eq!(export(&root, &output, &redactor)?.count, 1);
        let bytes = std::fs::read(output)?;
        assert!(!String::from_utf8_lossy(&bytes).contains("local-hidden-value"));
        let round_trip = parse(&bytes, &redactor)?;
        assert_eq!(
            (
                round_trip.unsupported,
                round_trip.invalid,
                round_trip.chats.len()
            ),
            (0, 0, 1)
        );
        assert!(round_trip.chats[0].java == expected.java);
        assert!(round_trip.chats[0].turns == expected.turns);
        Ok(())
    }

    #[test]
    fn native_agent_chat_exports_link_and_imports_without_local_journal_authority() -> io::Result<()>
    {
        use crate::workspace::terminal_ai::agent_follow_up::{StepKind, StepStatus};

        let run_id = "03684aee2b4c4bcfbe6dcfe70e0d025d";
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let redactor = redactor(None);
        let turn = Turn::from_imported_agent(
            "Check the fixture",
            "Command failed.",
            &[Step {
                kind: StepKind::Command,
                title: "Fixture".into(),
                detail: "Read-only command".into(),
                command: Some("false".into()),
                output: vec!["Fixture output".into()],
                exit_code: Some(i64::from(u32::MAX)),
                status: Some(StepStatus::Failed),
                elapsed_seconds: Some(0),
                truncated: None,
            }],
            &redactor,
        )
        .map_err(io::Error::other)?;
        let record = store::open_or_seed_agent_linked(
            &root,
            run_id,
            "Native Agent chat".into(),
            store::Draft {
                profile: ProfileRef::Policy("policy-Fixture".into()),
                profile_name: "Fixture model".into(),
                connection: "Fixture terminal".into(),
                response_language: None,
                turns: vec![turn],
                attachment: None,
            },
            Some(store::AgentJournalRef {
                journal_id: "journal-fixture".into(),
                entry_id: "entry-fixture".into(),
            }),
            || Ok(()),
        )?;
        assert_eq!(record.agent_run_id(), Some(run_id));
        assert!(record.agent_journal().is_some());

        let output = root.join("native-agent-export.xml");
        assert_eq!(export(&root, &output, &redactor)?.count, 1);
        let bytes = std::fs::read(&output)?;
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("<origin>AGENT_RUN</origin>"));
        assert!(text.contains("<exitCode>-1</exitCode>"));
        assert!(text.contains("<activeAiProfileId>policy-Fixture</activeAiProfileId>"));
        assert!(!text.contains("<selectedText>"));
        let copy = crate::snippets::tests::Profile::new();
        let imported = import(&copy.0, &output, &redactor)?;
        let archive = store::load(&copy.0, &imported.saved[0].key)?;
        assert!(archive.is_agent_origin());
        assert_eq!(archive.agent_run_id(), None);
        assert!(archive.agent_journal().is_none());
        assert_eq!(archive.portable_agent_run_id(), Some(run_id));
        assert_eq!(archive.summary.profile, ProfileRef::Numeric(0));
        let origin = archive.java.unwrap();
        assert_eq!(origin.profile_id.as_deref(), Some("policy-Fixture"));
        let refs = origin.agent_refs.unwrap();
        assert_eq!(refs.journal_id.as_deref(), Some("journal-fixture"));
        assert_eq!(refs.journal_entry_id.as_deref(), Some("entry-fixture"));
        Ok(())
    }

    #[test]
    fn java_agent_chat_refuses_ambiguous_context_invalid_type_and_late_steps() -> io::Result<()> {
        let fixture = include_str!("fixtures/java-agent-run.xml");
        let redactor = redactor(None);
        for malformed in [
            fixture.replace("<origin>AGENT_RUN</origin>", "<origin>AGENT_RUN</origin><selectedText>extra context</selectedText>"),
            fixture.replace("<type>ACTION</type>", "<type>THINKING</type>"),
            fixture.replace("<content>The fixture command.</content>", "<content>The fixture command.</content><agentSteps><step><type>MESSAGE</type><title>Late step</title></step></agentSteps>"),
            fixture.replace("<sourceJournalId>journal-fixture</sourceJournalId>", "<sourceJournalId>../untrusted</sourceJournalId>"),
        ] {
            let parsed = parse(malformed.as_bytes(), &redactor)?;
            assert_eq!((parsed.unsupported, parsed.invalid, parsed.chats.len()), (0, 1, 0));
        }
        Ok(())
    }

    #[test]
    fn java_chat_xml_roundtrip_retains_only_masked_web_provenance() -> io::Result<()> {
        let redactor = redactor(Some("private-fixture-secret"));
        let input = r#"<aiChats><chat><id>research</id><title>Research</title><messages>
            <message><role>USER</role><content>Explain the error.</content></message>
            <message><role>ASSISTANT</role><content>See the docs.</content>
                <webToolCalls><webToolCall>
                    <kind>SEARCH</kind><tool>web_search</tool><success>true</success>
                    <input>discarded original query</input><sources>
                        <source><title>Docs private-fixture-secret</title><url>https://example.com/guide?key=hidden</url></source>
                        <source><title>Private</title><url>https://name:password@example.com/</url></source>
                    </sources>
                </webToolCall></webToolCalls>
            </message>
        </messages></chat></aiChats>"#;
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let original = root.join("java-research.xml");
        std::fs::write(&original, input)?;
        assert_eq!(import(&root, &original, &redactor)?.saved.len(), 1);
        let output = root.join("rust-research.xml");
        assert_eq!(export(&root, &output, &redactor)?.count, 1);
        let xml = std::fs::read_to_string(&output)?;
        assert!(!xml.contains("discarded original query"));
        assert!(!xml.contains("private-fixture-secret"));
        assert!(!xml.contains("?key") && !xml.contains("name:password"));
        let result = parse(xml.as_bytes(), &redactor)?;
        assert_eq!(result.chats.len(), 1);
        let sources = result.chats[0].turns[0].web_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].title(), "Docs ***");
        assert_eq!(sources[0].url(), "https://example.com/guide");
        Ok(())
    }

    #[test]
    fn java_chat_xml_distinguishes_unsupported_invalid_and_plain_records() -> io::Result<()> {
        let redactor = redactor(Some("local-hidden-value"));
        let input = r#"<aiChats>
            <chat><id>agent</id><origin>AGENT_RUN</origin></chat>
            <chat><id>attachment</id><attachmentContent>hidden</attachmentContent></chat>
            <chat><id>step</id><messages><message><agentSteps><step/></agentSteps></message></messages></chat>
            <chat><id>tool</id><messages><message><webToolCalls><webToolCall/></webToolCalls></message></messages></chat>
            <chat><id>invalid</id><title>first</title><title>second</title></chat>
            <chat><id>chat-legacy</id><title>Build local-hidden-value</title><updatedAt>1700000000000</updatedAt>
                <selectedText>DB_PASSWORD=unsafe</selectedText><connectionDisplayName>local-hidden-value</connectionDisplayName>
                <activeAiProfileId>profile-1</activeAiProfileId><activeAiProfileName>Support local-hidden-value</activeAiProfileName>
                <responseLanguageCode>de-DE</responseLanguageCode><messages>
                    <message><role>ASSISTANT</role><content>First &lt;ok&gt; &amp; Grüße</content><reasoning>local-hidden-value</reasoning></message>
                    <message><role>USER</role><content>local-hidden-value</content></message>
                    <message><role>USER</role><content>Question</content></message>
                    <message><role>ASSISTANT</role><content><![CDATA[Second > result]]></content></message>
                    <message><role>ASSISTANT</role><content>Last answer</content></message>
                </messages>
            </chat></aiChats>"#;
        let parsed = parse(input.as_bytes(), &redactor)?;
        assert_eq!(
            (parsed.unsupported, parsed.invalid, parsed.chats.len()),
            (1, 4, 1)
        );
        let chat = &parsed.chats[0];
        assert_eq!(chat.title, "Build ***");
        assert_eq!(chat.profile_name, "Support ***");
        assert_eq!(chat.connection, "***");
        assert_eq!(
            (chat.created, chat.updated),
            (1_700_000_000_000, 1_700_000_000_000)
        );
        assert_eq!(chat.java.profile_id.as_deref(), Some("profile-1"));
        assert_eq!(chat.java.response_language.as_deref(), Some("de-DE"));
        assert_eq!(chat.java.id, chat_id(&redactor, "chat-legacy")?);
        assert!(store::java_id(&chat.java.id).is_some());
        assert_eq!(chat.turns[0].context(), Some("DB_PASSWORD=***"));
        assert_eq!(chat.turns[0].reasoning(), Some("***"));
        assert_eq!(
            chat.turns
                .iter()
                .map(|turn| (turn.question(), turn.answer()))
                .collect::<Vec<_>>(),
            [
                ("", "First <ok> & Grüße"),
                ("***", ""),
                ("Question", "Second > result"),
                ("", "Last answer"),
            ]
        );
        assert!(
            parse(
                b"<!DOCTYPE aiChats [<!ENTITY x SYSTEM 'file:///private'>]><aiChats/>",
                &redactor
            )
            .is_err()
        );
        assert!(parse(b"<aiChats xmlns='foreign'/>", &redactor).is_err());
        assert!(
            parse(
                b"<aiChats><chat title='unexpected'/><foreign/></aiChats>",
                &redactor
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn java_chat_xml_round_trip_masks_export_and_preserves_existing_files() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let input = root.join("java-input.xml");
        let output = root.join("java-export.xml");
        let copy = root.join("copy");
        std::fs::write(
            &input,
            r#"<aiChats><chat>
            <id>12345678-1234-5678-90ab-1234567890ab</id>
            <title>local-hidden-value</title><createdAt>1700000000000</createdAt><updatedAt>1700000000000</updatedAt>
            <messages><message><role>USER</role><content>local-hidden-value &amp; &lt;tag&gt;</content></message>
                <message><role>ASSISTANT</role><content>Reply</content><reasoning>local-hidden-value &amp; details</reasoning></message></messages>
        </chat></aiChats>"#,
        )?;
        let imported = import(&root, &input, &redactor(None))?;
        assert_eq!(imported.saved.len(), 1);
        assert_eq!(import(&root, &input, &redactor(None))?.duplicates, 1);
        let protected = redactor(Some("local-hidden-value"));
        assert_eq!(export(&root, &output, &protected)?.count, 1);
        let bytes = std::fs::read(&output)?;
        let xml = std::str::from_utf8(&bytes).map_err(io::Error::other)?;
        assert!(xml.contains("<content>*** &amp; &lt;tag&gt;</content>"));
        assert!(xml.contains("<reasoning>*** &amp; details</reasoning>"));
        assert!(!xml.contains("local-hidden-value"));
        assert!(
            matches!(export(&root, &output, &protected), Err(error) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(&output)?, bytes);
        let imported = import(&copy, &output, &protected)?;
        assert_eq!(imported.saved.len(), 1);
        let record = store::load(&copy, &imported.saved[0].key)?;
        assert_eq!(record.summary.title, "***");
        assert_eq!(record.turns[0].context(), None);
        assert_eq!(record.turns[0].question(), "*** & <tag>");
        assert_eq!(record.turns[0].answer(), "Reply");
        assert_eq!(record.turns[0].reasoning(), Some("*** & details"));
        assert!(selected_file(Path::new("relative.xml")).is_err());
        assert!(
            export(
                &root,
                &root.join("ai-chats/00000000000000000000000000000000.json"),
                &protected
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn java_chat_attachment_roundtrip_keeps_selected_text_separate_and_masks_all_fields()
    -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let input = root.join("attached.xml");
        let output = root.join("export.xml");
        let xml = r#"<aiChats><chat>
            <id>8d2772db-5a09-4530-941d-aa7cbc78295e</id><title>Attached</title>
            <selectedText>Terminal &amp; evidence</selectedText>
            <attachmentFileName>local-hidden-value &amp; Grüße.txt</attachmentFileName>
            <attachmentSourcePath>/private/local-hidden-value/Grüße.txt</attachmentSourcePath>
            <attachmentContent>Evidence &lt;ok&gt;
API_KEY=unmasked-key
</attachmentContent>
            <messages><message><role>USER</role><content>Explain</content></message>
            <message><role>ASSISTANT</role><content>Reply</content></message></messages>
        </chat></aiChats>"#;
        std::fs::write(&input, xml)?;
        let original = import(&root, &input, &redactor(None))?;
        assert_eq!(original.saved.len(), 1);
        let protected = redactor(Some("local-hidden-value"));
        assert_eq!(export(&root, &output, &protected)?.count, 1);
        let xml = std::fs::read_to_string(&output)?;
        assert!(xml.contains("<selectedText>Terminal &amp; evidence</selectedText>"));
        assert!(xml.contains("<attachmentFileName>*** &amp; Grüße.txt</attachmentFileName>"));
        assert!(
            xml.contains("<attachmentSourcePath>/private/***/Grüße.txt</attachmentSourcePath>")
        );
        assert!(
            xml.contains(
                "<attachmentContent>Evidence &lt;ok&gt;\nAPI_KEY=***\n</attachmentContent>"
            )
        );
        assert!(!xml.contains("local-hidden-value") && !xml.contains("unmasked-key"));

        let copy = root.join("copy");
        let imported = import(&copy, &output, &protected)?;
        assert_eq!(imported.saved.len(), 1);
        let record = store::load(&copy, &imported.saved[0].key)?;
        let attachment = record.attachment.as_ref().unwrap();
        assert_eq!(attachment.name, "*** & Grüße.txt");
        assert_eq!(attachment.content, "Evidence <ok>\nAPI_KEY=***\n");
        let context = record.turns[0].context().unwrap();
        assert_eq!(
            attachment.without_attachment(context),
            Some("Terminal & evidence")
        );
        assert_eq!(context.matches("Evidence <ok>").count(), 1);
        Ok(())
    }

    #[test]
    #[ignore = "requires the pinned Java chat manager and explicit XML fixture paths"]
    fn pinned_java_chat_attachment_interchange() -> io::Result<()> {
        let fixture = |name| {
            std::env::var_os(name)
                .map(std::path::PathBuf::from)
                .expect("provide an explicit Java attachment fixture path")
        };
        let input = fixture("KORTTY_JAVA_ATTACHMENT_INPUT");
        let output = fixture("KORTTY_JAVA_ATTACHMENT_OUTPUT");
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let protected = redactor(Some("local-hidden-value"));
        let imported = import(&root, &input, &protected)?;
        assert_eq!(imported.saved.len(), 1);
        let record = store::load(&root, &imported.saved[0].key)?;
        let attachment = record.attachment.as_ref().unwrap();
        assert_eq!(attachment.name, "*** & Grüße.txt");
        assert_eq!(attachment.source, "/private/***/Grüße.txt");
        assert_eq!(attachment.content, "Evidence <ok>\nAPI_KEY=***\n");
        assert_eq!(
            attachment.without_attachment(record.turns[0].context().unwrap()),
            Some("Terminal & evidence")
        );
        assert_eq!(export(&root, &output, &protected)?.count, 1);
        Ok(())
    }

    #[test]
    fn java_chat_xml_private_stage_preserves_a_destination_created_during_export() -> io::Result<()>
    {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        let output = root.join("java-export.xml");
        let (directory, name) = selected_file(&output)?;
        let result = publish_xml(&directory, name, |file| {
            file.write_all(b"new data")?;
            std::fs::write(&output, b"existing data")
        });
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::AlreadyExists));
        assert_eq!(std::fs::read(&output)?, b"existing data");
        assert_eq!(directory.names(4)?, [OsStr::new(name).to_os_string()]);
        Ok(())
    }
}
