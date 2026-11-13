use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use serde_json::Value;

use crate::snippet_analysis::model::{
    Analysis, ApplyRun, ApplyStatus, ChangeReason, Dependency, Finding, Provenance, Purpose,
    Selection, Source, content_hash, now_millis,
};
use crate::snippet_analysis::store::Store;
use crate::snippets::SnippetId;
use crate::snippets::tests::Profile;

use super::{
    CheckedWriter, Format, Item, Options, Packing, Progress, Record, Subject, export,
    export_script, export_with_progress, formatted_destination,
};

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
        selection: Selection {
            findings: vec!["F1".into()],
            ..Selection::default()
        },
        post: None,
    }
}

fn options(format: Format, packing: Packing) -> Options {
    Options {
        format,
        packing,
        include_code: false,
        branding: Default::default(),
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
fn report_destination_adds_the_selected_suffix_without_renaming_directories() {
    let (_scratch, folder) = scratch();
    for (name, format, packing, expected) in [
        ("deploy", Format::Pdf, Packing::Single, "deploy.pdf"),
        ("deploy.PDF", Format::Pdf, Packing::Single, "deploy.PDF"),
        (
            "deploy.md",
            Format::Json,
            Packing::Combined,
            "deploy.md.json",
        ),
        ("deploy", Format::Markdown, Packing::Zip, "deploy.zip"),
        ("deploy.ZIP", Format::Html, Packing::Zip, "deploy.ZIP"),
    ] {
        let path = formatted_destination(folder.join(name), format, packing).unwrap();
        assert_eq!(path, folder.join(expected));
    }
    let separator = std::path::MAIN_SEPARATOR;
    for leaf in [format!("deploy{separator}"), format!("deploy{separator}.")] {
        let typed = PathBuf::from(format!("{}{separator}{leaf}", folder.display()));
        assert!(formatted_destination(typed, Format::Pdf, Packing::Single).is_err());
    }
}

#[test]
fn successful_export_folder_reveal_rejects_a_replacement_directory() {
    let (_scratch, folder) = scratch();
    let destination = folder.join("published");
    fs::create_dir(&destination).unwrap();
    let outcome = export_script(
        destination.join("deploy.final.sh"),
        "echo deploy\n",
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        outcome.folder.current_path().unwrap(),
        destination.as_path()
    );

    fs::rename(&destination, folder.join("original")).unwrap();
    fs::create_dir(&destination).unwrap();
    assert!(outcome.folder.current_path().is_err());
}

#[test]
fn batch_report_progress_counts_rendered_reports_in_combined_and_zip_output() {
    let (_scratch, folder) = scratch();
    for (index, (format, packing)) in [
        (Format::Html, Packing::Combined),
        (Format::Pdf, Packing::Combined),
        (Format::Markdown, Packing::Combined),
        (Format::Json, Packing::Combined),
        (Format::Markdown, Packing::Zip),
    ]
    .into_iter()
    .enumerate()
    {
        let target = folder.join(format!("batch-{index}.{}", packing.extension(format)));
        let mut first = item();
        first.record.source.snippet_name = "First".into();
        let mut second = item();
        second.record.source.snippet_name = "Second".into();
        let mut settings = options(format, packing);
        settings.skipped.push("Unanalysed".into());
        let mut progress = Vec::new();
        let outcome = export_with_progress(
            target.clone(),
            vec![first, second],
            settings,
            &AtomicBool::new(false),
            |event| {
                assert!(
                    !target.exists(),
                    "output committed before progress finished"
                );
                progress.push(event);
            },
        )
        .unwrap();
        assert_eq!(outcome.exported, 2);
        assert_eq!(outcome.skipped, 1);
        assert!(target.is_file());
        assert_eq!(
            progress,
            [
                Progress::Rendering {
                    completed: 0,
                    total: 2,
                    last: None
                },
                Progress::Rendering {
                    completed: 1,
                    total: 2,
                    last: Some("First".into())
                },
                Progress::Rendering {
                    completed: 2,
                    total: 2,
                    last: Some("Second".into())
                },
                Progress::Publishing { total: 2 },
            ]
        );
    }
}

#[test]
fn batch_report_progress_cancellation_before_commit_leaves_no_output() {
    let (_scratch, folder) = scratch();
    for cancel_during_publication in [false, true] {
        let target = folder.join("batch.json");
        let cancelled = AtomicBool::new(false);
        let error = export_with_progress(
            target.clone(),
            vec![item(), item()],
            options(Format::Json, Packing::Combined),
            &cancelled,
            |event| {
                if match event {
                    Progress::Rendering { completed: 1, .. } => !cancel_during_publication,
                    Progress::Publishing { .. } => cancel_during_publication,
                    _ => false,
                } {
                    cancelled.store(true, std::sync::atomic::Ordering::Release);
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!target.exists());
        assert_eq!(fs::read_dir(&folder).unwrap().count(), 0);
    }
}

#[test]
fn plain_script_export_preserves_bytes_and_keeps_existing_files() {
    let (_scratch, folder) = scratch();
    let target = folder.join("deploy.analysed.sh");
    let script = "#!/bin/sh\r\n# Grüß dich\r\nprintf '%s' é";
    let outcome = export_script(target.clone(), script, &AtomicBool::new(false)).unwrap();
    assert_eq!(
        outcome.written_files.as_slice(),
        std::slice::from_ref(&target)
    );
    assert_eq!(fs::read(&target).unwrap(), script.as_bytes());
    assert_eq!(
        export_script(target.clone(), "replacement", &AtomicBool::new(false))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(&target).unwrap(), script.as_bytes());
    let cancelled = folder.join("deploy.final.sh");
    assert_eq!(
        export_script(cancelled.clone(), script, &AtomicBool::new(true))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::Interrupted
    );
    assert!(!cancelled.exists());
    assert_eq!(fs::read_dir(&folder).unwrap().count(), 1);
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
    assert_eq!(report["findings"][0]["selected"], true);
    assert_eq!(report["dependencies"][0]["selected"], false);
    assert!(report["apply"].is_null() && report["diagram"].is_null());
    assert!(!text.contains("full_script_only"));
    assert!(!text.contains("echo first"));
}

#[test]
fn after_apply_reports_bind_verification_and_export_the_reviewed_diff() {
    let (_scratch, folder) = scratch();
    let store = Store::new(folder.clone());
    let Item {
        subject,
        record: pre,
        ..
    } = item();
    store.append(subject, pre.clone()).unwrap();
    let after = CODE.replacen("echo first", "echo 'first'", 1);
    let mut run = ApplyRun::new(
        CODE.into(),
        after.clone(),
        Selection {
            findings: vec!["F1".into()],
            ..Selection::default()
        },
        vec![ChangeReason {
            id: "F1".into(),
            reason: "Preserve literal arguments.".into(),
            anchor: Some("echo 'first'".into()),
        }],
        now_millis(),
    )
    .unwrap();
    run.code_text_language = Some(crate::ai_manager::CodeTextLanguage::German);
    store
        .append_apply_while(subject, pre.id, &pre.source.sha256, run.clone(), || true)
        .unwrap();
    let pending = store.load(subject).unwrap();
    let proposed =
        super::Post::capture(pending.current().unwrap(), run.id, &pending.records).unwrap();
    assert!(proposed.verification.is_none());
    assert!(
        Item::batch(&pending, String::new(), pre.source.sha256.clone())
            .unwrap()
            .post
            .is_none()
    );

    store
        .decide_apply_while(
            subject,
            pre.id,
            run.id,
            &run.after_sha256,
            ApplyStatus::Accepted,
            || true,
        )
        .unwrap();
    let verify = |content: &str| {
        Record::new(
            Purpose::Verify,
            Some(pre.id),
            Source::new(content, "Shell", "en", "en", "Script").unwrap(),
            pre.provenance.clone(),
            Analysis {
                summary: "Verified code.".into(),
                improvements: vec![Finding {
                    id: "F2".into(),
                    category: "optimization".into(),
                    title: "Cache repeated lookups".into(),
                    ..Finding::default()
                }],
                dependencies: vec![Dependency {
                    id: "D2".into(),
                    name: " SH ".into(),
                    ..Dependency::default()
                }],
                ..Analysis::default()
            },
        )
        .unwrap()
    };
    let exact = verify(&after);
    let exact_id = exact.id;
    store
        .append_verification_while(subject, pre.id, run.id, exact, || true)
        .unwrap();
    let later = format!("{after}echo later\n");
    for _ in 0..8 {
        store
            .append_verification_while(subject, pre.id, run.id, verify(&later), || true)
            .unwrap();
    }
    let history = store.load(subject).unwrap();
    let current = history.current().unwrap().verification.as_ref().unwrap();
    assert!(!current.matches_applied_revision);
    let verified_at = history
        .records
        .iter()
        .find(|record| record.id == exact_id)
        .unwrap()
        .analyzed_at;
    let item = Item::batch(
        &history,
        "tools/network".into(),
        content_hash(&after).unwrap(),
    )
    .unwrap();
    assert_eq!(item.record.id, pre.id);
    assert_eq!(
        item.post
            .as_ref()
            .unwrap()
            .verification
            .as_ref()
            .unwrap()
            .verified_at,
        verified_at
    );

    for format in [Format::Json, Format::Markdown, Format::Html, Format::Pdf] {
        let target = folder.join(format!("post.{}", format.extension()));
        export(
            target.clone(),
            vec![item.clone()],
            options(format, Packing::Single),
            &AtomicBool::new(false),
        )
        .unwrap();
        let bytes = fs::read(target).unwrap();
        let content = String::from_utf8_lossy(&bytes);
        match format {
            Format::Json => {
                let report: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(report["kind"], "POST_APPLY");
                assert_eq!(report["header"]["runNumber"], 1);
                assert_eq!(report["header"]["stale"], false);
                assert_eq!(report["header"]["analysisLanguage"], "en");
                assert_eq!(report["header"]["codeTextLanguage"], "de");
                assert_eq!(report["findings"][0]["status"], "applied");
                assert_eq!(report["findings"][0]["hunks"], serde_json::json!([0]));
                assert_eq!(report["dependencies"][0]["status"], "not-selected");
                assert_eq!(report["apply"]["outcome"], "accepted");
                assert_eq!(report["apply"]["changes"][0]["anchor"], "echo 'first'");
                let hunk = &report["apply"]["diff"]["hunks"][0];
                assert_eq!(hunk["findings"], serde_json::json!(["F1"]));
                let diff = hunk["lines"].as_array().unwrap();
                assert!(diff.contains(&Value::String("+echo 'first'".into())));
                assert_eq!(report["verification"]["resolved"][0]["id"], "F1");
                assert_eq!(report["verification"]["persisting"][0]["previousId"], "D1");
                assert_eq!(report["verification"]["new"][0]["id"], "F2");
            }
            Format::Markdown => {
                assert!(content.contains("```diff"));
                assert!(content.contains("+echo 'first'"));
                assert!(content.contains("**Changed region 1** F1"));
                assert!(content.contains("Verification"));
            }
            Format::Html => {
                assert!(content.contains("class=\"add\">+echo &#39;first&#39;"));
                assert!(content.contains("<a href=\"#r1-f-4631\">F1</a>"));
                assert_eq!(content.matches("id=\"r1-f-4631\"").count(), 1);
                assert!(content.contains("Still open (1)"));
            }
            Format::Pdf => assert!(bytes.starts_with(b"%PDF-")),
        }
        assert!(!content.contains("echo later"));
    }

    let target = folder.join("combined-post.html");
    export(
        target.clone(),
        vec![item.clone(), item],
        options(Format::Html, Packing::Combined),
        &AtomicBool::new(false),
    )
    .unwrap();
    let combined = fs::read_to_string(target).unwrap();
    for index in 1..=2 {
        let fragment = format!("r{index}-f-4631");
        assert_eq!(combined.matches(&format!("id=\"{fragment}\"")).count(), 1);
        assert_eq!(
            combined.matches(&format!("href=\"#{fragment}\"")).count(),
            1
        );
    }

    store.remove(subject, exact_id).unwrap();
    let history = store.load(subject).unwrap();
    let item = Item::batch(&history, String::new(), content_hash(&later).unwrap()).unwrap();
    assert!(item.post.unwrap().verification.is_none());
}

#[test]
fn after_apply_reports_omit_unresolved_anchors() {
    let (_scratch, folder) = scratch();
    let mut item = item();
    let run = ApplyRun::new(
        CODE.into(),
        CODE.replacen("echo first", "echo 'first'", 1),
        item.selection.clone(),
        vec![ChangeReason {
            id: "F1".into(),
            reason: "Preserve literal arguments.".into(),
            anchor: Some("full_script_only".into()),
        }],
        now_millis(),
    )
    .unwrap();
    let run_id = run.id;
    item.record.apply_runs.push(run);
    item.post = super::Post::capture(&item.record, run_id, &[]);
    let target = folder.join("unresolved.json");
    export(
        target.clone(),
        vec![item],
        options(Format::Json, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    let content = fs::read_to_string(target).unwrap();
    let report: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(report["apply"]["changes"][0]["anchor"], "");
    assert_eq!(
        report["apply"]["diff"]["hunks"][0]["findings"],
        serde_json::json!([])
    );
    assert!(report["findings"][0].get("hunks").is_none());
    assert!(!content.contains("full_script_only"));
}

#[test]
fn report_formats_use_the_stored_flowchart_and_surface_rendering_failures() {
    use crate::diagram_window::Chart;
    use crate::snippet_analysis::diagram::AnalysisDiagram;
    use kortty_diagram::parse_mermaid;
    use std::collections::HashMap;

    let (_scratch, folder) = scratch();
    let mut item = item();
    let chart = Chart::stored(
        parse_mermaid("flowchart LR\na[Read] --> b[Write]").unwrap(),
        HashMap::new(),
        true,
        None,
    );
    item.record.diagram =
        Some(AnalysisDiagram::capture(&chart, item.record.source.line_count).unwrap());
    for format in [Format::Json, Format::Markdown, Format::Html, Format::Pdf] {
        let target = folder.join(format!("with-diagram.{}", format.extension()));
        let outcome = export(
            target.clone(),
            vec![item.clone()],
            options(format, Packing::Single),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(outcome.failed_diagrams, 0);
        let bytes = fs::read(target).unwrap();
        let output = String::from_utf8_lossy(&bytes);
        match format {
            Format::Json => {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(value["diagram"]["type"], "logical-structure");
                assert_eq!(value["diagram"]["fallback"], true);
                assert!(
                    value["diagram"]["mermaid"]
                        .as_str()
                        .unwrap()
                        .starts_with("flowchart TD")
                );
            }
            Format::Markdown => assert!(output.contains("```mermaid\nflowchart TD")),
            Format::Html => assert!(output.contains("src=\"data:image/png;base64,iVBOR")),
            Format::Pdf => {
                assert!(output.contains("/Subtype /Image"));
                if std::env::var_os("KORTTY_SAVE_ANALYSIS_PDF_FIXTURE").is_some() {
                    fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(std::env::temp_dir().join("kortty-analysis-flowchart.pdf"))
                        .unwrap()
                        .write_all(&bytes)
                        .unwrap();
                }
            }
        }
    }
    item.record
        .diagram
        .as_mut()
        .unwrap()
        .mermaid
        .push_str("\nclick a \"https://example.test\"");
    let target = folder.join("invalid-diagram.html");
    let outcome = export(
        target.clone(),
        vec![item],
        options(Format::Html, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(outcome.failed_diagrams, 1);
    let output = fs::read_to_string(target).unwrap();
    assert!(output.contains("The saved flowchart could not be rendered"));
    assert!(!output.contains("data:image/png;base64,"));
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
    let repository = crate::session_journal::export_branding::REPOSITORY_URL;
    assert!(text.contains(&format!("<a href=\"{repository}\" rel=\"noreferrer\">")));
    assert!(!text.contains("class=\"watermark\""));
}

#[test]
fn markdown_reports_publish_native_diagrams_for_every_packing() {
    use crate::diagram_window::Chart;
    use crate::snippet_analysis::diagram::AnalysisDiagram;
    use kortty_diagram::parse_mermaid;

    let (_scratch, folder) = scratch();
    let mut diagram = item();
    let chart = Chart::stored(
        parse_mermaid("flowchart TD\na[Read] --> b[Write]").unwrap(),
        Default::default(),
        false,
        None,
    );
    diagram.record.diagram = Some(AnalysisDiagram::capture(&chart, 1).unwrap());
    let verify_png = |bytes: &[u8]| {
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
        assert!(image.width() > 1 && image.height() > 1);
    };

    let single = folder.join("notes ü #%25.MD");
    let asset = folder.join("notes ü #%25.diagram.png");
    let result = export(
        single.clone(),
        vec![diagram.clone()],
        options(Format::Markdown, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.failed_diagrams, 0);
    assert_eq!(result.written_files, [single.clone(), asset.clone()]);
    let text = fs::read_to_string(single).unwrap();
    assert!(text.contains("![Analysis flowchart](<notes%20%C3%BC%20%23%2525.diagram.png>)"));
    assert!(text.contains("```mermaid\nflowchart TD"));
    verify_png(&fs::read(asset).unwrap());

    let combined = folder.join("combined.md");
    let result = export(
        combined.clone(),
        vec![item(), diagram.clone()],
        options(Format::Markdown, Packing::Combined),
        &AtomicBool::new(false),
    )
    .unwrap();
    let asset = folder.join("combined.02-diagram.png");
    assert_eq!(result.written_files, [combined.clone(), asset.clone()]);
    let text = fs::read_to_string(combined).unwrap();
    assert!(text.contains("![Analysis flowchart](<combined.02-diagram.png>)"));
    assert!(!folder.join("combined.01-diagram.png").exists());
    verify_png(&fs::read(asset).unwrap());

    let target = folder.join("reports.zip");
    let result = export(
        target.clone(),
        vec![diagram.clone(), item()],
        options(Format::Markdown, Packing::Zip),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        result.written_files.as_slice(),
        std::slice::from_ref(&target)
    );
    let mut archive = zip::ZipArchive::new(fs::File::open(target).unwrap()).unwrap();
    assert_eq!(archive.len(), 3);
    let (stem, text) = {
        let mut report = archive.by_index(0).unwrap();
        let name = report.name().unwrap().into_owned();
        let stem = name.strip_suffix(".md").unwrap().to_owned();
        let mut text = String::new();
        report.read_to_string(&mut text).unwrap();
        (stem, text)
    };
    assert!(stem.starts_with("tools/network/01-"));
    let mut png = archive.by_name(&format!("{stem}.diagram.png")).unwrap();
    let basename = stem.rsplit('/').next().unwrap();
    assert!(text.contains(&format!("![Analysis flowchart](<{basename}.diagram.png>)")));
    let mut bytes = Vec::new();
    png.read_to_end(&mut bytes).unwrap();
    verify_png(&bytes);

    let mut too_wide = String::from("flowchart TD\n");
    for index in 1..40 {
        too_wide.push_str(&format!("n0 --> n{index}\n"));
    }
    diagram.record.diagram.as_mut().unwrap().mermaid = too_wide;
    let fallback = folder.join("fallback.md");
    let result = export(
        fallback.clone(),
        vec![diagram],
        options(Format::Markdown, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.failed_diagrams, 1);
    assert_eq!(
        result.written_files.as_slice(),
        std::slice::from_ref(&fallback)
    );
    let text = fs::read_to_string(fallback).unwrap();
    assert!(text.contains("```mermaid\nflowchart TD"));
    assert!(text.contains("The saved flowchart could not be rendered"));
    assert!(!folder.join("fallback.diagram.png").exists());
}

#[test]
fn markdown_asset_publication_rolls_back_before_report_commit() {
    use std::cell::Cell;

    let (_scratch, folder) = scratch();
    let parent = crate::private_files::Directory::open(&folder, false).unwrap();
    let mut stage = super::markdown::Stage::new(&parent, ".kortty-analysis-export").unwrap();
    for name in ["first.png", "second.png"] {
        stage
            .write_file(name, |file| file.write_all(b"fixture"))
            .unwrap();
    }
    stage
        .write_file("report.md", |file| file.write_all(b"private draft"))
        .unwrap();
    let checks = Cell::new(0);
    let error = stage
        .publish(|| {
            if checks.replace(checks.get() + 1) == 2 {
                assert_eq!(fs::read(folder.join("first.png")).unwrap(), b"fixture");
                assert_eq!(fs::read(folder.join("second.png")).unwrap(), b"fixture");
                fs::write(folder.join("report.md"), b"competing export")?;
            }
            Ok(())
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    stage.cleanup(false).unwrap();
    assert!(!folder.join("first.png").exists());
    assert!(!folder.join("second.png").exists());
    assert_eq!(
        fs::read(folder.join("report.md")).unwrap(),
        b"competing export"
    );
    assert_eq!(fs::read_dir(folder).unwrap().count(), 1);
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
    let repository = crate::session_journal::export_branding::REPOSITORY_URL;
    assert!(text.ends_with(&format!(" · <{repository}>\n")));
}

#[test]
fn analysis_report_branding_uses_one_escaped_footer_per_document() {
    use crate::session_journal::export_branding::{Preferences, REPOSITORY_URL};
    use crate::terminal_theme::Rgb;

    let (_scratch, folder) = scratch();
    let preferences = Preferences {
        watermark_enabled: true,
        watermark_color: Rgb::from_bytes([0x26, 0x39, 0x55]),
        watermark_text: "Private <img src=x onerror=\"alert(1)\">".into(),
        footer_text: "Review [notes](https://secret.invalid) & <script>".into(),
        ..Preferences::default()
    };
    for format in [Format::Html, Format::Markdown] {
        for (packing, name, count) in [
            (Packing::Single, "single", 1),
            (Packing::Combined, "combined", 2),
            (Packing::Zip, "archive", 2),
        ] {
            let target = folder.join(format!("{name}-{}", format.extension()));
            let mut options = options(format, packing);
            options.branding = preferences.resolve();
            export(
                target.clone(),
                vec![item(); count],
                options,
                &AtomicBool::new(false),
            )
            .unwrap();

            let documents = if packing == Packing::Zip {
                let mut archive = zip::ZipArchive::new(fs::File::open(&target).unwrap()).unwrap();
                assert_eq!(archive.len(), count);
                (0..count)
                    .map(|index| {
                        let mut text = String::new();
                        archive
                            .by_index(index)
                            .unwrap()
                            .read_to_string(&mut text)
                            .unwrap();
                        text
                    })
                    .collect::<Vec<_>>()
            } else {
                vec![fs::read_to_string(&target).unwrap()]
            };
            for text in documents {
                assert!(!text.contains(REPOSITORY_URL));
                assert!(!text.contains("<script>"));
                match format {
                    Format::Html => {
                        assert_eq!(text.matches("<footer>").count(), 1);
                        assert_eq!(text.matches("class=\"watermark\"").count(), 1);
                        assert!(text.contains("style=\"color:#263955\">Private &lt;img src=x onerror=&quot;alert(1)&quot;&gt;"));
                        assert!(text.contains("</main>\n<footer>Review [notes](https://secret.invalid) &amp; &lt;script&gt;</footer>"));
                        assert!(!text.contains("href=\"https://secret.invalid"));
                        assert!(!text.contains("<img src=x"));
                    }
                    Format::Markdown => {
                        let footer = r"_Review \[notes\](https\://secret\.invalid\) \& \<script\>_";
                        assert_eq!(text.matches(footer).count(), 1);
                        assert!(!text.contains("Private"));
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    let target = folder.join("disabled.html");
    let mut options = options(Format::Html, Packing::Single);
    options.branding = Preferences {
        footer_enabled: false,
        watermark_enabled: false,
        ..preferences
    }
    .resolve();
    export(
        target.clone(),
        vec![item()],
        options,
        &AtomicBool::new(false),
    )
    .unwrap();
    let text = fs::read_to_string(target).unwrap();
    assert!(!text.contains("<footer>"));
    assert!(!text.contains("class=\"watermark\""));
    assert!(!text.contains(REPOSITORY_URL));
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
fn pdf_reports_use_native_pages_fonts_and_bookmarks() {
    let (_scratch, folder) = scratch();
    let mut long = item();
    long.record.source = Source::new(
        &"echo zusätzlicher Text\n".repeat(160),
        "Shell",
        "en",
        "en",
        "Long script",
    )
    .unwrap();
    let without_code = folder.join("without-code.pdf");
    export(
        without_code.clone(),
        vec![long.clone()],
        options(Format::Pdf, Packing::Single),
        &AtomicBool::new(false),
    )
    .unwrap();
    let with_code = folder.join("with-code.pdf");
    let mut pdf_options = options(Format::Pdf, Packing::Single);
    pdf_options.include_code = true;
    export(
        with_code.clone(),
        vec![long.clone()],
        pdf_options,
        &AtomicBool::new(false),
    )
    .unwrap();
    let without_code = fs::read(without_code).unwrap();
    let with_code = fs::read(with_code).unwrap();
    let without_code = String::from_utf8_lossy(&without_code);
    let with_code = String::from_utf8_lossy(&with_code);
    assert!(with_code.starts_with("%PDF-"));
    assert!(with_code.contains("/ToUnicode"));
    assert!(with_code.contains("/Outlines"));
    assert!(with_code.contains("Long script"));
    assert!(
        with_code.matches("/Type /Page\n").count() > without_code.matches("/Type /Page\n").count()
    );

    let combined = folder.join("combined.pdf");
    export(
        combined.clone(),
        vec![item(), long.clone()],
        options(Format::Pdf, Packing::Combined),
        &AtomicBool::new(false),
    )
    .unwrap();
    let combined = fs::read(combined).unwrap();
    let combined = String::from_utf8_lossy(&combined);
    assert!(combined.contains("Long script") && combined.contains("Script <img src=x>"));
    assert!(combined.matches("/Type /Page\n").count() >= 3);

    let zip = folder.join("pdf.zip");
    export(
        zip.clone(),
        vec![long],
        options(Format::Pdf, Packing::Zip),
        &AtomicBool::new(false),
    )
    .unwrap();
    let mut archive = zip::ZipArchive::new(fs::File::open(zip).unwrap()).unwrap();
    let mut entry = archive.by_index(0).unwrap();
    assert!(entry.name().unwrap().ends_with(".pdf"));
    let mut signature = [0; 5];
    entry.read_exact(&mut signature).unwrap();
    assert_eq!(&signature, b"%PDF-");
}

#[test]
fn combined_pdf_contents_links_follow_cover_and_chapter_pagination() {
    let (_scratch, folder) = scratch();
    let target = folder.join("contents.pdf");
    let mut first = item();
    first.record.source = Source::new(
        &"echo extra source line\n".repeat(160),
        "Shell",
        "en",
        "en",
        "Multi-page script",
    )
    .unwrap();
    let reports: Vec<_> = std::iter::once(first)
        .chain(std::iter::repeat_n(item(), 39))
        .collect();
    let count = reports.len();
    let mut options = options(Format::Pdf, Packing::Combined);
    options.include_code = true;
    options.skipped.push("No stored analysis".into());
    export(target.clone(), reports, options, &AtomicBool::new(false)).unwrap();

    let bytes = fs::read(target).unwrap();
    let pdf = String::from_utf8_lossy(&bytes);
    let objects: Vec<_> = pdf.split("\nendobj").map(str::trim_start).collect();
    let links: Vec<_> = objects
        .iter()
        .copied()
        .filter(|object| object.contains("/Subtype /Link") && object.contains("/S /GoTo"))
        .collect();
    let destinations: Vec<_> = links.iter().map(|link| pdf_array(link, "/D [")).collect();
    let outlines: Vec<_> = objects
        .iter()
        .filter(|object| object.contains("/Dest ["))
        .map(|object| pdf_array(object, "/Dest ["))
        .collect();
    assert_eq!(destinations.len(), count);
    assert_eq!(destinations, outlines);

    let pages: Vec<_> = pdf_array(&pdf, "/Kids [")
        .split_whitespace()
        .step_by(3)
        .collect();
    let chapter_pages: Vec<_> = destinations
        .iter()
        .map(|destination| {
            let page = destination.split_whitespace().next().unwrap();
            pages.iter().position(|&id| id == page).unwrap()
        })
        .collect();
    assert_eq!(chapter_pages[0], 2);
    assert!(chapter_pages[1] > chapter_pages[0] + 1);
    assert!(chapter_pages.windows(2).all(|pair| pair[0] < pair[1]));

    let mut attached = 0;
    for page in &pages[..2] {
        let header = format!("{page} 0 obj\n");
        let cover = objects
            .iter()
            .find(|object| object.starts_with(&header))
            .unwrap();
        let annotations: Vec<_> = pdf_array(cover, "/Annots [")
            .split_whitespace()
            .step_by(3)
            .collect();
        let source = format!("/P {page} 0 R\n");
        let mut cover_links = 0;
        for link in links.iter().filter(|link| link.contains(&source)) {
            let annotation = link.split_whitespace().next().unwrap();
            assert!(annotations.contains(&annotation));
            cover_links += 1;
        }
        assert!(cover_links > 0);
        assert_eq!(annotations.len(), cover_links + 1);
        attached += cover_links;
    }
    assert_eq!(attached, count);
    assert!(pdf.contains("/S /URI"));
}

#[test]
fn combined_pdf_uses_landscape_for_wide_diagrams_and_restores_portrait() {
    use std::collections::HashMap;
    use std::fmt::Write as _;

    use kortty_diagram::parse_mermaid;

    use crate::diagram_window::Chart;
    use crate::session_journal::export_branding::Preferences;
    use crate::snippet_analysis::diagram::AnalysisDiagram;

    let diagram = |source| {
        let chart = Chart::stored(parse_mermaid(source).unwrap(), HashMap::new(), false, None);
        AnalysisDiagram::capture(&chart, 1).unwrap()
    };
    let mut wide = item();
    wide.record.diagram = Some(diagram(
        "flowchart TD\na[Start] --> b[Read]\na --> c[Write]\na --> d[Check]",
    ));
    let mut tall_chart = String::from("flowchart TD\n");
    for index in 0..11 {
        writeln!(tall_chart, "n{index} --> n{}", index + 1).unwrap();
    }
    let mut tall = item();
    tall.record.diagram = Some(diagram(&tall_chart));
    let (_scratch, folder) = scratch();
    let target = folder.join("mixed-orientation.pdf");
    let mut options = options(Format::Pdf, Packing::Combined);
    options.include_code = true;
    options.branding = Preferences {
        watermark_enabled: true,
        ..Preferences::default()
    }
    .resolve();
    let result = export(
        target.clone(),
        vec![wide, tall],
        options,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(result.failed_diagrams, 0);

    let bytes = fs::read(target).unwrap();
    let pdf = String::from_utf8_lossy(&bytes);
    let objects: Vec<_> = pdf.split("\nendobj").map(str::trim_start).collect();
    let pages: Vec<_> = objects
        .iter()
        .filter(|page| page.contains("/Type /Page\n"))
        .collect();
    let is_landscape = |page: &str| {
        let mut dimensions = pdf_array(page, "/MediaBox [")
            .split_whitespace()
            .skip(2)
            .map(|value| value.parse::<f32>().unwrap());
        dimensions.next().unwrap() > dimensions.next().unwrap()
    };
    let landscapes: Vec<_> = pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| is_landscape(page).then_some(index))
        .collect();
    assert_eq!(landscapes.len(), 1);
    let landscape = landscapes[0];
    assert!(pages[landscape].contains("/XObject <<"));
    assert!(pages[landscape].contains("/ExtGState <<"));
    assert!(pages[landscape].contains("/Annots ["));
    assert!(!is_landscape(pages[landscape + 1]));

    let chapter_starts: Vec<_> = objects
        .iter()
        .filter(|object| object.contains("/S /GoTo"))
        .map(|link| {
            let target = pdf_array(link, "/D [").split_whitespace().next().unwrap();
            pages
                .iter()
                .position(|page| page.split_whitespace().next().unwrap() == target)
                .unwrap()
        })
        .collect();
    assert_eq!(chapter_starts.len(), 2);
    assert_eq!(chapter_starts[0], 1);
    assert!(chapter_starts[0] < landscape && landscape + 1 < chapter_starts[1]);
    assert!(
        pages[chapter_starts[1]..]
            .iter()
            .filter(|page| page.contains("/XObject <<"))
            .count()
            > 1
    );
}

fn pdf_array<'a>(object: &'a str, key: &str) -> &'a str {
    object.split_once(key).unwrap().1.split_once(']').unwrap().0
}

#[test]
fn pdf_cover_charts_count_exported_findings_dependencies_and_apply_results() {
    let (_scratch, folder) = scratch();
    let mut report = item();
    report.record.analysis.improvements = [
        ("security", "crit"),
        (" SECURITY ", "high"),
        ("security", "High"),
        ("optimization", "moderate"),
        ("maintainability", "low"),
        ("design", "unknown"),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (category, severity))| Finding {
        id: format!("F{}", index + 1),
        title: format!("Finding {}", index + 1),
        category: category.into(),
        severity: severity.into(),
        ..Finding::default()
    })
    .collect();
    report.record.analysis.dependencies = ["D1", "D2"]
        .into_iter()
        .map(|id| Dependency {
            id: id.into(),
            name: id.into(),
            ..Dependency::default()
        })
        .collect();
    report.selection = Selection {
        findings: vec!["F1".into(), "F2".into()],
        dependencies: vec!["D1".into(), "D2".into()],
        ..Selection::default()
    };

    let render = |name, item| {
        let target = folder.join(name);
        export(
            target.clone(),
            vec![item],
            options(Format::Pdf, Packing::Single),
            &AtomicBool::new(false),
        )
        .unwrap();
        pdf_cover_bars(&fs::read(target).unwrap())
    };
    let pre = render("pre-cover.pdf", report.clone());

    let mut run = ApplyRun::new(
        CODE.into(),
        CODE.replacen("echo first", "echo 'first'", 1),
        report.selection.clone(),
        ["F1", "D1"]
            .into_iter()
            .map(|id| ChangeReason {
                id: id.into(),
                reason: "Confirmed in the proposal".into(),
                anchor: None,
            })
            .collect(),
        now_millis(),
    )
    .unwrap();
    run.status = ApplyStatus::Accepted;
    run.decided_at = Some(run.started_at);
    run.validate(&report.record.analysis).unwrap();
    let run_id = run.id;
    report.record.apply_runs.push(run);
    report.post = super::Post::capture(&report.record, run_id, &[]);
    let post = render("post-cover.pdf", report);

    assert_eq!(pre.len(), 4);
    assert_eq!(&post[..4], &pre);
    assert_eq!(post.len(), 5);
    let expected: [&[(f32, [u8; 3])]; 5] = [
        &[
            (1.0 / 3.0, [0x99, 0x1b, 0x1b]),
            (2.0 / 3.0, [0xc0, 0x39, 0x2b]),
        ],
        &[(1.0 / 3.0, [0xe6, 0x7e, 0x22])],
        &[
            (1.0 / 3.0, [0xb7, 0x95, 0x0b]),
            (1.0 / 3.0, [0x7f, 0x8c, 0x8d]),
        ],
        &[(2.0 / 3.0, [0x0d, 0x94, 0x88])],
        &[(0.5, [0x15, 0x80, 0x3d]), (0.5, [0x6b, 0x72, 0x80])],
    ];
    for (bar, expected) in post.iter().zip(expected) {
        let [(track, _), segments @ ..] = bar.as_slice() else {
            panic!("missing chart track");
        };
        assert_eq!(segments.len(), expected.len());
        for ((width, color), (fraction, expected_color)) in segments.iter().zip(expected) {
            assert_eq!(color, expected_color);
            assert!((width / track - fraction).abs() < 0.0001);
        }
    }
}

fn pdf_cover_bars(bytes: &[u8]) -> Vec<Vec<(f32, [u8; 3])>> {
    let pdf = String::from_utf8_lossy(bytes);
    let cover = pdf
        .split("\nendobj")
        .find(|object| object.contains("/Type /Page\n"))
        .unwrap();
    let (_, contents) = cover.split_once("/Contents ").unwrap();
    let stream = contents.split_whitespace().next().unwrap();
    let header = format!("\n{stream} 0 obj\n");
    let start = bytes
        .windows(header.len())
        .position(|part| part == header.as_bytes())
        .unwrap()
        + header.len();
    let stream = &bytes[start..];
    let marker = b"\nstream\n";
    let offset = stream
        .windows(marker.len())
        .position(|part| part == marker)
        .unwrap();
    let dictionary = std::str::from_utf8(&stream[..offset]).unwrap();
    let length: usize = dictionary
        .split_once("/Length ")
        .unwrap()
        .1
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    let mut content = Vec::new();
    let data = &stream[offset + marker.len()..][..length];
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut content)
        .unwrap();

    let mut bars: Vec<Vec<(f32, [u8; 3])>> = Vec::new();
    let mut color = [0; 3];
    for line in String::from_utf8_lossy(&content).lines() {
        if let Some(rgb) = line.strip_suffix(" rg") {
            for (channel, value) in color.iter_mut().zip(rgb.split_whitespace()) {
                *channel = (value.parse::<f32>().unwrap() * 255.0).round() as u8;
            }
        } else if let Some(rectangle) = line.strip_suffix(" re") {
            let mut dimensions = rectangle
                .split_whitespace()
                .skip(2)
                .map(|value| value.parse::<f32>().unwrap());
            let width = dimensions.next().unwrap();
            if dimensions.next() == Some(10.0) {
                if color == [0xe2, 0xe8, 0xf0] {
                    bars.push(Vec::new());
                }
                bars.last_mut().unwrap().push((width, color));
            }
        }
    }
    bars
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
