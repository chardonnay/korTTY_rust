use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use super::{Config, DirectoryMode, Format, Policy, Prepared, Warning};
use crate::snippets::tests::Profile;

fn archive(folder: &Path) -> Option<PathBuf> {
    fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.extension().and_then(std::ffi::OsStr::to_str) == Some("gz")
                && !path
                    .with_file_name(format!(
                        ".{}.lock",
                        path.file_stem().unwrap().to_string_lossy()
                    ))
                    .exists()
        })
}

fn wait<T>(mut get: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(value) = get() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the native logfile"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn fragmented_terminal_output_is_redacted_before_java_json_gzip_publication() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let mut policy = Policy::default();
    let declaration = r#"
[session-journal]
[[session-journal.replace]]
pattern = '(token=)[a-z]+'
replacement = '$1***'
regex = true
"#
    .parse::<toml_edit::DocumentMut>()
    .unwrap();
    policy.read_rule(declaration.as_table()).unwrap();
    let config = Config {
        enabled: true,
        format: Format::Json,
        include_username: true,
        directory_mode: DirectoryMode::Relative,
        ..Config::default()
    };
    let prepared = Prepared::new(
        config,
        policy,
        "ops secret-one".into(),
        "localhost".into(),
        "operator".into(),
    )
    .with_test_profile(root.clone());
    prepared.secrets().add("secret-one");
    prepared.secrets().add("keyboard-value");
    let (redraw, _updates) = async_channel::bounded(1);
    let warning = Warning::new(redraw);
    let mut capture = prepared.start(warning.clone()).unwrap();
    for fragment in [
        b"\x1b]7;file://localhost/home/secret-".as_slice(),
        b"one\x1b\\secret-\x1b[3",
        b"1mone\x1b[0m token=hidden\x1b]52;c;must-not-log",
        b"\x07\x1bPqDCS-must-not-log\x1b\\\r\n",
        b"0%\r100%\n\x1b]7;file://localhost/home/secret-one/docs\x07",
        b"key",
        b"board-value end",
    ] {
        capture.advance(fragment);
    }
    drop(capture);

    let path = wait(|| {
        assert!(warning.current().is_none(), "{:?}", warning.current());
        archive(&root.join("terminal-logs"))
    });
    let mut raw = String::new();
    flate2::read::GzDecoder::new(fs::File::open(&path).unwrap())
        .read_to_string(&mut raw)
        .unwrap();
    for protected in [
        "secret-one",
        "keyboard-value",
        "token=hidden",
        "must-not-log",
    ] {
        assert!(!raw.contains(protected), "captured a protected value");
    }
    let log: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(log["connection"], "ops ***");
    assert_eq!(log["entries"][0]["line"], "*** token=***");
    assert_eq!(log["entries"][0]["username"], "operator");
    assert_eq!(log["entries"][0]["directoryBase"], "/home/***");
    assert_eq!(log["entries"][0]["directory"], ".");
    assert_eq!(log["entries"][1]["line"], "100%");
    assert_eq!(log["entries"][2]["line"], "*** end");
    assert_eq!(log["entries"][2]["directory"], "docs");
    let stamp = log["entries"][0]["timestamp"].as_str().unwrap();
    assert_eq!(stamp.len(), 23);
    assert!(stamp.parse::<jiff::civil::DateTime>().is_ok());
    let report = crate::logfile_window::catalog::scan(&root, || false).unwrap();
    assert_eq!(report.entries.len(), 1);
    assert!(!report.entries[0].live);
}

#[test]
fn capture_is_lazy_and_refuses_an_authentication_value_with_controls() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config = Config {
        enabled: true,
        ..Config::default()
    };
    let (redraw, _updates) = async_channel::bounded(1);
    let warning = Warning::new(redraw);
    let prepare = || {
        Prepared::new(
            config.clone(),
            Policy::default(),
            "host".into(),
            "host".into(),
            "user".into(),
        )
        .with_test_profile(root.clone())
    };
    drop(prepare().start(warning.clone()).unwrap());
    let prepared = prepare();
    prepared.secrets().add("secret\nline");
    assert!(prepared.start(warning).is_err());
    assert!(!root.join("terminal-logs").exists());
}

#[test]
fn xml_capture_replaces_invalid_unicode_and_keeps_writing_later_lines() {
    let profile = Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let config = Config {
        enabled: true,
        format: Format::Xml,
        include_username: true,
        directory_mode: DirectoryMode::Full,
        ..Config::default()
    };
    let prepared = Prepared::new(
        config,
        Policy::default(),
        "site \u{fffe}".into(),
        "host".into(),
        "operator \u{ffff}".into(),
    )
    .with_test_profile(root.clone());
    let (redraw, _updates) = async_channel::bounded(1);
    let warning = Warning::new(redraw);
    let mut capture = prepared.start(warning.clone()).unwrap();
    capture.advance(b"\x1b]7;file://host/tmp/%EF%BF%BE\x07before\n\xef");
    capture.advance(b"\xbf\xbf\nafter\n");
    drop(capture);

    let path = wait(|| {
        assert!(warning.current().is_none(), "{:?}", warning.current());
        archive(&root.join("terminal-logs"))
    });
    let mut raw = String::new();
    flate2::read::GzDecoder::new(fs::File::open(&path).unwrap())
        .read_to_string(&mut raw)
        .unwrap();
    let xml = roxmltree::Document::parse(&raw).unwrap();
    let log = xml.root_element();
    assert_eq!(log.attribute("connection"), Some("site \u{fffd}"));
    let entries: Vec<_> = log.children().filter(roxmltree::Node::is_element).collect();
    assert_eq!(
        entries.iter().map(|entry| entry.text()).collect::<Vec<_>>(),
        [Some("before"), Some("\u{fffd}"), Some("after")]
    );
    assert_eq!(entries[2].attribute("directory"), Some("/tmp/\u{fffd}"));
    assert_eq!(entries[2].attribute("username"), Some("operator \u{fffd}"));
}

#[test]
fn unsupported_managed_redaction_expression_fails_closed() {
    let doc = r#"
[session-journal]
[[session-journal.replace]]
pattern = '(?<=token=)[a-z]+'
regex = true
"#
    .parse::<toml_edit::DocumentMut>()
    .unwrap();
    assert!(Policy::default().read_rule(doc.as_table()).is_err());
}
