//! Historical Java Swarm messages, independent of native providers and routes.

use std::collections::BTreeMap;
use std::io;

use roxmltree::Node;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::terminal_logging::Redactor;
use crate::workspace::terminal_ai::mask;

use super::super::{java_xml, store};
use super::{
    FOLDER, MAX_CHATS, MAX_TITLE_CHARS, Stored, Summary, encode, invalid, valid_line, valid_title,
};

const VERSION: u8 = 1;
const MAX_XML_CHATS: usize = 256;
const MAX_MESSAGES: usize = 128;
const MAX_TARGETS: usize = 128;
const MAX_METADATA_CHARS: usize = 120;
const MAX_CONTENT_BYTES: usize = 128 * 1024;
const MAX_SUMMARY_BYTES: usize = 8 * 1024;
const CHAT_FIELDS: &[&str] = &[
    "id",
    "title",
    "createdAt",
    "updatedAt",
    "responseLanguageCode",
    "activeAiProfileId",
    "activeAiProfileName",
    "targetConnectionIds",
    "targetGroupPaths",
    "messages",
];
const MESSAGE_FIELDS: &[&str] = &[
    "role",
    "content",
    "createdAt",
    "aiProfileId",
    "aiProfileName",
    "serverSummaries",
];
const SERVER_FIELDS: &[&str] = &[
    "serverDisplayName",
    "finalState",
    "summaryText",
    "elapsedSeconds",
    "totalTokens",
];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace::saved_chats) struct Archive {
    version: u8,
    pub(in crate::workspace::saved_chats) id: String,
    pub(in crate::workspace::saved_chats) title: String,
    pub(in crate::workspace::saved_chats) created: i64,
    pub(in crate::workspace::saved_chats) updated: i64,
    pub(in crate::workspace::saved_chats) profile_name: String,
    pub(in crate::workspace::saved_chats) response_language: Option<String>,
    pub(in crate::workspace::saved_chats) messages: Vec<Message>,
    #[serde(skip)]
    pub(super) fingerprint: [u8; 32],
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace::saved_chats) struct Message {
    pub(in crate::workspace::saved_chats) role: Role,
    pub(in crate::workspace::saved_chats) content: String,
    pub(in crate::workspace::saved_chats) created: i64,
    pub(in crate::workspace::saved_chats) profile_name: String,
    pub(in crate::workspace::saved_chats) servers: Vec<Server>,
}

#[derive(Clone, Copy, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(in crate::workspace::saved_chats) enum Role {
    User,
    Assistant,
}

impl Role {
    pub(in crate::workspace::saved_chats) fn label(self) -> &'static str {
        match self {
            Self::User => "You",
            Self::Assistant => "Assistant",
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::workspace::saved_chats) struct Server {
    pub(in crate::workspace::saved_chats) display_name: String,
    pub(in crate::workspace::saved_chats) state: String,
    pub(in crate::workspace::saved_chats) summary: String,
    pub(in crate::workspace::saved_chats) elapsed_seconds: u64,
    pub(in crate::workspace::saved_chats) total_tokens: u64,
}

impl Archive {
    pub(super) fn valid(&self) -> bool {
        self.version == VERSION
            && store::valid_id(&self.id)
            && valid_title(&self.title)
            && valid_date(self.created)
            && valid_date(self.updated)
            && self.updated >= self.created
            && metadata_valid(&self.profile_name)
            && self
                .response_language
                .as_deref()
                .is_none_or(store::valid_response_language)
            && self.messages.len() <= MAX_MESSAGES
            && self.messages.iter().all(Message::valid)
    }

    pub(super) fn summary(&self) -> Summary {
        Summary {
            id: self.id.clone(),
            revision: 1,
            profile_name: self.profile_name.clone(),
            title: self.title.clone(),
            is_draft: false,
            is_java_archive: true,
            target_count: self
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .map_or(0, |message| message.servers.len()),
            updated: self.updated,
            fingerprint: self.fingerprint,
        }
    }
}

impl Message {
    fn valid(&self) -> bool {
        super::valid_text(&self.content, MAX_CONTENT_BYTES)
            && valid_date(self.created)
            && metadata_valid(&self.profile_name)
            && self.servers.len() <= MAX_TARGETS
            && (self.role == Role::Assistant || self.servers.is_empty())
            && self.servers.iter().all(Server::valid)
    }
}

impl Server {
    fn valid(&self) -> bool {
        metadata_valid(&self.display_name)
            && (self.state.is_empty() || valid_line(&self.state, 128))
            && (self.summary.is_empty() || super::valid_text(&self.summary, MAX_SUMMARY_BYTES))
            && self.elapsed_seconds <= i64::MAX as u64
            && self.total_tokens <= i64::MAX as u64
    }
}

fn metadata_valid(value: &str) -> bool {
    value.is_empty()
        || (value.chars().count() <= MAX_METADATA_CHARS
            && valid_line(value, MAX_METADATA_CHARS * 4))
}

fn valid_date(date: i64) -> bool {
    date >= 0 && jiff::Timestamp::from_millisecond(date).is_ok()
}

#[derive(Default)]
pub(in crate::workspace::saved_chats) struct Entries {
    pub(in crate::workspace::saved_chats) records: BTreeMap<String, Zeroizing<Vec<u8>>>,
    pub(in crate::workspace::saved_chats) duplicates: usize,
    pub(in crate::workspace::saved_chats) full: usize,
    pub(in crate::workspace::saved_chats) invalid: usize,
}

pub(in crate::workspace::saved_chats) fn entries(
    bytes: &[u8],
    redactor: &Redactor,
    mut reserve: impl FnMut(&str, usize) -> bool,
) -> io::Result<Entries> {
    let document = java_xml::document(bytes)?;
    let root = document.root_element();
    if !java_xml::is_element(root, "swarmChats") {
        return Err(invalid("Expected a Java korTTY swarmChats document."));
    }
    let mut result = Entries::default();
    for node in java_xml::children(root, "swarmChat", MAX_XML_CHATS)? {
        let Ok(archive) = parse_chat(node, redactor) else {
            result.invalid += 1;
            continue;
        };
        let path = format!("{FOLDER}/{}.json", archive.id);
        if result.records.contains_key(&path) {
            result.duplicates += 1;
        } else if result.records.len() == MAX_CHATS {
            result.full += 1;
        } else {
            match encode(&Stored::JavaArchive(Box::new(archive))) {
                Ok(bytes) if reserve(&path, bytes.len()) => {
                    result.records.insert(path, bytes);
                }
                Ok(_) => result.full += 1,
                Err(_) => result.invalid += 1,
            }
        }
    }
    Ok(result)
}

fn parse_chat(node: Node<'_, '_>, redactor: &Redactor) -> io::Result<Archive> {
    let fields = java_xml::fields(node, CHAT_FIELDS)?;
    let id = java_xml::chat_id(redactor, java_xml::value(&fields, "id")?.trim())?;
    let digest = store::fingerprint(format!("kortty Java Swarm archive v1:{id}").as_bytes())?;
    let mut compact = [0; 16];
    compact.copy_from_slice(&digest[..16]);
    let id = format!("{:032x}", u128::from_be_bytes(compact));
    let updated = java_xml::timestamp(&fields, "updatedAt")?;
    let created = java_xml::timestamp(&fields, "createdAt")?
        .or(updated)
        .unwrap_or_default();
    let messages = wrapped_nodes(&fields, "messages", "swarmMessage", MAX_MESSAGES)?
        .into_iter()
        .map(|node| parse_message(node, created, redactor))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Archive {
        version: VERSION,
        id,
        title: java_xml::masked_metadata(
            redactor,
            &java_xml::value(&fields, "title")?,
            MAX_TITLE_CHARS,
        )?,
        created,
        updated: updated.unwrap_or(created),
        profile_name: metadata(&fields, "activeAiProfileName", redactor)?,
        response_language: java_xml::safe_hint(
            redactor,
            java_xml::value(&fields, "responseLanguageCode")?.trim(),
            store::valid_response_language,
        )?,
        messages,
        fingerprint: [0; 32],
    })
}

fn parse_message(
    node: Node<'_, '_>,
    chat_created: i64,
    redactor: &Redactor,
) -> io::Result<Message> {
    let fields = java_xml::fields(node, MESSAGE_FIELDS)?;
    let role = match java_xml::value(&fields, "role")?.trim() {
        "USER" => Role::User,
        "ASSISTANT" => Role::Assistant,
        _ => return Err(invalid("Invalid Java Swarm message role.")),
    };
    let servers = wrapped_nodes(&fields, "serverSummaries", "serverSummary", MAX_TARGETS)?
        .into_iter()
        .map(|node| parse_server(node, redactor))
        .collect::<io::Result<Vec<_>>>()?;
    Ok(Message {
        role,
        content: mask_content(redactor, &java_xml::value(&fields, "content")?)?,
        created: java_xml::timestamp(&fields, "createdAt")?.unwrap_or(chat_created),
        profile_name: metadata(&fields, "aiProfileName", redactor)?,
        servers,
    })
}

fn mask_content(redactor: &Redactor, content: &str) -> io::Result<String> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(invalid("Java Swarm message exceeds the text limit."));
    }
    mask::prompt_counted(redactor, content)
        .map(|(masked, _)| masked)
        .map_err(io::Error::other)
}

fn parse_server(node: Node<'_, '_>, redactor: &Redactor) -> io::Result<Server> {
    let fields = java_xml::fields(node, SERVER_FIELDS)?;
    Ok(Server {
        display_name: metadata(&fields, "serverDisplayName", redactor)?,
        state: java_xml::masked_metadata(redactor, &java_xml::value(&fields, "finalState")?, 32)?,
        summary: java_xml::masked_text(
            redactor,
            &java_xml::value(&fields, "summaryText")?,
            MAX_SUMMARY_BYTES,
        )?,
        elapsed_seconds: metric(&fields, "elapsedSeconds")?,
        total_tokens: metric(&fields, "totalTokens")?,
    })
}

fn wrapped_nodes<'a, 'input>(
    fields: &java_xml::Fields<'a, 'input>,
    wrapper: &str,
    item: &str,
    limit: usize,
) -> io::Result<Vec<Node<'a, 'input>>> {
    fields.get(wrapper).map_or_else(
        || Ok(Vec::new()),
        |&node| java_xml::children(node, item, limit),
    )
}

fn metadata(
    fields: &java_xml::Fields<'_, '_>,
    name: &str,
    redactor: &Redactor,
) -> io::Result<String> {
    java_xml::masked_metadata(
        redactor,
        &java_xml::value(fields, name)?,
        MAX_METADATA_CHARS,
    )
}

fn metric(fields: &java_xml::Fields<'_, '_>, name: &str) -> io::Result<u64> {
    let value = java_xml::value(fields, name)?;
    if value.trim().is_empty() {
        return Ok(0);
    }
    value
        .trim()
        .parse::<u64>()
        .map_err(|_| invalid("Invalid Java Swarm server metric."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::private_files::Directory;
    use crate::terminal_logging::{Policy, Secrets};
    use crate::workspace::saved_chats::backup;

    const FIXTURE: &str = include_str!("fixtures/java-2.26.0.xml");

    #[test]
    fn java_swarm_backup_reopens_masked_history_without_execution_bindings() -> io::Result<()> {
        let secrets = Secrets::default();
        secrets.add("synthetic-private-token");
        let redactor = Redactor::for_journal(secrets.seal()?, Policy::default());
        let converted = backup::java_swarm_entries(FIXTURE.as_bytes(), &redactor, |_, _| true)?;
        assert_eq!(
            (converted.entries.len(), converted.invalid, converted.full),
            (1, 0, 0)
        );
        let (path, bytes) = converted.entries.first_key_value().unwrap();
        let (_, name) = path.split_once('/').unwrap();
        let json = std::str::from_utf8(bytes).unwrap();
        for forbidden in [
            "synthetic-private-token",
            "synthetic-private-group",
            "profile_id",
            "target_connection",
            "routes",
        ] {
            assert!(!json.contains(forbidden), "retained {forbidden}");
        }
        assert_ne!(name, "9012ae78adc243018ffec59163856c4d.json");
        backup::validate_java_swarm(name, bytes)?;
        let profile = crate::snippets::tests::Profile::new();
        let directory = Directory::open(&profile.0, false)?.private_child(FOLDER, true)?;
        directory.publish_copy(name, bytes)?;
        let list = super::super::list(&profile.0)?;
        let [summary] = list.chats.as_slice() else {
            panic!("expected one archived Swarm")
        };
        assert!(summary.is_java_archive);
        assert_eq!(summary.target_count, 2);
        assert!(super::super::load(&profile.0, &summary.id).is_err());
        let archive = super::super::load_archive(&profile.0, summary)?;
        assert_eq!(archive.title, "Grüße & 東京");
        assert_eq!(archive.response_language.as_deref(), Some("de-DE"));
        assert_eq!(archive.updated, 1720000003000);
        let [first, response, incomplete] = archive.messages.as_slice() else {
            panic!("expected two prompts and one response")
        };
        assert!(first.role == Role::User && incomplete.role == Role::User);
        assert_eq!(incomplete.content, "Check again?");
        assert!(response.role == Role::Assistant);
        assert!(response.content.contains("# Combined result") && response.content.contains("***"));
        assert_eq!(response.servers[0].state, "DONE");
        assert_eq!(response.servers[0].elapsed_seconds, 17);
        assert_eq!(response.servers[0].total_tokens, 1205);
        assert_eq!(response.servers[1].state, "SKIPPED");
        assert_eq!(response.servers[1].summary, "***");
        assert!(
            backup::bind_swarm_routes(name, bytes, &std::collections::HashSet::from([42, 88]))?
                .is_none()
        );
        super::super::delete_archive(&profile.0, summary)?;
        assert!(super::super::load_archive(&profile.0, summary).is_err());
        let deleted = directory.open_private_file(name)?;
        assert!(deleted.metadata()?.len() < 128);
        assert!(directory.publish_copy(name, bytes).is_err());
        Ok(())
    }

    #[test]
    fn java_swarm_backup_rejects_active_or_malformed_payloads() -> io::Result<()> {
        let redactor = super::super::super::java_xml_redactor(&Policy::default())?;
        let body = FIXTURE
            .split_once("<swarmChats>")
            .unwrap()
            .1
            .split_once("</swarmChats>")
            .unwrap()
            .0;
        let invalid_chat = body.replace(
            "<elapsedSeconds>17</elapsedSeconds>",
            "<elapsedSeconds>-17</elapsedSeconds>",
        );
        let xml = format!("<swarmChats>{body}{body}{invalid_chat}</swarmChats>");
        let converted = entries(xml.as_bytes(), &redactor, |_, _| true)?;
        assert_eq!(
            (
                converted.records.len(),
                converted.duplicates,
                converted.invalid
            ),
            (1, 1, 1)
        );
        let declined = entries(FIXTURE.as_bytes(), &redactor, |_, _| false)?;
        assert_eq!((declined.records.len(), declined.full), (0, 1));
        let external =
            br#"<!DOCTYPE swarmChats [<!ENTITY file SYSTEM "file:///secret">]><swarmChats/>"#;
        assert!(entries(external, &redactor, |_, _| true).is_err());
        let (path, bytes) = converted.records.first_key_value().unwrap();
        let name = path.split_once('/').unwrap().1;
        let mut active = serde_json::from_slice::<serde_json::Value>(bytes)?;
        active["profile_id"] = serde_json::json!(77);
        assert!(backup::validate_java_swarm(name, &serde_json::to_vec(&active)?).is_err());
        Ok(())
    }
}
