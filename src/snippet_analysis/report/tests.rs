use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

use crate::snippet_analysis::model::{Analysis, Dependency, Finding, Provenance, Purpose, Source};
use crate::snippets::SnippetId;
use crate::snippets::tests::Profile;

use super::{CheckedWriter, Format, Item, Options, Packing, Record, Subject, export};

const CODE: &str = "echo first\nprint two\nprint three\nprint four\nprint five\nprint six\n\
                    print seven\nprint eight\nprint nine\nprint ten\n```\nfull_script_only\n";

fn item() -> Item {
    let source = Source::new(CODE, "Shell", "en", "en", "Script <img src=x>").unwrap();
    let record = Record::new(
        Purpose::Analysis,
        None,
        source,
        Provenance {
            profile_name: "Local AI".into(),
            model: "test-model".into(),
            skill_names: vec!["Shell".into()],
            ..Provenance::default()
        },
        Analysis {
            summary: "<script>alert(1)</script> [link](https://outside.invalid)".into(),
            improvements: vec![Finding {
                id: "F1".into(),
                category: "maintainability".into(),
                severity: "med".into(),
                title: "Quoting".into(),
                detail: "Quote expansion".into(),
                recommendation: "Add quotes".into(),
                line: Some(1),
                file: None,
            }],
            dependencies: vec![Dependency {
                id: "D1".into(),
                name: "sh".into(),
                kind: "runtime".into(),
                purpose: "Shell interpreter".into(),
                suggestion: String::new(),
            }],
            ..Analysis::default()
        },
    )
    .unwrap();
    Item {
        subject: Subject::Snippet(SnippetId::try_from("abc0".repeat(8)).unwrap()),
        record,
        folder_path: "tools/network".into(),
        current_hash: Some("different".into()),
    }
}

fn options(format: Format, packing: Packing) -> Options {
    Options {
        format,
        packing,
        include_code: false,
        exported_at: 1_700_000_000_000,
        locale: "en-US".into(),
        skipped: Vec::new(),
    }
}

fn scratch() -> (Profile, PathBuf) {
    let directory = Profile::new();
    let path = directory.0.canonicalize().unwrap();
    (directory, path)
}

#[test]
fn combined_json_uses_pre_schema_and_excludes_code() {
    let (_scratch, folder) = scratch();
    let target = folder.join("reports.json");
    let mut options = options(Format::Json, Packing::Combined);
    options.include_code = true;
    let result = export(
        target.clone(),
        vec![item(), item()],
        options,
        &AtomicBool::new(false),
    )
    .unwrap();
    let text = fs::read_to_string(target).unwrap();
    let data: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(result.exported, 2);
    assert_eq!(data.as_array().unwrap().len(), 2);
    let report = &data[0];
    assert_eq!(report["schema"], "kortty.snippetAnalysisReport/1");
    assert_eq!(report["kind"], "PRE_APPLY");
    assert_eq!(report["header"]["snippetId"], "abc0".repeat(8));
    assert_eq!(report["header"]["stale"], true);
    assert_eq!(report["findings"][0]["section"], "design");
    assert_eq!(report["findings"][0]["category"], "maintainability");
    assert_eq!(report["findings"][0]["severity"], "medium");
    assert!(report["apply"].is_null() && report["diagram"].is_null());
    assert!(!text.contains("full_script_only"));
    assert!(!text.contains("echo first"));
}

#[test]
fn standalone_html_escapes_model_text_and_includes_excerpt_without_full_code() {
    let (_scratch, folder) = scratch();
    let target = folder.join("report.html");
    export(
        target.clone(),
        vec![item()],
        options(Format::Html, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    let text = fs::read_to_string(target).unwrap();
    assert!(text.contains("Content-Security-Policy"));
    assert!(text.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!text.contains("<script>"));
    assert!(!text.contains("<img src="));
    assert!(!text.contains("href=\"https://outside.invalid"));
    assert!(text.contains("echo first"));
    assert!(!text.contains("full_script_only"));
}

#[test]
fn markdown_neutralizes_links_and_expands_code_fences() {
    let (_scratch, folder) = scratch();
    let target = folder.join("report.md");
    let mut options = options(Format::Markdown, Packing::Single);
    options.include_code = true;
    export(
        target.clone(),
        vec![item()],
        options,
        &AtomicBool::new(false),
    )
    .unwrap();
    let text = fs::read_to_string(target).unwrap();
    assert!(text.contains("## Analysed code"));
    assert!(text.contains("````Shell\n"));
    assert!(text.contains("\n```\nfull_script_only\n````\n"));
    assert!(!text.contains("[link](https://outside.invalid)"));
    assert!(!text.contains("<script>"));
}

#[test]
fn zip_keeps_library_paths_and_reports_skipped_snippets() {
    let (_scratch, folder) = scratch();
    let target = folder.join("reports.zip");
    let mut options = options(Format::Markdown, Packing::Zip);
    options.skipped.push("Missing\n\u{001b}[31m".into());
    let result = export(
        target.clone(),
        vec![item(), item()],
        options,
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(target).unwrap()).unwrap();
    assert_eq!(result.skipped, 1);
    assert_eq!(archive.len(), 3);
    let first = archive.by_index(0).unwrap().name().unwrap().into_owned();
    let second = archive.by_index(1).unwrap().name().unwrap().into_owned();
    assert!(first.starts_with("tools/network/01-"));
    assert!(second.starts_with("tools/network/02-"));
    assert!(first.ends_with(".md") && second.ends_with(".md"));
    let mut skipped = String::new();
    archive
        .by_name("skipped.txt")
        .unwrap()
        .read_to_string(&mut skipped)
        .unwrap();
    assert!(skipped.contains("- Missing"));
    assert!(!skipped.contains('\u{001b}'));
}

#[test]
fn failures_preserve_existing_targets_and_remove_staging() {
    let (_scratch, folder) = scratch();
    let target = folder.join("report.json");
    fs::write(&target, "original").unwrap();
    assert!(
        export(
            target.clone(),
            vec![item()],
            options(Format::Json, Packing::Single),
            &AtomicBool::new(false),
        )
        .is_err()
    );
    assert_eq!(fs::read(&target).unwrap(), b"original");

    let cancelled_target = folder.join("cancelled.json");
    let error = export(
        cancelled_target.clone(),
        vec![item()],
        options(Format::Json, Packing::Single),
        &AtomicBool::new(true),
    )
    .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert!(!cancelled_target.exists());

    let mut invalid = item();
    invalid.folder_path = "tools/../unsafe".into();
    let zip_target = folder.join("unsafe.zip");
    assert!(
        export(
            zip_target.clone(),
            vec![invalid],
            options(Format::Json, Packing::Zip),
            &AtomicBool::new(false),
        )
        .is_err()
    );
    assert!(!zip_target.exists());
    assert_eq!(fs::read_dir(folder).unwrap().count(), 1);
}

#[test]
fn output_writer_bounds_and_cancels_without_retrying_forever() {
    let mut output = Vec::new();
    let mut remaining = 3;
    let cancelled = AtomicBool::new(false);
    let mut writer = CheckedWriter::new(&mut output, &mut remaining, &cancelled);
    writer.write_all(b"ab").unwrap();
    assert!(writer.write_all(b"cd").is_err());
    cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(writer.write_all(b"c").is_err());
    assert_eq!(output, b"ab");
}
