use std::collections::BTreeSet;
use std::fs;
use std::io::Read as _;

use pgp::composed::{Message, SignedPublicKey};
use pgp::types::{KeyDetails, KeyVersion};
use zip::ZipArchive;

use super::*;
use crate::session_journal::{catalog, export};
use crate::snippets::archive::test_openpgp_key;
use crate::snippets::recipients::{Action, Change, FILE_NAME};

const SCREENSHOT: &str = "shot-000001.png";
const PRIVATE_NOTE: &str = "private journal note";

fn markdown_request<'a>(options: &'a Options, protection: &'a Protection) -> Request<'a> {
    Request {
        format: Format::Markdown,
        options,
        protection,
        links_allowed: false,
        topic_ai: None,
    }
}

struct Fixture {
    scratch: PathBuf,
    root: PathBuf,
    output: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let scratch = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("kortty-export-output-{}", nonce().unwrap()));
        fs::create_dir(&scratch).unwrap();
        let root = scratch.join("journals");
        let output = scratch.join("exports");
        fs::create_dir(&root).unwrap();
        fs::create_dir(&output).unwrap();
        Self {
            scratch,
            root,
            output,
        }
    }

    fn seed(&self, id: &str, title: &str) -> Vec<u8> {
        let directory = self.root.join(id);
        fs::create_dir_all(directory.join("screenshots")).unwrap();
        let screenshot = tiny_skia::Pixmap::new(2, 2).unwrap().encode_png().unwrap();
        fs::write(directory.join("screenshots").join(SCREENSHOT), &screenshot).unwrap();
        fs::write(directory.join("journal.xml"), format!(r#"
<session-journal formatVersion="1" id="{id}">
<meta><title>{title}</title><startedAt>2026-11-06T10:00:00Z</startedAt></meta>
<entries>
<entry><id>note</id><kind>USER_NOTE</kind><title>Entry</title><text>{PRIVATE_NOTE}</text></entry>
<entry><id>shot</id><kind>SCREENSHOT</kind><title>Image</title><screenshotFile>screenshots/{SCREENSHOT}</screenshotFile></entry>
</entries>
</session-journal>"#).trim()).unwrap();
        screenshot
    }

    fn input(&self, id: &str) -> JournalInput {
        self.input_for(id, Format::Markdown)
    }

    fn input_for(&self, id: &str, format: Format) -> JournalInput {
        let list = catalog(&self.root).unwrap();
        let row = list.journals.iter().find(|row| row.id == id).unwrap();
        let load = match format {
            Format::Pdf | Format::Markdown => export::load,
            Format::HtmlBundle => export::load_for_bundle,
        };
        let snapshot = load(
            &self.root,
            &row.directory,
            &row.id,
            row.deletion_identity.as_ref().unwrap(),
        );
        (row.title.clone(), snapshot)
    }

    fn destination(&self, name: &str) -> Destination {
        let parent = Directory::open(&self.output, false).unwrap();
        Destination::new(self.output.join(name), parent.identity().unwrap()).unwrap()
    }

    fn names(&self) -> BTreeSet<String> {
        fs::read_dir(&self.output)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.scratch);
    }
}

#[test]
fn journal_export_ai_uses_the_same_resolved_entry_in_all_formats() {
    use crate::local_ai::test_support::{exclusive_run, fixture_response, start_test_double};
    use crate::session_journal::export::topic_ai::Run;
    use crate::terminal_logging::{Policy, Redactor, Secrets};

    let _exclusive = exclusive_run();
    let fixture = Fixture::new();
    fixture.seed("one", "Journal with duplicate IDs");
    let directory = fixture.root.join("one");
    let xml_path = directory.join("journal.xml");
    let xml = fs::read_to_string(&xml_path).unwrap().replace(
        "<id>shot</id>",
        "<id>note</id><logStartSeq>2</logStartSeq><logEndSeq>2</logEndSeq>",
    );
    fs::write(xml_path, xml).unwrap();
    fs::write(
        directory.join("session-log.json"),
        concat!(
            "{\"type\":\"meta\",\"formatVersion\":1,\"journalId\":\"one\",\"part\":1}\n",
            "{\"seq\":1,\"t\":\"2026-11-06T10:00:01Z\",\"k\":\"out\",\"x\":\"omitted capture\"}\n",
            "{\"seq\":2,\"t\":\"2026-11-06T10:00:02Z\",\"k\":\"out\",\"x\":\"retained capture\"}\n",
            "{\"type\":\"end\"}\n"
        ),
    )
    .unwrap();
    let (profile, server) =
        start_test_double(vec![fixture_response("200 OK", "", r#"{"ids":[2]}"#); 3]);
    let (run, _cancel) = Run::begin(profile).unwrap();
    let topic = TopicSelection::Model {
        run: Box::new(run),
        redactor: Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default()),
    };
    let mut options = Options::default();
    options.filter.topic = "a topic absent from the literal text".into();
    for (format, output) in [
        (Format::Markdown, "result.md"),
        (Format::Pdf, "result.pdf"),
        (Format::HtmlBundle, "result.zip"),
    ] {
        let report = publish(
            &fixture.destination(output),
            1,
            [fixture.input_for("one", format)],
            Request {
                format,
                options: &options,
                protection: &Protection::None,
                links_allowed: false,
                topic_ai: Some(&topic),
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(report.exported, 1);
        assert!(report.topic_notices.is_empty());
    }
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| !request.to_string().contains("omitted capture"))
    );

    let markdown = fs::read_to_string(fixture.output.join("result.md")).unwrap();
    assert!(markdown.contains("AI topic") && markdown.contains("Image"));
    assert!(!markdown.contains(PRIVATE_NOTE));
    let pdf = fs::read(fixture.output.join("result.pdf")).unwrap();
    assert!(String::from_utf8_lossy(&pdf).contains("/Subtype /Image"));
    let mut archive =
        ZipArchive::new(fs::File::open(fixture.output.join("result.zip")).unwrap()).unwrap();
    for (name, keep, omit) in [
        ("journal.xml", "<title>Image</title>", PRIVATE_NOTE),
        ("session-log.json", "retained capture", "omitted capture"),
    ] {
        let mut text = String::new();
        archive
            .by_name(name)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.contains(keep) && !text.contains(omit));
    }
}

#[test]
fn journal_export_ai_fallback_notice_reaches_the_actual_output() {
    use crate::session_journal::export::topic_ai::Fallback;

    let fixture = Fixture::new();
    fixture.seed("one", "Fallback journal");
    let mut options = Options::default();
    options.filter.topic = PRIVATE_NOTE.into();
    let topic = TopicSelection::LocalFallback(Fallback::InvalidReply);
    let report = publish(
        &fixture.destination("fallback.md"),
        1,
        [fixture.input("one")],
        Request {
            format: Format::Markdown,
            options: &options,
            protection: &Protection::None,
            links_allowed: false,
            topic_ai: Some(&topic),
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(report.topic_notices.len(), 1);
    assert!(report.topic_notices[0].contains("local text filter"));
    let markdown = fs::read_to_string(report.path).unwrap();
    assert!(markdown.contains(PRIVATE_NOTE));
    assert!(!markdown.contains("AI topic"));
}

#[test]
fn journal_pdf_export_publishes_plain_pdf_and_password_zip() {
    let fixture = Fixture::new();
    fixture.seed("one", "One journal");
    let options = Options::default();
    let request = |protection| Request {
        format: Format::Pdf,
        options: &options,
        protection,
        links_allowed: false,
        topic_ai: None,
    };
    let report = publish(
        &fixture.destination("report.pdf"),
        1,
        [fixture.input("one")],
        request(&Protection::None),
        &|| false,
    )
    .unwrap();
    assert_eq!(report.exported, 1);
    assert_eq!(report.skipped_assets, 0);
    assert!(fs::read(report.path).unwrap().starts_with(b"%PDF-"));

    let password = Protection::Password(Zeroizing::new("fixture-password".into()));
    let report = publish(
        &fixture.destination("protected.zip"),
        1,
        [fixture.input("one")],
        request(&password),
        &|| false,
    )
    .unwrap();
    let mut archive = ZipArchive::new(fs::File::open(report.path).unwrap()).unwrap();
    assert!(
        archive
            .by_name_decrypt("protected.pdf", b"wrong-password")
            .is_err()
    );
    let mut pdf = Vec::new();
    archive
        .by_name_decrypt("protected.pdf", b"fixture-password")
        .unwrap()
        .read_to_end(&mut pdf)
        .unwrap();
    assert!(pdf.starts_with(b"%PDF-"));

    fixture.seed("two", "one journal");
    let report = publish(
        &fixture.destination("reports.zip"),
        2,
        [fixture.input("one"), fixture.input("two")],
        request(&Protection::None),
        &|| false,
    )
    .unwrap();
    let mut archive = ZipArchive::new(fs::File::open(report.path).unwrap()).unwrap();
    for name in ["One_journal.pdf", "one_journal-2.pdf"] {
        let mut header = [0; 5];
        archive
            .by_name(name)
            .unwrap()
            .read_exact(&mut header)
            .unwrap();
        assert_eq!(&header, b"%PDF-");
    }
}

#[test]
fn journal_export_html_bundle_is_always_an_archive_with_unique_per_journal_paths() {
    let fixture = Fixture::new();
    fixture.seed("one", "Same title");
    fixture.seed("two", "same title");
    let options = Options::default();
    let password = Protection::Password(Zeroizing::new("fixture-password".into()));
    let destination = fixture.destination("bundles.zip");
    let report = publish(
        &destination,
        2,
        [
            fixture.input_for("one", Format::HtmlBundle),
            fixture.input_for("two", Format::HtmlBundle),
        ],
        Request {
            format: Format::HtmlBundle,
            options: &options,
            protection: &password,
            links_allowed: false,
            topic_ai: None,
        },
        &|| false,
    )
    .unwrap();
    assert_eq!(report.exported, 2);
    let mut archive = ZipArchive::new(fs::File::open(report.path).unwrap()).unwrap();
    for folder in ["Same_title", "same_title-2"] {
        for name in ["journal.html", "journal.xml", "screenshots/shot-000001.png"] {
            let path = format!("{folder}/{name}");
            assert!(archive.by_name_decrypt(&path, b"wrong-password").is_err());
            let mut contents = Vec::new();
            archive
                .by_name_decrypt(&path, b"fixture-password")
                .unwrap()
                .read_to_end(&mut contents)
                .unwrap();
            assert!(!contents.is_empty());
        }
    }
}

#[test]
fn journal_export_single_markdown_publishes_sibling_assets_and_rolls_back_conflicts() {
    let fixture = Fixture::new();
    let png = fixture.seed("one", "Sample journal");
    let destination = fixture.destination("my report.md");
    assert!(fixture.names().is_empty());
    let run = || {
        publish(
            &destination,
            1,
            [fixture.input("one")],
            markdown_request(&Options::default(), &Protection::None),
            &|| false,
        )
    };
    let existing = b"existing markdown";
    fs::write(&destination.path, existing).unwrap();
    assert_eq!(run().unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fixture.names(), BTreeSet::from(["my report.md".into()]));
    assert_eq!(fs::read(&destination.path).unwrap(), existing);
    fs::remove_file(&destination.path).unwrap();

    let sidecar = fixture.output.join("my report-files");
    fs::create_dir(&sidecar).unwrap();
    fs::write(sidecar.join("existing"), b"untouched").unwrap();
    assert!(run().is_err());
    assert_eq!(fixture.names(), BTreeSet::from(["my report-files".into()]));
    assert_eq!(fs::read(sidecar.join("existing")).unwrap(), b"untouched");
    fs::remove_dir_all(&sidecar).unwrap();

    let report = run().unwrap();
    assert_eq!(report.exported, 1);
    assert!(report.skipped.is_empty());
    assert_eq!(report.skipped_assets, 0);
    let text = fs::read_to_string(&report.path).unwrap();
    assert!(text.contains(PRIVATE_NOTE));
    assert!(text.contains("my%20report-files/"));
    assert_eq!(
        fixture.names(),
        BTreeSet::from(["my report-files".into(), "my report.md".into()])
    );
    let asset = fs::read_dir(sidecar).unwrap().next().unwrap().unwrap();
    assert_eq!(fs::read(asset.path()).unwrap(), png);
}

#[test]
fn journal_export_zip_names_are_portable_unique_and_keep_per_document_assets() {
    let fixture = Fixture::new();
    fixture.seed("one", "Same title");
    fixture.seed("two", "same title");
    let destination = fixture.destination("many.zip");
    let report = publish(
        &destination,
        3,
        [
            fixture.input("one"),
            fixture.input("two"),
            (
                "Unreadable journal".into(),
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "fixture source was removed",
                )),
            ),
        ],
        markdown_request(&Options::default(), &Protection::None),
        &|| false,
    )
    .unwrap();
    assert_eq!(report.exported, 2);
    assert_eq!(
        report.skipped,
        ["Unreadable journal: fixture source was removed"]
    );
    assert_eq!(fixture.names(), BTreeSet::from(["many.zip".into()]));
    let mut archive = ZipArchive::new(fs::File::open(report.path).unwrap()).unwrap();
    let names = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .collect::<BTreeSet<_>>();
    assert!(names.contains("Same_title.md"));
    assert!(names.contains("same_title-2.md"));
    for stem in ["Same_title", "same_title-2"] {
        let directory = format!("{stem}-files/");
        let asset = names
            .iter()
            .find(|name| name.starts_with(&directory))
            .unwrap();
        let mut md = String::new();
        archive
            .by_name(&format!("{stem}.md"))
            .unwrap()
            .read_to_string(&mut md)
            .unwrap();
        assert!(md.contains(asset));
    }
}

#[test]
fn journal_export_never_publishes_an_archive_when_the_filter_skips_every_journal() {
    let fixture = Fixture::new();
    fixture.seed("one", "First journal");
    fixture.seed("two", "Second journal");
    let mut options = Options::default();
    options.filter.topic = "absent literal".into();
    let error = publish(
        &fixture.destination("empty.zip"),
        2,
        [fixture.input("one"), fixture.input("two")],
        markdown_request(&options, &Protection::None),
        &|| false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("No entries matched"));
    assert!(fixture.names().is_empty());
}

#[test]
fn journal_export_aes_protects_markdown_and_every_screenshot_in_one_archive() {
    let fixture = Fixture::new();
    let png = fixture.seed("one", "Protected journal");
    let destination = fixture.destination("secured.zip");
    let protection = Protection::Password(Zeroizing::new("fixture-password".into()));
    let report = publish(
        &destination,
        1,
        [fixture.input("one")],
        markdown_request(&Options::default(), &protection),
        &|| false,
    )
    .unwrap();
    let payload = fs::read(report.path).unwrap();
    assert!(
        !payload
            .windows(PRIVATE_NOTE.len())
            .any(|bytes| bytes == PRIVATE_NOTE.as_bytes())
    );
    assert!(
        !payload
            .windows(8)
            .any(|bytes| bytes == b"\x89PNG\r\n\x1a\n")
    );
    assert_eq!(fixture.names(), BTreeSet::from(["secured.zip".into()]));
    let mut archive = ZipArchive::new(Cursor::new(payload)).unwrap();
    let asset = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .find(|name| name.starts_with("secured-files/"))
        .unwrap();
    for (index, name) in ["secured.md", asset.as_str()].into_iter().enumerate() {
        assert_eq!(
            archive
                .get_aes_verification_key_and_salt(index)
                .unwrap()
                .unwrap()
                .aes_mode,
            AesMode::Aes256
        );
        assert!(archive.by_name(name).is_err());
        let wrong = archive
            .by_name_decrypt(name, b"wrong")
            .and_then(|mut entry| Ok(entry.read_to_end(&mut Vec::new())?));
        assert!(wrong.is_err());
    }
    let mut md = String::new();
    archive
        .by_name_decrypt("secured.md", b"fixture-password")
        .unwrap()
        .read_to_string(&mut md)
        .unwrap();
    assert!(md.contains(PRIVATE_NOTE));
    assert!(md.contains(&asset));
    let mut bytes = Vec::new();
    archive
        .by_name_decrypt(&asset, b"fixture-password")
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, png);
}

#[test]
fn journal_export_openpgp_encrypts_whole_zip_and_rejects_a_stale_recipient_choice() {
    let fixture = Fixture::new();
    let png = fixture.seed("one", "Public-key export");
    let profile = fixture.scratch.join("profile");
    fs::create_dir(&profile).unwrap();
    let secret = test_openpgp_key(KeyVersion::V4, 44);
    let certificate = SignedPublicKey::from(secret.clone());
    let fingerprint = format!("{:X}", certificate.fingerprint());
    let certificate_path = profile.join("certificate.asc");
    fs::write(
        &certificate_path,
        certificate.to_armored_bytes(Default::default()).unwrap(),
    )
    .unwrap();
    let store = profile.join(FILE_NAME);
    let recipients = Action::Save(
        store.clone(),
        Change {
            previous: None,
            name: "Fixture recipient".into(),
            fingerprint: fingerprint.clone(),
            certificate: Some(certificate_path),
        },
    )
    .run()
    .unwrap();
    let recipient = recipients[0].clone();
    let protection = Protection::OpenPgp {
        profile: profile.clone(),
        recipient: recipient.clone(),
    };
    let destination = fixture.destination("sealed.zip.gpg");
    let report = publish(
        &destination,
        1,
        [fixture.input("one")],
        markdown_request(&Options::default(), &protection),
        &|| false,
    )
    .unwrap();
    let ciphertext = fs::read(report.path).unwrap();
    assert!(
        !ciphertext
            .windows(b"sealed.md".len())
            .any(|bytes| bytes == b"sealed.md")
    );
    let mut message = Message::from_bytes(ciphertext.as_slice())
        .unwrap()
        .decrypt(&"".into(), &secret)
        .unwrap();
    let mut archive = ZipArchive::new(Cursor::new(message.as_data_vec().unwrap())).unwrap();
    let asset = archive
        .file_names()
        .map(|name| name.unwrap().into_owned())
        .find(|name| name.starts_with("sealed-files/"))
        .unwrap();
    let mut markdown = String::new();
    archive
        .by_name("sealed.md")
        .unwrap()
        .read_to_string(&mut markdown)
        .unwrap();
    assert!(markdown.contains(PRIVATE_NOTE));
    assert!(markdown.contains(&asset));
    let mut bytes = Vec::new();
    archive
        .by_name(&asset)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, png);

    let html_report = publish(
        &fixture.destination("html.zip.gpg"),
        1,
        [fixture.input_for("one", Format::HtmlBundle)],
        Request {
            format: Format::HtmlBundle,
            options: &Options::default(),
            protection: &protection,
            links_allowed: false,
            topic_ai: None,
        },
        &|| false,
    )
    .unwrap();
    let ciphertext = fs::read(html_report.path).unwrap();
    let mut message = Message::from_bytes(ciphertext.as_slice())
        .unwrap()
        .decrypt(&"".into(), &secret)
        .unwrap();
    let mut archive = ZipArchive::new(Cursor::new(message.as_data_vec().unwrap())).unwrap();
    let mut html = String::new();
    archive
        .by_name("journal.html")
        .unwrap()
        .read_to_string(&mut html)
        .unwrap();
    assert!(html.contains(PRIVATE_NOTE));
    let mut image = Vec::new();
    archive
        .by_name("screenshots/shot-000001.png")
        .unwrap()
        .read_to_end(&mut image)
        .unwrap();
    assert_eq!(image, png);

    let pdf_report = publish(
        &fixture.destination("document.pdf.gpg"),
        1,
        [fixture.input("one")],
        Request {
            format: Format::Pdf,
            options: &Options::default(),
            protection: &protection,
            links_allowed: false,
            topic_ai: None,
        },
        &|| false,
    )
    .unwrap();
    let ciphertext = fs::read(pdf_report.path).unwrap();
    let mut message = Message::from_bytes(ciphertext.as_slice())
        .unwrap()
        .decrypt(&"".into(), &secret)
        .unwrap();
    assert!(message.as_data_vec().unwrap().starts_with(b"%PDF-"));

    Action::Save(
        store,
        Change {
            previous: Some(recipient),
            name: "Updated recipient".into(),
            fingerprint,
            certificate: None,
        },
    )
    .run()
    .unwrap();
    let stale = fixture.destination("stale.zip.gpg");
    assert!(
        publish(
            &stale,
            1,
            [fixture.input("one")],
            markdown_request(&Options::default(), &protection),
            &|| false
        )
        .is_err()
    );
    assert_eq!(
        fixture.names(),
        BTreeSet::from([
            "document.pdf.gpg".into(),
            "html.zip.gpg".into(),
            "sealed.zip.gpg".into()
        ])
    );
}

#[test]
fn journal_export_rejects_changed_destination_and_rolls_back_cancellation_after_assets_move() {
    let fixture = Fixture::new();
    fixture.seed("one", "Sample");
    let destination = fixture.destination("cancel.md");
    let sidecar = fixture.output.join("cancel-files");
    // Test double for a cancel request arriving after the asset rename, before Markdown commit.
    let cancelled = || sidecar.exists();
    let error = publish(
        &destination,
        1,
        [fixture.input("one")],
        markdown_request(&Options::default(), &Protection::None),
        &cancelled,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert!(fixture.names().is_empty());

    let detached = fixture.scratch.join("old-output");
    fs::rename(&fixture.output, &detached).unwrap();
    fs::create_dir(&fixture.output).unwrap();
    assert!(
        publish(
            &destination,
            1,
            [fixture.input("one")],
            markdown_request(&Options::default(), &Protection::None),
            &|| false
        )
        .is_err()
    );
    assert!(fixture.names().is_empty());
    assert_eq!(fs::read_dir(detached).unwrap().count(), 0);
}

#[test]
fn journal_export_rejects_unsafe_names_and_stops_an_oversized_selection_before_loading() {
    let fixture = Fixture::new();
    let identity = Directory::open(&fixture.output, false)
        .unwrap()
        .identity()
        .unwrap();
    for name in ["CON.md", "x:secret.md", "tailing-space.md "] {
        assert!(Destination::new(fixture.output.join(name), identity).is_err());
    }
    assert!(!valid_component("nested\\file.md"));
    assert_eq!(export_basename("  Ops / nightly  "), "Ops_nightly");
    assert_eq!(export_basename(".."), "session-journal");
    assert_eq!(
        export_basename("a".repeat(200).as_str()).len(),
        MAX_STEM_BYTES
    );
    // Test double for a lazy source. Its callback must remain untouched at an invalid count.
    let load =
        std::iter::from_fn(|| -> Option<JournalInput> { panic!("loaded an oversized selection") });
    assert!(
        publish(
            &fixture.destination("too-many.zip"),
            MAX_CATALOG_ROWS + 1,
            load,
            markdown_request(&Options::default(), &Protection::None),
            &|| false
        )
        .is_err()
    );
    assert!(fixture.names().is_empty());
}
