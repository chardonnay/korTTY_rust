use std::fs;

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-metadata-{}",
            crate::private_files::nonce().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn journal(&self, xml: &str) -> PathBuf {
        let directory = self.0.join("journal");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(DOCUMENT), xml).unwrap();
        directory
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// On-disk fixture matching the pinned JAXB v1 model, including fields the native viewer ignores.
const IMPORT: &str = r##"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<session-journal id="metadata-fixture" formatVersion="1">
  <meta>
    <title>Original &amp; title</title>
    <description><![CDATA[Original <description>]]></description>
    <sourceKind>JOB</sourceKind><sourceId>job-7</sourceId><runId>run-3</runId>
    <pinned>true</pinned><startedAt>2026-11-06T13:00:00Z</startedAt>
    <futureField attr="keep">  this stays byte for byte &#x41;  </futureField>
  </meta>
  <markerDefinitions><markerDefinition id="x"><label>Review</label></markerDefinition></markerDefinitions>
  <entries>
    <entry><id>one</id><kind>SCREENSHOT</kind><title>Screenshot</title>
      <screenshotFile>screenshots/shot-000001.png</screenshotFile>
      <userNote><![CDATA[https://example.test/a?x=1&y=2]]></userNote>
      <futureEntryField>keep &quot;quoted&quot;</futureEntryField>
    </entry>
  </entries>
</session-journal>
"##;
const ID: &str = "metadata-fixture";

#[test]
fn journal_metadata_pin_and_unpin_preserve_imported_xml_and_skip_unchanged_values() {
    let fixture = Fixture::new();
    let legacy = IMPORT.replace("<pinned>true</pinned>", "");
    let directory = fixture.journal(&legacy);
    let path = directory.join(DOCUMENT);
    let stamp = FileStamp::read(&File::open(&path).unwrap()).unwrap();
    let untouched = edit_metadata(&directory, ID, &MetadataEdit::Pinned(false)).unwrap();
    assert!(!untouched.summary.pinned);
    assert_eq!(fs::read_to_string(&path).unwrap(), legacy);
    stamp
        .require_unchanged(&File::open(&path).unwrap())
        .unwrap();

    let pinned = edit_metadata(&directory, ID, &MetadataEdit::Pinned(true)).unwrap();
    assert!(pinned.summary.pinned);
    let expected = legacy.replace("</meta>", "<pinned>true</pinned></meta>");
    assert_eq!(fs::read_to_string(&path).unwrap(), expected);
    let stamp = FileStamp::read(&File::open(&path).unwrap()).unwrap();
    let unchanged = edit_metadata(&directory, ID, &MetadataEdit::Pinned(true)).unwrap();
    assert!(unchanged.summary.pinned);
    stamp
        .require_unchanged(&File::open(&path).unwrap())
        .unwrap();

    let released = edit_metadata(&directory, ID, &MetadataEdit::Pinned(false)).unwrap();
    assert!(!released.summary.pinned);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        expected.replace("<pinned>true</pinned>", "<pinned>false</pinned>")
    );
}

#[test]
fn journal_metadata_normalization_matches_java_whitespace() {
    for character in ['\u{00a0}', '\u{2007}', '\u{202f}', '\u{0085}'] {
        let title = format!("{character}name{character}");
        assert_eq!(
            MetadataEdit::Title(title.clone()).normalized().unwrap(),
            MetadataEdit::Title(title)
        );
        assert_eq!(
            MetadataEdit::Description(character.into())
                .normalized()
                .unwrap(),
            MetadataEdit::Description(character.into())
        );
    }
    assert_eq!(
        MetadataEdit::Title("\u{2000} name \u{3000}".into())
            .normalized()
            .unwrap(),
        MetadataEdit::Title("name".into())
    );
    assert_eq!(
        MetadataEdit::Description("\t \u{1680}\u{2000}\u{2028}".into())
            .normalized()
            .unwrap(),
        MetadataEdit::Description(String::new())
    );
}

#[test]
fn journal_metadata_startup_cleanup_retains_published_locks() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let lock = lock_metadata(&directory).unwrap();
    assert!(lock.discard_unpublished().is_err());
    assert!(directory.join(LOCK).is_file());
    let lock = lock_metadata(&directory).unwrap();
    fs::remove_file(directory.join(DOCUMENT)).unwrap();
    lock.discard_unpublished().unwrap();
    assert!(!directory.join(LOCK).exists());
}

#[test]
fn journal_metadata_edits_preserve_imported_xml_and_screenshots() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let screenshots = directory.join("screenshots");
    fs::create_dir(&screenshots).unwrap();
    fs::write(screenshots.join("shot-000001.png"), b"untouched screenshot").unwrap();
    fs::write(directory.join("session-log.xml"), b"untouched capture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(directory.join(DOCUMENT), fs::Permissions::from_mode(0o644)).unwrap();
    }
    let renamed = edit_metadata(
        &directory,
        ID,
        &MetadataEdit::Title("  漢 🦀 <work> & \"done\"  ".into()),
    )
    .unwrap();
    assert_eq!(renamed.summary.title, "漢 🦀 <work> & \"done\"");
    assert_eq!(renamed.summary.description, "Original <description>");
    assert!(renamed.summary.ended_at.is_none());
    let expected = IMPORT.replace(
        "<title>Original &amp; title</title>",
        "<title>漢 🦀 &lt;work&gt; &amp; \"done\"</title>",
    );
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected
    );
    let described = edit_metadata(
        &directory,
        ID,
        &MetadataEdit::Description(" \tLine 1\r\n  <Line 2> & next\n ".into()),
    )
    .unwrap();
    // The XML stores CR exactly. The existing display filter omits carriage returns.
    assert_eq!(
        described.summary.description,
        " \tLine 1\n  <Line 2> & next\n "
    );
    let expected = expected.replace(
        "<description><![CDATA[Original <description>]]></description>",
        "<description> \tLine 1&#13;\n  &lt;Line 2&gt; &amp; next\n </description>",
    );
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected
    );
    assert_eq!(
        fs::read(screenshots.join("shot-000001.png")).unwrap(),
        b"untouched screenshot"
    );
    assert_eq!(
        fs::read(directory.join("session-log.xml")).unwrap(),
        b"untouched capture"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(directory.join(DOCUMENT))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn journal_metadata_description_clear_and_missing_fields_preserve_surroundings() {
    let fixture = Fixture::new();
    let original = "<session-journal id=\"metadata-fixture\" formatVersion=\"1\"><meta data=\"keep\"><host>remote</host><!--end--></meta><entries/></session-journal>";
    let directory = fixture.journal(original);
    let result = edit_metadata(&directory, ID, &MetadataEdit::Title("Added".into())).unwrap();
    assert_eq!(result.summary.title, "Added");
    let expected = original.replace("<!--end--></meta>", "<!--end--><title>Added</title></meta>");
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected
    );
    edit_metadata(
        &directory,
        ID,
        &MetadataEdit::Description("preserve".into()),
    )
    .unwrap();
    let cleared =
        edit_metadata(&directory, ID, &MetadataEdit::Description(" \r\n\t".into())).unwrap();
    assert!(cleared.summary.description.is_empty());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected.replace("</meta>", "<description></description></meta>")
    );
}

#[test]
fn journal_metadata_self_closing_fields_and_meta_are_supported() {
    let fixture = Fixture::new();
    let directory = fixture.journal("<session-journal id=\"metadata-fixture\" formatVersion=\"1\"><meta data=\"keep\" /><entries/></session-journal>");
    edit_metadata(&directory, ID, &MetadataEdit::Description("text".into())).unwrap();
    let expected = "<session-journal id=\"metadata-fixture\" formatVersion=\"1\"><meta data=\"keep\" ><description>text</description></meta><entries/></session-journal>";
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected
    );
    let self_closing = expected.replace("<description>text</description>", "<description/>");
    fs::write(directory.join(DOCUMENT), &self_closing).unwrap();
    let result = edit_metadata(&directory, ID, &MetadataEdit::Description("new".into())).unwrap();
    assert_eq!(result.summary.description, "new");
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        expected.replace(">text<", ">new<")
    );
}

#[test]
fn journal_metadata_validation_preserves_existing_document() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    for edit in [
        MetadataEdit::Title(" \n\t".into()),
        MetadataEdit::Title("bad\0title".into()),
        MetadataEdit::Description("bad\u{0008}description".into()),
        MetadataEdit::Description("bad\u{ffff}description".into()),
        MetadataEdit::Title("x".repeat(MAX_FIELD_BYTES + 1)),
        MetadataEdit::Description(" ".repeat(MAX_FIELD_BYTES + 1)),
    ] {
        assert_eq!(
            edit_metadata(&directory, ID, &edit).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
            IMPORT
        );
    }
    assert!(
        edit_metadata(
            &directory,
            "another-id",
            &MetadataEdit::Title("valid".into())
        )
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
    assert!(
        !format!(
            "{:?}",
            MetadataEdit::Description("sensitive content".into())
        )
        .contains("sensitive content")
    );
}

#[test]
fn journal_metadata_rejects_duplicate_nested_or_oversized_xml_without_changes() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    for original in [
        IMPORT.replace("</meta>", "<title>duplicate</title></meta>"),
        IMPORT.replace("Original &amp; title", "<nested/>"),
        format!("<!DOCTYPE session-journal [<!ENTITY unsafe 'text'>]>{IMPORT}"),
        format!("{IMPORT}{}", " ".repeat(MAX_DOCUMENT_BYTES as usize)),
    ] {
        fs::write(directory.join(DOCUMENT), &original).unwrap();
        assert!(edit_metadata(&directory, ID, &MetadataEdit::Title("new".into())).is_err());
        assert_eq!(
            fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
            original
        );
    }
}

#[test]
fn journal_metadata_text_and_escaped_document_limits_are_enforced_before_publication() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let boundary = "x".repeat(MAX_FIELD_BYTES);
    let result =
        edit_metadata(&directory, ID, &MetadataEdit::Description(boundary.clone())).unwrap();
    assert_eq!(result.summary.description, boundary);
    let empty = IMPORT.replace("<![CDATA[Original <description>]]>", "");
    let large = empty.replace(
        "</meta>",
        &format!(
            "<padding>{}</padding></meta>",
            "a".repeat(MAX_DOCUMENT_BYTES as usize - empty.len() - 20)
        ),
    );
    assert!(large.len() as u64 <= MAX_DOCUMENT_BYTES);
    fs::write(directory.join(DOCUMENT), &large).unwrap();
    assert!(
        edit_metadata(
            &directory,
            ID,
            &MetadataEdit::Description("&".repeat(MAX_FIELD_BYTES))
        )
        .is_err()
    );
    assert_eq!(fs::read_to_string(directory.join(DOCUMENT)).unwrap(), large);
}

#[test]
fn journal_metadata_lock_excludes_other_edits_and_allows_retry_after_drop() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let lock = lock_metadata(&directory).unwrap();
    let error =
        edit_metadata(&directory, ID, &MetadataEdit::Description("blocked".into())).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
    #[cfg(unix)]
    let inherited = lock.file.try_clone().unwrap();
    drop(lock);
    assert_eq!(
        edit_metadata(&directory, ID, &MetadataEdit::Description("retry".into()))
            .unwrap()
            .summary
            .description,
        "retry"
    );
    #[cfg(unix)]
    drop(inherited);
    assert!(directory.join(LOCK).is_file());
}

#[test]
fn journal_metadata_detects_document_replacement_and_cleans_failed_stage() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let pending =
        PendingEdit::prepare(&directory, ID, &MetadataEdit::Title("pending".into())).unwrap();
    let changed = IMPORT.replace("Original &amp; title", "Concurrent update");
    let replacement = directory.join("replacement.xml");
    fs::write(&replacement, &changed).unwrap();
    fs::remove_file(directory.join(DOCUMENT)).unwrap();
    fs::rename(replacement, directory.join(DOCUMENT)).unwrap();
    assert!(pending.publish().is_err());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        changed
    );
    assert!(fs::read_dir(&directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".stage-")
    }));
}

#[test]
fn journal_metadata_rejects_document_hardlinks_without_modifying_either_name() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let second = fixture.0.join("other-journal.xml");
    fs::hard_link(directory.join(DOCUMENT), &second).unwrap();
    assert!(edit_metadata(&directory, ID, &MetadataEdit::Title("unsafe".into())).is_err());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
    assert_eq!(fs::read_to_string(second).unwrap(), IMPORT);
}

#[test]
fn journal_metadata_rejects_lock_hardlinks() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    drop(lock_metadata(&directory).unwrap());
    fs::hard_link(directory.join(LOCK), fixture.0.join("other-lock")).unwrap();
    assert!(edit_metadata(&directory, ID, &MetadataEdit::Title("unsafe".into())).is_err());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
}

#[cfg(unix)]
#[test]
fn journal_metadata_rejects_directory_file_and_lock_symlinks() {
    use std::os::unix::fs::symlink;

    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let alias = fixture.0.join("alias");
    symlink(&directory, &alias).unwrap();
    assert!(edit_metadata(&alias, ID, &MetadataEdit::Title("unsafe".into())).is_err());
    let original = fixture.0.join("original.xml");
    fs::rename(directory.join(DOCUMENT), &original).unwrap();
    symlink(&original, directory.join(DOCUMENT)).unwrap();
    assert!(edit_metadata(&directory, ID, &MetadataEdit::Title("unsafe".into())).is_err());
    assert_eq!(fs::read_to_string(&original).unwrap(), IMPORT);
    fs::remove_file(directory.join(DOCUMENT)).unwrap();
    fs::rename(&original, directory.join(DOCUMENT)).unwrap();
    fs::remove_file(directory.join(LOCK)).unwrap();
    fs::write(&original, b"external lock content").unwrap();
    symlink(&original, directory.join(LOCK)).unwrap();
    assert!(lock_metadata(&directory).is_err());
    assert_eq!(fs::read(&original).unwrap(), b"external lock content");
}

#[cfg(unix)]
#[test]
fn journal_metadata_rejects_directory_and_lock_replacement_before_publication() {
    let fixture = Fixture::new();
    let directory = fixture.journal(IMPORT);
    let pending =
        PendingEdit::prepare(&directory, ID, &MetadataEdit::Title("pending".into())).unwrap();
    let detached = fixture.0.join("detached");
    fs::rename(&directory, &detached).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join(DOCUMENT), IMPORT).unwrap();
    assert!(pending.publish().is_err());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
    assert_eq!(fs::read_to_string(detached.join(DOCUMENT)).unwrap(), IMPORT);
    let pending =
        PendingEdit::prepare(&directory, ID, &MetadataEdit::Title("pending".into())).unwrap();
    fs::remove_file(directory.join(LOCK)).unwrap();
    drop(
        Directory::open(&directory, false)
            .unwrap()
            .open_lock(LOCK)
            .unwrap(),
    );
    assert!(pending.publish().is_err());
    assert_eq!(
        fs::read_to_string(directory.join(DOCUMENT)).unwrap(),
        IMPORT
    );
}
