//! Reviewed folder scan options for knowledge sources.

use std::path::Path;

use iced::widget::{checkbox, column, pick_list, row, text, text_editor, text_input};
use iced::{Alignment, Element};

use crate::knowledge_pdf as pdf;

use crate::ai_manager::profiles::picker::DismissOnEscape as _;
use crate::ai_manager::search::{KnowledgeSetting, Reveal, Setting};
use crate::workspace::dialog_design::number_step;

use super::knowledge::source::{
    FileSizeLimit, MAX_CONFIGURED_BYTES, NEW_SOURCE_DEFAULT, Options, SyncMode, validate_file_size,
};

const MIB: usize = 1024 * 1024;
const MAX_PATTERN_CHARS: usize = 4096;

#[derive(Clone)]
pub(crate) enum Edit {
    Sync(SyncMode),
    Recursive(bool),
    GitIgnore(bool),
    IncludePdf(bool),
    MaxFileBytes(String),
    Includes(String),
    Excludes(String),
    IncludeLines(text_editor::Action),
    ExcludeLines(text_editor::Action),
}

#[derive(Default)]
pub(super) struct Draft {
    options: Options,
    max_file_bytes: String,
    size_edited: bool,
    includes: text_editor::Content,
    excludes: text_editor::Content,
    multiline: bool,
}

impl Draft {
    pub(super) fn set(&mut self, options: &Options) {
        self.options = options.clone();
        let limit = options.max_file_bytes.map(FileSizeLimit::bytes);
        self.max_file_bytes = if self.multiline {
            format_advanced_limit(limit.unwrap_or(NEW_SOURCE_DEFAULT))
        } else {
            file_size_text(limit)
        };
        self.size_edited = false;
        let separator = if self.multiline { "\n" } else { "; " };
        self.includes = text_editor::Content::with_text(&options.includes.join(separator));
        self.excludes = text_editor::Content::with_text(&options.excludes.join(separator));
    }

    pub(super) fn advanced(options: &Options) -> Self {
        let mut draft = Self {
            multiline: true,
            ..Self::default()
        };
        draft.set(options);
        draft
    }

    pub(super) fn update(&mut self, edit: Edit) {
        match edit {
            Edit::Sync(value) => self.options.sync = value,
            Edit::Recursive(value) => self.options.recursive = value,
            Edit::GitIgnore(value) => self.options.respect_gitignore = value,
            Edit::IncludePdf(value) => self.options.include_pdf = value,
            Edit::MaxFileBytes(value) => {
                self.max_file_bytes = value[..value.floor_char_boundary(32)].to_owned();
                self.size_edited = true;
            }
            Edit::Includes(value) => {
                self.includes = text_editor::Content::with_text(
                    &value.chars().take(MAX_PATTERN_CHARS).collect::<String>(),
                );
            }
            Edit::Excludes(value) => {
                self.excludes = text_editor::Content::with_text(
                    &value.chars().take(MAX_PATTERN_CHARS).collect::<String>(),
                );
            }
            Edit::IncludeLines(action) => edit_pattern(&mut self.includes, action),
            Edit::ExcludeLines(action) => edit_pattern(&mut self.excludes, action),
        }
    }

    pub(super) fn options(&self) -> Result<Options, String> {
        let mut options = self.options.clone();
        if self.size_edited {
            let limit = if self.multiline {
                advanced_file_size(&self.max_file_bytes)?
            } else {
                file_size(&self.max_file_bytes)?.unwrap_or(NEW_SOURCE_DEFAULT)
            };
            options.max_file_bytes = Some(FileSizeLimit::reviewed(limit));
        }
        let separator = if self.multiline { '\n' } else { ';' };
        let patterns = |text: &str| {
            text.split(separator)
                .map(str::trim)
                .filter(|pattern| !pattern.is_empty())
                .map(str::to_owned)
                .collect()
        };
        options.includes = patterns(&self.includes.text());
        options.excludes = patterns(&self.excludes.text());
        options.validate()?;
        Ok(options)
    }

    pub(super) fn view(&self, enabled: bool, reveal: &Reveal) -> Element<'_, Edit> {
        self.scan_options_view(enabled, reveal, true)
    }

    pub(super) fn scan_options_view(
        &self,
        enabled: bool,
        reveal: &Reveal,
        show_sync: bool,
    ) -> Element<'_, Edit> {
        let legacy_size = !self.size_edited
            && self
                .options
                .max_file_bytes
                .is_some_and(FileSizeLimit::uses_previous_boundary);
        let pattern_editor = |content, action: fn(text_editor::Action) -> Edit| {
            let editor = text_editor(content).height(60).size(13);
            if enabled {
                editor.on_action(action)
            } else {
                editor
            }
        };
        column![
            show_sync.then(|| text("Folder scan options").size(16)),
            (show_sync && enabled).then(|| {
                reveal.wrap(Setting::Knowledge(KnowledgeSetting::FolderSync),
                    pick_list(SyncMode::ALL, Some(self.options.sync), Edit::Sync).dismiss_on_escape())
            }),
            (show_sync && self.options.sync == SyncMode::Automatic).then(|| text("Automatic sync checks new and changed files within these reviewed rules. Approve the embedding destination separately.").size(13)),
            reveal.wrap(Setting::Knowledge(KnowledgeSetting::FolderRecursive), checkbox(self.options.recursive)
                .label("Include subfolders")
                .on_toggle_maybe(enabled.then_some(Edit::Recursive))),
            reveal.wrap(Setting::Knowledge(KnowledgeSetting::FolderGitIgnore), checkbox(self.options.respect_gitignore)
                .label("Respect the source root's .gitignore")
                .on_toggle_maybe(enabled.then_some(Edit::GitIgnore))),
            reveal.wrap(Setting::Knowledge(KnowledgeSetting::FolderPdf), checkbox(self.options.include_pdf)
                .label("Include PDFs with a text layer")
                .on_toggle_maybe(enabled.then_some(Edit::IncludePdf))),
            (!self.multiline).then(|| text(format!(
                "Enter a per-file limit up to {} MiB. New sources default to {} MiB. A scan accepts up to {} MiB in total.",
                MAX_CONFIGURED_BYTES / MIB as u64,
                NEW_SOURCE_DEFAULT / MIB as u64,
                pdf::MAX_FILE_BYTES / MIB,
            )).size(13)),
            if self.multiline {
                size_spinner(KnowledgeSetting::FolderFileLimit, &self.max_file_bytes, enabled, Edit::MaxFileBytes, reveal)
            } else {
                reveal.knowledge_input(KnowledgeSetting::FolderFileLimit, enabled, text_input("Default for this source", &self.max_file_bytes)
                    .on_input_maybe(enabled.then_some(Edit::MaxFileBytes)))
            },
            legacy_size.then(|| text("Re-enter a saved size to review it against the higher format limits.").size(13)),
            self.multiline.then(|| column![
                text("Include globs (one per line)").size(13),
                pattern_editor(&self.includes, Edit::IncludeLines),
                text("Exclude globs (one per line)").size(13),
                pattern_editor(&self.excludes, Edit::ExcludeLines),
            ].spacing(6)),
            (!self.multiline).then(|| reveal.knowledge_input(KnowledgeSetting::FolderIncludes, enabled, text_input("Include globs separated by semicolons", &self.includes.text())
                .on_input_maybe(enabled.then_some(Edit::Includes)))),
            (!self.multiline).then(|| reveal.knowledge_input(KnowledgeSetting::FolderExcludes, enabled, text_input("Exclude globs separated by semicolons", &self.excludes.text())
                .on_input_maybe(enabled.then_some(Edit::Excludes)))),
            text("Use relative paths with /, * for one segment and ** for subfolders. Review the resulting file list before applying these options.").size(13),
        ]
        .spacing(6)
        .into()
    }
}

fn edit_pattern(content: &mut text_editor::Content, action: text_editor::Action) {
    let before = action.is_edit().then(|| content.text());
    content.perform(action);
    if let Some(before) = before
        && content.text().chars().count() > MAX_PATTERN_CHARS
    {
        *content = text_editor::Content::with_text(&before);
    }
}

pub(super) fn file_size_help(path: &Path) -> String {
    let format = if pdf::is_pdf(path) { "PDF" } else { "UTF-8" };
    let cap = if pdf::is_pdf(path) {
        pdf::MAX_FILE_BYTES / MIB
    } else {
        500
    };
    format!("The {format} scan cap is {cap} MiB. Clearing the field restores the 50 MiB default.")
}

pub(super) fn format_advanced_limit(bytes: u64) -> String {
    let mib = MIB as u64;
    if bytes.is_multiple_of(mib) {
        (bytes / mib).to_string()
    } else {
        format!("{bytes} B")
    }
}

pub(super) fn advanced_file_size(input: &str) -> Result<u64, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(NEW_SOURCE_DEFAULT);
    }
    let invalid = || {
        format!(
            "Enter a size from 1 to {} MiB, or use B for an exact byte limit.",
            MAX_CONFIGURED_BYTES / MIB as u64
        )
    };
    let bytes = if let Some(bytes) = input.strip_suffix(['B', 'b']) {
        bytes.trim().parse::<u64>().map_err(|_| invalid())?
    } else {
        input
            .parse::<u64>()
            .ok()
            .and_then(|mib| mib.checked_mul(MIB as u64))
            .ok_or_else(invalid)?
    };
    validate_file_size(Some(bytes)).map_err(|_| invalid())?;
    Ok(bytes)
}

fn step_limit(input: &str, increase: bool) -> Option<String> {
    let value = advanced_file_size(input).ok()?;
    let step = MIB as u64;
    let next = if increase {
        value.saturating_add(step).min(MAX_CONFIGURED_BYTES)
    } else {
        value.saturating_sub(step).max(value.min(step))
    };
    (next != value).then(|| format_advanced_limit(next))
}

pub(super) fn size_spinner<'a, Message: Clone + 'a>(
    setting: KnowledgeSetting,
    input: &str,
    enabled: bool,
    edit: fn(String) -> Message,
    reveal: &Reveal,
) -> Element<'a, Message> {
    let step = |increase| {
        enabled
            .then(|| step_limit(input, increase))
            .flatten()
            .map(edit)
    };
    let setting = Setting::Knowledge(setting);
    column![
        text("Maximum size per file (MiB)").size(13),
        reveal.action(
            setting,
            row![
                reveal.input(
                    setting,
                    enabled,
                    text_input("50", input)
                        .width(128)
                        .size(13)
                        .on_input_maybe(enabled.then_some(edit))
                ),
                column![number_step("▴", step(true)), number_step("▾", step(false))].width(24),
            ]
            .width(152)
            .align_y(Alignment::Center)
        ),
        text("Add B to enter an exact byte limit.")
            .size(12)
            .style(text::secondary),
    ]
    .spacing(4)
    .into()
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
    use super::super::knowledge::source::{DEFAULT_TEXT_BYTES, MAX_TEXT_BYTES};
    use super::super::knowledge::{Store, folder};
    use super::super::{Configuration, Dialog, Message, Output, State};
    use super::*;
    use iced::futures::{StreamExt as _, executor::block_on};

    #[test]
    fn advanced_spinner_keeps_exact_bytes_and_respects_the_size_bounds() {
        assert_eq!(advanced_file_size("").unwrap(), NEW_SOURCE_DEFAULT);
        assert_eq!(advanced_file_size("51").unwrap(), 51 * MIB as u64);
        assert_eq!(advanced_file_size("4096 B").unwrap(), 4096);
        assert_eq!(format_advanced_limit(4096), "4096 B");
        assert_eq!(step_limit("", true).as_deref(), Some("51"));
        assert_eq!(step_limit("1", false), None);
        assert_eq!(step_limit("1024", true), None);
        assert_eq!(step_limit("4096 B", true).as_deref(), Some("1052672 B"));
        for invalid in ["0", "1025", "1.5", "18446744073709551615", "0 B"] {
            assert!(advanced_file_size(invalid).is_err(), "{invalid}");
        }
    }

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
        assert_eq!(
            store.effective_file_size_limit(&pdf_path),
            NEW_SOURCE_DEFAULT
        );
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
            Message::FileSize(format_advanced_limit(limit)),
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
            Message::FileSize(format_advanced_limit(pdf_limit)),
            Message::ApplyFileSize(0, pdf_path.clone()),
            Message::ConfigureFileSize(0, pdf_path.clone()),
        ] {
            send(&mut dialog, &mut draft, event);
        }
        assert_eq!(
            draft.stores[0].effective_file_size_limit(&pdf_path),
            pdf_limit
        );
        assert_eq!(dialog.file_size.as_ref().unwrap().input, "4");
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
        assert_eq!(store.effective_file_size_limit(&text), NEW_SOURCE_DEFAULT);
        store.set_file_size_limit(text.clone(), None).unwrap();
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
            Message::FileSize(format_advanced_limit(bytes)),
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
        let source_limit = NEW_SOURCE_DEFAULT as usize;
        std::fs::File::create(root.join("over-limit.rs"))
            .unwrap()
            .set_len(NEW_SOURCE_DEFAULT + 1)
            .unwrap();
        {
            use std::io::Write as _;
            let mut file = std::fs::File::create(root.join("at-limit.rs")).unwrap();
            let text = vec![b'\n'; MIB];
            for _ in 0..source_limit / MIB - 1 {
                file.write_all(&text).unwrap();
            }
            file.write_all(&text[..MIB - 1]).unwrap();
            file.write_all(b"x").unwrap();
        }
        let saved = State {
            stores: vec![Store::new("Source options").unwrap()],
            ..State::default()
        };
        let mut draft = saved.clone();
        let mut dialog = Dialog::new(profile.0.clone());
        let finish = |dialog: &mut Dialog, draft: &mut State, action| {
            let task = dialog.update(action, draft, &saved, false);
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
        finish(&mut dialog, &mut draft, Message::ReviewFolder);
        let review = dialog.folder_review.as_ref().unwrap();
        assert!(review.rows.iter().any(|row| {
            row.path == "at-limit.rs"
                && matches!(
                    row.status,
                    folder::Status::Ready { bytes, .. } if bytes == source_limit
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
        finish(&mut dialog, &mut draft, Message::ReviewFolder);
        let _ = dialog.update(Message::ApproveFolder, &mut draft, &saved, false);
        assert_eq!(draft.stores[0].source_count(), 1);

        let _ = dialog.update(Message::ConfigureFolder(0), &mut draft, &saved, false);
        assert_eq!(
            dialog
                .folder_advanced
                .as_ref()
                .unwrap()
                .draft
                .max_file_bytes,
            "15 B"
        );
        let _ = dialog.update(
            Message::EditFolderAdvanced(Edit::Includes("*.txt".into())),
            &mut draft,
            &saved,
            false,
        );
        let _ = dialog.update(Message::CancelFolderAdvanced, &mut draft, &saved, false);
        assert_eq!(draft.stores[0].folders[0].options.includes, ["**/*.rs"]);
        let _ = dialog.update(Message::ConfigureFolder(0), &mut draft, &saved, false);
        for edit in [Edit::Recursive(true), Edit::GitIgnore(false)] {
            let _ = dialog.update(Message::EditFolderAdvanced(edit), &mut draft, &saved, false);
        }
        finish(&mut dialog, &mut draft, Message::ReviewFolderAdvanced);
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
