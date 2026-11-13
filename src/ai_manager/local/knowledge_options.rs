//! Reviewed folder scan options for knowledge sources.

use std::path::Path;

use iced::Element;
use iced::widget::{checkbox, column, pick_list, text, text_input};

use crate::knowledge_pdf as pdf;

use crate::ai_manager::profiles::picker::DismissOnEscape as _;

use super::knowledge::source::{
    DEFAULT_TEXT_BYTES, FileSizeLimit, MAX_TEXT_BYTES, Options, SyncMode, validate_file_size,
};

const KIB: usize = 1024;
const MIB: usize = KIB * KIB;

#[derive(Clone)]
pub(crate) enum Edit {
    Sync(SyncMode),
    Recursive(bool),
    GitIgnore(bool),
    IncludePdf(bool),
    MaxFileBytes(String),
    Includes(String),
    Excludes(String),
}

#[derive(Default)]
pub(super) struct Draft {
    options: Options,
    max_file_bytes: String,
    size_edited: bool,
    includes: String,
    excludes: String,
}

impl Draft {
    pub(super) fn set(&mut self, options: &Options) {
        self.options = options.clone();
        self.max_file_bytes = file_size_text(options.max_file_bytes.map(FileSizeLimit::bytes));
        self.size_edited = false;
        self.includes = options.includes.join("; ");
        self.excludes = options.excludes.join("; ");
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Sync(value) => self.options.sync = value,
            Edit::Recursive(value) => self.options.recursive = value,
            Edit::GitIgnore(value) => self.options.respect_gitignore = value,
            Edit::IncludePdf(value) => self.options.include_pdf = value,
            Edit::MaxFileBytes(value) => {
                self.max_file_bytes = value;
                self.size_edited = true;
            }
            Edit::Includes(value) => self.includes = value.chars().take(4096).collect(),
            Edit::Excludes(value) => self.excludes = value.chars().take(4096).collect(),
        }
    }

    pub(super) fn options(&self) -> Result<Options, String> {
        let mut options = self.options.clone();
        if self.size_edited {
            options.max_file_bytes = file_size(&self.max_file_bytes)?.map(FileSizeLimit::reviewed);
        }
        let patterns = |text: &str| {
            text.split(';')
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(str::to_owned)
                .collect()
        };
        options.includes = patterns(&self.includes);
        options.excludes = patterns(&self.excludes);
        options.validate()?;
        Ok(options)
    }

    pub(super) fn view(&self, enabled: bool) -> Element<'_, Edit> {
        let legacy_size = !self.size_edited
            && self
                .options
                .max_file_bytes
                .is_some_and(FileSizeLimit::uses_previous_boundary);
        column![
            text("Folder scan options").size(16),
            enabled.then(|| {
                pick_list(SyncMode::ALL, Some(self.options.sync), Edit::Sync).dismiss_on_escape()
            }),
            (self.options.sync == SyncMode::Automatic).then(|| text("Automatic sync checks new and changed files within these reviewed rules. Approve the embedding destination separately.").size(13)),
            checkbox(self.options.recursive)
                .label("Include subfolders")
                .on_toggle_maybe(enabled.then_some(Edit::Recursive)),
            checkbox(self.options.respect_gitignore)
                .label("Respect the source root's .gitignore")
                .on_toggle_maybe(enabled.then_some(Edit::GitIgnore)),
            checkbox(self.options.include_pdf)
                .label("Include PDFs with a text layer")
                .on_toggle_maybe(enabled.then_some(Edit::IncludePdf)),
            text(format!(
                "Maximum bytes per file. Blank uses {} KiB for UTF-8 and {} MiB for PDFs. An explicit limit allows up to {} MiB for UTF-8 and {} MiB for PDFs.",
                DEFAULT_TEXT_BYTES / KIB,
                pdf::DEFAULT_FILE_BYTES / MIB,
                MAX_TEXT_BYTES / MIB,
                pdf::MAX_FILE_BYTES / MIB,
            )).size(13),
            text_input("Use format defaults", &self.max_file_bytes)
                .on_input_maybe(enabled.then_some(Edit::MaxFileBytes)),
            legacy_size.then(|| text("Re-enter a saved size to review it against the higher format limits.").size(13)),
            text_input("Include globs separated by semicolons", &self.includes)
                .on_input_maybe(enabled.then_some(Edit::Includes)),
            text_input("Exclude globs separated by semicolons", &self.excludes)
                .on_input_maybe(enabled.then_some(Edit::Excludes)),
            text("Use relative paths with /, * for one segment and ** for subfolders. Review the resulting file list before applying these options.").size(13),
        ]
        .spacing(6)
        .into()
    }
}

pub(super) fn file_size_help(path: &Path) -> String {
    if pdf::is_pdf(path) {
        format!(
            "Maximum PDF size in bytes. Blank uses {} MiB. An explicit limit allows up to {} MiB.",
            pdf::DEFAULT_FILE_BYTES / MIB,
            pdf::MAX_FILE_BYTES / MIB,
        )
    } else {
        format!(
            "Maximum UTF-8 file size in bytes. Blank uses {} KiB. An explicit limit allows up to {} MiB.",
            DEFAULT_TEXT_BYTES / KIB,
            MAX_TEXT_BYTES / MIB,
        )
    }
}

pub(super) fn file_size(input: &str) -> Result<Option<u64>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    let limit = Some(input.parse::<u64>().unwrap_or_default());
    validate_file_size(limit)?;
    Ok(limit)
}

pub(super) fn file_size_text(value: Option<u64>) -> String {
    value.map(|bytes| bytes.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::knowledge::{Store, folder};
    use super::super::{Configuration, Dialog, Message, Output, State};
    use super::*;
    use iced::futures::{StreamExt as _, executor::block_on};

    #[test]
    fn legacy_folder_size_only_changes_when_the_size_field_is_edited() {
        let limit = u64::try_from(MIB).unwrap();
        let options: Options =
            serde_json::from_value(serde_json::json!({"max_file_bytes": limit})).unwrap();
        let mut draft = Draft::default();
        draft.set(&options);
        assert_eq!(draft.max_file_bytes, limit.to_string());
        draft.update(Edit::Recursive(false));
        let saved_size = draft.options().unwrap().max_file_bytes.unwrap();
        assert_eq!(saved_size.bytes(), limit);
        assert!(!saved_size.is_reviewed());

        draft.update(Edit::MaxFileBytes(limit.to_string()));
        let reviewed_size = draft.options().unwrap().max_file_bytes.unwrap();
        assert_eq!(reviewed_size.bytes(), limit);
        assert!(reviewed_size.is_reviewed());
    }

    #[test]
    fn explicit_size_edits_apply_by_format_and_reset_file_sync_approval() {
        let profile = crate::snippets::tests::Profile::new();
        let text_path = profile.0.join("notes.txt");
        let pdf_path = profile.0.join("runbook.PDF");
        let mut store = Store::new("Source sizes").unwrap();
        store.add_source(text_path.clone()).unwrap();
        store.add_source(pdf_path.clone()).unwrap();
        assert!(store.set_file_sync(text_path.clone(), SyncMode::Automatic));

        let limit = MIB as u64;
        let legacy = serde_json::to_value(std::collections::BTreeMap::from([(
            text_path.clone(),
            limit,
        )]))
        .unwrap();
        let mut store = serde_json::to_value(store).unwrap();
        store["file_size_limits"] = legacy.clone();
        store["automatic_file_size_limits"] = legacy;
        let saved = State {
            stores: vec![serde_json::from_value(store).unwrap()],
            ..State::default()
        };
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let send = |dialog: &mut Dialog, draft: &mut State, event| {
            let _ = dialog.update(event, draft, &saved, false);
        };
        for event in [
            Message::SelectStore(0),
            Message::ConfigureFileSize(0, text_path.clone()),
            Message::ApplyFileSize(0, text_path.clone()),
        ] {
            send(&mut dialog, &mut draft, event);
        }
        let store = &draft.stores[0];
        assert_eq!(store.file_size_limit(&text_path), Some(limit));
        assert_eq!(
            store.effective_file_size_limit(&text_path),
            DEFAULT_TEXT_BYTES as u64
        );
        assert_eq!(store.file_sync(&text_path), SyncMode::Automatic);
        assert_eq!(
            store.effective_file_size_limit(&pdf_path),
            pdf::DEFAULT_FILE_BYTES as u64
        );

        let pdf_limit = (4 * MIB) as u64;
        for event in [
            Message::Consent(true),
            Message::ConfigureFileSize(0, text_path.clone()),
            Message::FileSize(limit.to_string()),
            Message::ApplyFileSize(0, text_path.clone()),
        ] {
            send(&mut dialog, &mut draft, event);
        }
        assert_eq!(draft.stores[0].effective_file_size_limit(&text_path), limit);
        assert_eq!(draft.stores[0].file_sync(&text_path), SyncMode::Manual);
        assert!(!dialog.consent);
        assert!(
            dialog
                .status
                .as_deref()
                .unwrap()
                .contains("Automatic again")
        );

        for event in [
            Message::ConfigureFileSize(0, pdf_path.clone()),
            Message::FileSize(pdf_limit.to_string()),
            Message::ApplyFileSize(0, pdf_path.clone()),
            Message::ConfigureFileSize(0, pdf_path.clone()),
        ] {
            send(&mut dialog, &mut draft, event);
        }
        assert_eq!(
            draft.stores[0].effective_file_size_limit(&pdf_path),
            pdf_limit
        );
        assert_eq!(
            dialog.file_size.as_ref().unwrap().input,
            pdf_limit.to_string()
        );
    }

    #[test]
    fn knowledge_corpus_size_requires_explicit_file_and_folder_approval() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.join("corpus");
        std::fs::create_dir(&root).unwrap();
        let text = root.join("large.txt");
        let mut file = std::fs::File::create(&text).unwrap();
        use std::io::Write as _;
        let block = vec![b'x'; MIB];
        for _ in 0..33 {
            file.write_all(&block).unwrap();
        }
        drop(file);

        let bytes = MAX_TEXT_BYTES as u64;
        let old: FileSizeLimit =
            serde_json::from_value(serde_json::json!({"large_text_bytes": bytes})).unwrap();
        let mut options = Options {
            max_file_bytes: Some(old),
            ..Options::default()
        };
        let mut folder_draft = Draft::default();
        folder_draft.set(&options);
        assert_eq!(folder_draft.options().unwrap().max_file_bytes, Some(old));
        let mut store = Store::new("Corpus folder").unwrap();
        let (reply, _cancel) = store
            .folder_review_options(root.clone(), options.clone())
            .unwrap();
        let refused = reply.recv_blocking().unwrap().unwrap();
        assert_eq!(refused.folder.len(), 0);
        assert!(matches!(refused.rows[0].status, folder::Status::Skipped(_)));
        folder_draft.update(Edit::MaxFileBytes(bytes.to_string()));
        options = folder_draft.options().unwrap();
        assert_eq!(options.max_file_bytes, Some(FileSizeLimit::reviewed(bytes)));
        let (reply, _cancel) = store.folder_review_options(root, options.clone()).unwrap();
        let reviewed = reply.recv_blocking().unwrap().unwrap();
        assert_eq!(reviewed.folder.len(), 1);
        assert_eq!(store.source_count(), 0);
        store.approve_folder(&reviewed).unwrap();
        let restored: Store = serde_json::from_value(serde_json::to_value(store).unwrap()).unwrap();
        assert_eq!(
            restored.folders[0].options.max_file_bytes,
            options.max_file_bytes
        );

        let mut store = Store::new("Corpus file").unwrap();
        store.add_source(text.clone()).unwrap();
        assert!(store.set_file_sync(text.clone(), SyncMode::Automatic));
        let saved = State {
            stores: vec![store],
            ..State::default()
        };
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        for event in [
            Message::SelectStore(0),
            Message::ConfigureFileSize(0, text.clone()),
            Message::FileSize(bytes.to_string()),
        ] {
            let _ = dialog.update(event, &mut draft, &saved, false);
        }
        assert_eq!(
            draft.stores[0].effective_file_size_limit(&text),
            DEFAULT_TEXT_BYTES as u64
        );
        let _ = dialog.update(
            Message::ApplyFileSize(0, text.clone()),
            &mut draft,
            &saved,
            false,
        );
        assert_eq!(draft.stores[0].effective_file_size_limit(&text), bytes);
        assert_eq!(draft.stores[0].file_sync(&text), SyncMode::Manual);
    }

    #[test]
    fn knowledge_source_options_require_review_and_survive_reopening() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.join("source");
        std::fs::create_dir_all(root.join("nested")).unwrap();
        for (name, text) in [
            ("main.rs", "fn main() {}"),
            ("skip.rs", "fn skipped() {}"),
            ("drop.rs", "fn excluded() {}"),
            ("nested/child.rs", "fn child() {}"),
            ("notes.txt", "Only Rust files were selected"),
            (".gitignore", "skip.rs\n"),
        ] {
            std::fs::write(root.join(name), text).unwrap();
        }
        std::fs::write(root.join("at-limit.rs"), "x".repeat(DEFAULT_TEXT_BYTES)).unwrap();
        std::fs::write(
            root.join("over-limit.rs"),
            "x".repeat(DEFAULT_TEXT_BYTES + 1),
        )
        .unwrap();
        let saved = State {
            stores: vec![Store::new("Source options").unwrap()],
            ..State::default()
        };
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let finish = |dialog: &mut Dialog, draft: &mut State| {
            let task = dialog.update(Message::ReviewFolder, draft, &saved, false);
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(message)) = block_on(stream.next()) else {
                panic!("folder review did not finish");
            };
            assert!(matches!(
                message,
                Message::Finished(_, Ok(Output::FolderReview(_)))
            ));
            let _ = dialog.update(message, draft, &saved, false);
        };
        let _ = dialog.update(Message::SelectStore(0), &mut draft, &saved, false);
        let _ = dialog.update(
            Message::Source(root.display().to_string()),
            &mut draft,
            &saved,
            false,
        );
        for edit in [
            Edit::Includes("**/*.rs".into()),
            Edit::Excludes("**/drop.rs".into()),
            Edit::Recursive(false),
        ] {
            let _ = dialog.update(Message::FolderOptions(edit), &mut draft, &saved, false);
        }
        finish(&mut dialog, &mut draft);
        let review = dialog.folder_review.as_ref().unwrap();
        assert!(review.rows.iter().any(|row| {
            row.path == "at-limit.rs"
                && matches!(
                    row.status,
                    folder::Status::Ready {
                        bytes: DEFAULT_TEXT_BYTES,
                        ..
                    }
                )
        }));
        assert!(review.rows.iter().any(|row| {
            row.path == "over-limit.rs" && matches!(row.status, folder::Status::Skipped(_))
        }));
        assert_eq!(draft.stores[0].source_count(), 0);
        let _ = dialog.update(
            Message::FolderOptions(Edit::MaxFileBytes("0".into())),
            &mut draft,
            &saved,
            false,
        );
        let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, false);
        assert_eq!(draft.stores[0].source_count(), 0);
        let task = dialog.update(Message::ReviewFolder, &mut draft, &saved, false);
        assert!(iced_runtime::task::into_stream(task).is_none());
        assert!(
            dialog
                .status
                .as_deref()
                .is_some_and(|status| status.contains("maximum file size"))
        );
        let _ = dialog.update(
            Message::FolderOptions(Edit::MaxFileBytes("15".into())),
            &mut draft,
            &saved,
            false,
        );
        finish(&mut dialog, &mut draft);
        let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, false);
        assert_eq!(draft.stores[0].source_count(), 1);

        let _ = dialog.update(Message::ConfigureFolder(0), &mut draft, &saved, false);
        assert_eq!(dialog.folder_options.max_file_bytes, "15");
        for edit in [Edit::Recursive(true), Edit::GitIgnore(false)] {
            let _ = dialog.update(Message::FolderOptions(edit), &mut draft, &saved, false);
        }
        finish(&mut dialog, &mut draft);
        assert_eq!(draft.stores[0].source_count(), 1);
        let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, false);
        assert_eq!(draft.stores[0].source_count(), 3);
        let configuration = Configuration {
            local: draft,
            ..Configuration::default()
        };
        configuration
            .save_fixture_at(&profile.0, &Configuration::default())
            .unwrap();
        let restored = Configuration::load_fixture_from(&profile.0).unwrap();
        let options = &restored.local.stores[0].folders[0].options;
        assert!(options.recursive);
        assert!(!options.respect_gitignore);
        assert_eq!(options.max_file_bytes.map(FileSizeLimit::bytes), Some(15));
        assert_eq!(options.includes, ["**/*.rs"]);
        assert_eq!(options.excludes, ["**/drop.rs"]);
    }
}
