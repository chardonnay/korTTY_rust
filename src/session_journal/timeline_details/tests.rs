use std::fs;
use std::path::PathBuf;

use super::super::{Journal, LinkField, read};
use super::{MAX_CHAT_ROWS, MAX_CHAT_UNITS, MAX_MARKER_UNITS};

struct Fixture(PathBuf);

impl Fixture {
    fn new(entries: &str, markers: &str) -> Self {
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-timeline-details-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("journal.xml"),
            format!(
                "<session-journal formatVersion=\"1\" id=\"viewer-details\"><meta/>\
                <entries>{entries}</entries><markerDefinitions>{markers}</markerDefinitions>\
                </session-journal>"
            ),
        )
        .unwrap();
        Self(directory)
    }

    fn read(&self) -> Journal {
        read(&self.0).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn journal_viewer_sorts_dates_and_keeps_their_capture_ranges() {
    let fixture = Fixture::new(
        "<entry><id>undated-one</id><logStartSeq>1</logStartSeq></entry>\
         <entry><id>plus-two</id><createdAt>2026-10-01T10:00:00+02:00</createdAt>\
           <logPart>2</logPart><logStartSeq>20</logStartSeq><logEndSeq>29</logEndSeq></entry>\
         <entry><id>zero-one</id><createdAt>2026-10-01T08:00:00Z</createdAt>\
           <logPart>1</logPart><logStartSeq>5</logStartSeq><logEndSeq>5</logEndSeq></entry>\
         <entry><id>earliest</id><createdAt>2026-10-01T07:59:00Z</createdAt></entry>\
         <entry><id>zero-two</id><createdAt>2026-10-01T08:00:00+00:00</createdAt>\
           <logPart>1</logPart><logStartSeq>6</logStartSeq><logEndSeq>7</logEndSeq></entry>\
         <entry><id>undated-two</id><logEndSeq>30</logEndSeq></entry>",
        "",
    );
    let journal = fixture.read();
    let rows: Vec<_> = journal
        .timeline
        .iter()
        .map(|entry| {
            (
                entry.id.as_str(),
                entry.log_part,
                entry.log_start,
                entry.log_end,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("earliest", None, None, None),
            ("zero-one", Some(1), Some(5), Some(5)),
            ("zero-two", Some(1), Some(6), Some(7)),
            ("plus-two", Some(2), Some(20), Some(29)),
            ("undated-one", None, Some(1), None),
            ("undated-two", None, None, Some(30)),
        ]
    );
    assert!(!journal.timeline_limited);
}

#[test]
fn journal_viewer_resolves_legacy_snapshot_orphan_and_unmarked_labels_and_colors() {
    let fixture = Fixture::new(
        "<entry><marker>ERROR</marker></entry>\
         <entry><markerId>Deploy _ Please</markerId><marker>IMPORTANT</marker></entry>\
         <entry><markerId>Missing Definition</markerId><marker>INFO</marker></entry>\
         <entry><markerId>NONE</markerId><marker>ERROR</marker></entry>\
         <entry><markerId>info</markerId></entry><entry><marker>INFO</marker></entry>\
         <entry><markerId>broken-color</markerId></entry>",
        "<marker><id>deploy-please</id><name>Deploy &amp; Review</name><color>#3264A9</color><legacyMarker>IMPORTANT</legacyMarker></marker>\
         <marker><id>broken-color</id><color>bad</color><legacyMarker>ERROR</legacyMarker></marker>",
    );
    let journal = fixture.read();
    assert!(!journal.timeline_limited);
    let rows = &journal.timeline;
    assert_eq!(rows[0].marker, "ERROR");
    assert_eq!(rows[0].marker_key.as_deref(), Some("error"));
    assert_eq!(rows[0].marker_name, "Error");
    assert_eq!(rows[0].marker_color, Some([0xcf, 0x22, 0x2e]));
    assert_eq!(rows[1].marker, "Deploy _ Please");
    assert_eq!(rows[1].marker_key.as_deref(), Some("deploy-please"));
    assert_eq!(rows[1].marker_name, "Deploy & Review");
    assert_eq!(rows[1].marker_color, Some([0x32, 0x64, 0xa9]));
    assert_eq!(rows[2].marker_name, "missing-definition");
    assert_eq!(rows[2].marker_color, Some([0x09, 0x69, 0xda]));
    assert_eq!(rows[3].marker, "NONE");
    assert!(rows[3].marker_key.is_none());
    assert!(rows[3].marker_name.is_empty());
    assert!(rows[3].marker_color.is_none());
    assert_eq!(rows[4].marker_key, rows[5].marker_key);
    assert_eq!(rows[4].marker_name, "Info");
    assert_eq!(rows[6].marker_name, "broken-color");
    assert_eq!(rows[6].marker_color, Some([0xcf, 0x22, 0x2e]));
}

#[test]
fn journal_viewer_projects_java_card_analysis_agent_metadata_and_chat_steps() {
    let fixture = Fixture::new(
        "<entry><kind>SCREENSHOT</kind><screenshotFile>screenshots/shot-000001.png</screenshotFile>\
           <aiDescription>Summary &lt;screen&gt;</aiDescription><aiTags><tag>green</tag><tag> </tag><tag>dashboard</tag></aiTags>\
           <aiAnalysisModel>Not a visible screenshot card field</aiAnalysisModel></entry>\
         <entry><kind>AGENT</kind><state>FAILED</state><commandState>RUNNING</commandState><runAsUser>root</runAsUser>\
           <agentModel>Local model</agentModel><agentDurationMillis>62000</agentDurationMillis><agentTokens>1290</agentTokens>\
           <chatId>private-chat-id</chatId><chatSnapshot><capturedAt>2026-10-01T09:00:00Z</capturedAt><frozen>true</frozen>\
             <messages><message><role>user</role><content>One question</content></message>\
             <message><role>assistant</role><aiProfileName>Local profile</aiProfileName><createdAt>2026-10-01T09:01:00Z</createdAt>\
               <content>One answer</content><steps><step><status>DONE</status><title>Inspect</title><summary>Safe output</summary>\
               <command>ls</command><exitCode>0</exitCode><output>last line</output><truncated>true</truncated></step></steps>\
             </message></messages></chatSnapshot></entry>\
         <entry><kind>SESSION_SUMMARY</kind><state>RAW</state><commandState>ENDED_WITH_SESSION</commandState></entry>",
        "",
    );
    let journal = fixture.read();
    assert!(!journal.timeline_limited);
    assert_eq!(
        journal.timeline[0].extra_details,
        ["AI analysis", "Summary <screen>", "green", "dashboard"]
    );
    assert!(journal.timeline[0].chat_details.is_empty());
    let agent = &journal.timeline[1];
    assert_eq!(
        agent.extra_details,
        [
            "failed",
            "AI agent",
            "still running",
            "AI chat",
            "ROOT",
            "Local model · 1 min 2 s · 1.3k tokens"
        ]
    );
    let chat = &agent.chat_details;
    assert!(chat[0].starts_with("Show AI chat (2 messages) · as of "));
    assert!(chat[0].ends_with(" (final)"));
    assert_eq!(&chat[1..3], ["You", "One question"]);
    assert!(chat[3].starts_with("Local profile · "));
    assert_eq!(
        &chat[4..],
        [
            "One answer",
            "Steps (1)",
            "DONE Inspect Safe output Exit 0",
            "$ ls",
            "last line",
            "Output truncated"
        ]
    );
    assert_eq!(
        journal.timeline[2].extra_details,
        ["raw", "session summary", "session ended"]
    );
}

#[test]
fn journal_viewer_bounds_chat_rows_without_clipping_base_text_or_links() {
    let messages = "<message><role>assistant</role><content>ordinary</content></message>"
        .repeat(MAX_CHAT_ROWS);
    let fixture = Fixture::new(
        &format!(
            "<entry><kind>AGENT</kind><title>Keep this title</title><text>Keep this base text</text>\
           <userNote>Use https://example.test/retained</userNote><outputExcerpt><line>Keep excerpt</line></outputExcerpt>\
           <chatSnapshot><messages>{messages}<message><content>Omitted tail</content></message></messages></chatSnapshot>\
         </entry>"
        ),
        "",
    );
    let journal = fixture.read();
    assert!(journal.timeline_limited);
    let row = &journal.timeline[0];
    assert_eq!(row.title, "Keep this title");
    assert_eq!(row.text, "Keep this base text");
    assert_eq!(row.note, "Use https://example.test/retained");
    assert_eq!(row.output, ["Keep excerpt"]);
    assert_eq!(row.link_spans(LinkField::Note, 100).len(), 2);
    assert_eq!(row.chat_details.len(), MAX_CHAT_ROWS);
    assert_eq!(
        row.chat_details.last().unwrap(),
        "Chat transcript shortened"
    );
    assert!(
        !row.chat_details
            .iter()
            .any(|line| line.contains("Omitted tail"))
    );
}

#[test]
fn journal_viewer_bounds_utf16_and_recovers_from_bad_optional_details() {
    let large = "a".repeat(MAX_CHAT_UNITS) + "😀 hidden suffix";
    let name = "😀".repeat(MAX_MARKER_UNITS);
    let fixture = Fixture::new(
        &format!(
            "<entry><kind>AGENT</kind><markerId>large-name</markerId><chatSnapshot><messages>\
           <message><content>{large}</content></message></messages></chatSnapshot></entry>\
         <entry><kind>AGENT</kind><title>Visible despite malformed data</title><agentTokens>invalid</agentTokens></entry>"
        ),
        &format!("<marker><id>large-name</id><name>{name}</name></marker>"),
    );
    let journal = fixture.read();
    assert!(journal.timeline_limited);
    let first = &journal.timeline[0];
    assert!(first.marker_name.ends_with('…'));
    assert!(first.marker_name.encode_utf16().count() <= MAX_MARKER_UNITS);
    let data_units = first.chat_details[..first.chat_details.len() - 1]
        .iter()
        .map(|value| value.encode_utf16().count())
        .sum::<usize>();
    assert!(data_units <= MAX_CHAT_UNITS);
    assert!(
        !first
            .chat_details
            .iter()
            .any(|line| line.contains("hidden suffix"))
    );
    assert_eq!(journal.timeline[1].title, "Visible despite malformed data");
    assert_eq!(journal.timeline[1].extra_details, ["AI agent"]);
}
