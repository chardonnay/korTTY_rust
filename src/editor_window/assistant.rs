//! Adapts the native file draft to the shared reviewed AI workflow.

use std::sync::Arc;

use iced::widget::text_editor;

use crate::ai_manager::ProfileRef;
use crate::clipboard_policy::{ClipboardPolicy, RemoteEditorPolicy};
use crate::snippet_window::ai_assist::{Action, Change, Editor, Outcome, Recipe, Snapshot, Target};
use crate::terminal_logging::RedactionSecrets;
use crate::workspace::SshTarget;

use super::{EditorWindow, Language, source_selection};

#[derive(Clone, Default)]
pub(crate) struct AiOrigin {
    pub(crate) profile: Option<ProfileRef>,
    pub(crate) target: Option<SshTarget>,
    pub(crate) secrets: Option<RedactionSecrets>,
    pub(crate) incognito: bool,
    pub(crate) local_terminal: bool,
}

impl AiOrigin {
    pub(crate) fn authorize(&self, policy: &ClipboardPolicy) -> Result<(), String> {
        if policy.problem.is_some()
            || (self.incognito && !policy.incognito_allowed)
            || (self.local_terminal
                && (policy.remote_editor == RemoteEditorPolicy::Deny
                    || policy.minimum_isolation
                        == crate::session_security::IsolationLevel::Sandbox))
            || self.target.as_ref().is_some_and(|target| {
                !policy.file_transfer_allowed
                    || policy.remote_editor == RemoteEditorPolicy::Deny
                    || !target.workflow_route_allowed(policy)
            })
        {
            return Err("Managed policy blocks this file's AI workflow.".into());
        }
        Ok(())
    }
}

impl EditorWindow {
    pub(crate) fn file_ai_available(&self) -> bool {
        self.available()
            && self.document.is_some()
            && self
                .remote_editor()
                .is_none_or(|editor| self.ai_origin.target.is_some() && !editor.is_closed())
    }

    pub(crate) fn file_ai_revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn file_ai_revision_token(&self) -> std::sync::Weak<()> {
        Arc::downgrade(&self.revision_token)
    }

    fn replace_reviewed_text(&mut self, value: &str) -> Result<(), String> {
        if !self.file_ai_available() || !self.writable() {
            return Err("The file is read-only or no longer available.".into());
        }
        let replacement = text_editor::Content::with_text(value);
        if replacement.text() != value || value.len() > super::MAX_FILE_BYTES {
            return Err("The file editor cannot represent the exact reviewed text.".into());
        }
        self.remember_edit(None);
        self.replace_content(replacement);
        self.endings.update_from_text(value);
        self.search.invalidate_scope();
        self.mark_edited();
        self.dirty = self
            .document
            .as_ref()
            .is_none_or(|document| &*document.original != value);
        self.status = Some("Reviewed AI change is in the editor.".into());
        Ok(())
    }
}

impl Editor for EditorWindow {
    fn assist_status(&mut self, error: String) {
        self.status = Some(error);
    }

    fn assist_snapshot(&self) -> Result<Snapshot, String> {
        if !self.file_ai_available() {
            return Err("Open an available file before using AI assistance.".into());
        }
        let (source, selection) = source_selection(&self.content)?;
        let path = self.source_path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("File");
        let language = match Language::detect(path, &source) {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::Shell => "shell",
            Language::Dockerfile => "dockerfile",
            Language::Sql => "sql",
            Language::Extended(language) => language,
            Language::Other => "text",
        };
        Ok(Snapshot::file(
            self.revision,
            name.to_owned(),
            language.to_owned(),
            source,
            selection,
            self.ai_origin.profile.clone(),
            self.endings.replacement_ending(),
        ))
    }

    fn last_ai_change(&self) -> Option<Arc<Change>> {
        self.last_ai_change.clone()
    }

    fn accept_ai_change(
        &mut self,
        before: Snapshot,
        recipe: Recipe,
        outcome: &Outcome,
    ) -> Result<(), String> {
        if !before.same_source(&self.assist_snapshot()?) {
            return Err(
                "The file changed. Review a new request before applying a proposal.".into(),
            );
        }
        let Some((Target::Source, diff)) = &outcome.change else {
            return Err("This result does not change the file.".into());
        };
        self.replace_reviewed_text(&outcome.text)?;
        let mut after = self.assist_snapshot()?;
        if let Some(selection) = &outcome.selection {
            after.selection = selection.clone();
        }
        self.last_ai_change = Some(Arc::new(Change {
            before,
            after,
            target: Target::Source,
            diff: diff.clone(),
            recipe,
        }));
        Ok(())
    }

    fn undo_ai_change(&mut self, change: &Arc<Change>) -> Result<(), String> {
        if self
            .last_ai_change
            .as_ref()
            .is_none_or(|last| !Arc::ptr_eq(last, change))
            || !change.undoable(&self.assist_snapshot()?)
        {
            return Err("The file changed after the reviewed AI edit.".into());
        }
        self.replace_reviewed_text(change.before.value(Target::Source))?;
        self.last_ai_change = None;
        Ok(())
    }

    fn supports_ai_action(&self, action: Action) -> bool {
        !matches!(action, Action::CorrectDescription | Action::MigrateScript)
            && (!action.edits_file() || self.writable())
    }

    fn assistant_title(&self) -> &'static str {
        "File AI assistance"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippet_window::ai_assist::Request;

    #[test]
    fn file_proposals_require_exact_source_and_leave_saving_to_the_editor() {
        let fixture = crate::snippets::tests::Profile::new();
        let path = fixture.path().with_file_name("fixture.sh");
        let original = "echo old\r\n";
        std::fs::write(&path, original).unwrap();
        let document = super::super::Document::local(path.clone(), Arc::from(original));
        let (mut editor, _) = EditorWindow::with_document(document, None, None);
        let before = editor.assist_snapshot().unwrap();
        let reviewed_revision = editor.file_ai_revision_token();
        let recipe = Recipe {
            action: Action::Improve,
            ..Default::default()
        };
        let request = Request::new(
            before.clone(),
            &recipe,
            None,
            "",
            2048,
            Arc::new(|text| Ok(text.to_owned())),
        )
        .unwrap();
        let outcome = request.parse(r#"{"text":"echo new\n"}"#).unwrap();
        assert_eq!(editor.assist_snapshot().unwrap().source.as_ref(), original);
        editor
            .accept_ai_change(before.clone(), recipe.clone(), &outcome)
            .unwrap();
        assert_eq!(reviewed_revision.strong_count(), 0);
        assert_eq!(
            editor.assist_snapshot().unwrap().source.as_ref(),
            "echo new\r\n"
        );
        assert!(editor.dirty);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(editor.accept_ai_change(before, recipe, &outcome).is_err());
        editor
            .undo_ai_change(&editor.last_ai_change().unwrap())
            .unwrap();
        assert_eq!(editor.assist_snapshot().unwrap().source.as_ref(), original);
        assert!(!editor.dirty);
    }
}
