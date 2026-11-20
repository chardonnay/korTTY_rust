//! Text selected by filename in the source shell of an Ask request.

use std::sync::Arc;

use crate::clipboard_policy::{ClipboardPolicy, RemoteEditorPolicy};
use crate::session_security::IsolationLevel;
use crate::workspace::terminal_ai::attachment::{Snapshot, Verification, remote};

use super::{Desktop, Message, Mode, Phase, Run, Task, Workspace, window};

#[derive(Default)]
pub(super) struct State {
    pub(super) filename: Option<String>,
    pub(super) snapshot: Option<Arc<Snapshot>>,
    pub(super) cancel: Option<remote::Cancel>,
}

pub(super) fn filename(selection: &str) -> Option<&str> {
    let selection = selection.trim();
    let quoted = selection
        .strip_prefix('"')
        .and_then(|name| name.strip_suffix('"'))
        .or_else(|| {
            selection
                .strip_prefix('\'')
                .and_then(|name| name.strip_suffix('\''))
        });
    let name = quoted.map_or(selection, str::trim);
    (!matches!(name, "" | "." | "..")
        && name.len() <= 255
        && !name.contains(['/', '\\', ':', '\'', '"'])
        && !name
            .chars()
            .any(|ch| ch.is_control() || !crate::session_journal::is_display_character(ch))
        && (quoted.is_some() || !name.chars().any(char::is_whitespace)))
    .then_some(name)
}

pub(super) fn require_allowed(policy: &ClipboardPolicy, remote: bool) -> Result<(), String> {
    if policy.problem.is_some()
        || !policy.ai_agent_allowed()
        || !policy.ai_memory.external_files_allowed()
        || policy.remote_editor == RemoteEditorPolicy::Deny
        || remote && !policy.file_transfer_allowed
        || !remote && policy.minimum_isolation == IsolationLevel::Sandbox
    {
        return Err("Managed policy blocks the selected file attachment.".into());
    }
    Ok(())
}

impl Workspace {
    pub(super) fn start_ask_review(
        &self,
        id: window::Id,
        run: &mut Run,
    ) -> Result<Task<Desktop>, String> {
        let Some(name) = run.ask_file.filename.take() else {
            return self.agent_turn(id, run);
        };
        if run.swarm || filename(&run.context) != Some(name.as_str()) {
            return self.agent_turn(id, run);
        }
        self.load_ask_attachment(id, run, name)
            .or_else(|error| self.ask_review_without_attachment(id, run, error))
    }

    fn load_ask_attachment(
        &self,
        id: window::Id,
        run: &mut Run,
        name: String,
    ) -> Result<Task<Desktop>, String> {
        let source = run.binding.live()?;
        let (_, pane) = self.current_agent_pane(source, Mode::Ask)?;
        let remote = pane.ssh.is_some();
        self.require_ask_attachment_source(run, remote)?;
        let limit = self
            .agent_transport(run)?
            .1
            .saturating_sub(run.context.chars().count() + run.goal.chars().count());
        if limit == 0 {
            return Err("The question and selection leave no room for an attachment.".into());
        }
        let redactor = Arc::new(run.redactor.clone());
        let task = if remote {
            let directory = self
                .remote_shell_file_context(source.session)
                .ok_or("The original SSH shell could not be verified.")?;
            let access = self
                .remote_attachment_access_for_pane(source.session)
                .ok_or("The original SFTP connection is unavailable.")?;
            let remote::Pending {
                reply,
                done,
                cancel,
            } = access.start_selected(directory, name, limit, redactor, || {
                require_allowed(&ClipboardPolicy::load(), true)
            })?;
            run.ask_file.cancel = Some(cancel);
            let epoch = run.next(Phase::AskAttachment, "Reading the selected SSH file…");
            Task::perform(
                async move {
                    let result = reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The SSH attachment worker stopped.".into()));
                    let _ = done.recv().await;
                    result
                },
                move |result| Desktop::TerminalAgent(id, Message::AskAttachment(epoch, result)),
            )
        } else {
            #[cfg(unix)]
            let path = pane
                .session
                .local_shell_directory()
                .map_err(|_| "The original shell directory is unavailable.")?;
            #[cfg(windows)]
            let path = pane.session.windows_terminal_file(&name)?;
            let cancel = run.cancel.clone();
            let epoch = run.next(Phase::AskAttachment, "Reading the selected local file…");
            Task::perform(
                async move {
                    let path = path
                        .recv()
                        .await
                        .map_err(|_| "The original shell closed.")?
                        .map_err(|_| "The original shell could not resolve the selected file.")?;
                    if cancel.requested() {
                        return Err("The selected file read was cancelled.".into());
                    }
                    require_allowed(&ClipboardPolicy::load(), false)?;
                    #[cfg(unix)]
                    let path = path.join(name);
                    Snapshot::read_bounded(&path, &redactor, limit).map(Arc::new)
                },
                move |result| Desktop::TerminalAgent(id, Message::AskAttachment(epoch, result)),
            )
        };
        Ok(task)
    }

    fn require_ask_attachment_source(&self, run: &Run, remote: bool) -> Result<(), String> {
        let (_, pane) = self.current_agent_pane(run.binding.live()?, Mode::Ask)?;
        require_allowed(&self.clipboard.policy, remote)?;
        require_allowed(&ClipboardPolicy::load(), remote)?;
        if !remote
            && (pane.session.sandbox_backend().is_some()
                || pane
                    .local_security
                    .as_ref()
                    .is_some_and(|settings| settings.isolation == IsolationLevel::Sandbox))
        {
            return Err("Reading files from a sandboxed shell is unavailable.".into());
        }
        Ok(())
    }

    pub(super) fn finish_ask_attachment(
        &self,
        id: window::Id,
        run: &mut Run,
        result: Result<Arc<Snapshot>, String>,
    ) -> Result<Task<Desktop>, String> {
        run.ask_file.cancel = None;
        let source = run.binding.live()?;
        self.current_agent_pane(source, Mode::Ask)?;
        match result {
            Ok(snapshot) => {
                let remote = snapshot.is_remote();
                if let Err(error) = self.require_ask_attachment_source(run, remote) {
                    return self.ask_review_without_attachment(id, run, error);
                }
                let access = remote
                    .then(|| self.remote_attachment_access_for_pane(source.session))
                    .flatten();
                if !snapshot.matches_source(access.as_ref()) {
                    return self.ask_review_without_attachment(
                        id,
                        run,
                        "The original attachment source changed.".into(),
                    );
                }
                run.ask_file.snapshot = Some(snapshot);
                self.agent_turn(id, run)
                    .or_else(|error| self.ask_review_without_attachment(id, run, error))
            }
            Err(error) => self.ask_review_without_attachment(id, run, error),
        }
    }

    fn ask_review_without_attachment(
        &self,
        id: window::Id,
        run: &mut Run,
        error: String,
    ) -> Result<Task<Desktop>, String> {
        run.ask_file.snapshot = None;
        let task = self.agent_turn(id, run)?;
        run.notice = format!("Selected file skipped. {error}");
        Ok(task)
    }

    pub(super) fn verify_ask_attachment(
        &self,
        run: &mut Run,
    ) -> Result<Option<Verification>, String> {
        let Some(snapshot) = run.ask_file.snapshot.clone() else {
            return Ok(None);
        };
        let remote = snapshot.is_remote();
        self.require_ask_attachment_source(run, remote)?;
        let source = run.binding.live()?;
        let access = remote
            .then(|| self.remote_attachment_access_for_pane(source.session))
            .flatten();
        let (verification, cancel) = snapshot.prepare_verification(access)?;
        run.ask_file.cancel = cancel;
        Ok(Some(verification))
    }
}

#[cfg(test)]
mod tests {
    use super::filename;

    #[test]
    fn ask_attachment_requires_one_filename_from_the_selection() {
        for (source, expected) in [
            ("notes.txt", "notes.txt"),
            (" 'project notes.txt' ", "project notes.txt"),
            ("\"Grüße.txt\"", "Grüße.txt"),
        ] {
            assert_eq!(filename(source), Some(expected));
        }
        for source in [
            "",
            ".",
            "..",
            "two words",
            "../notes",
            "/file",
            r"C:file.txt",
            "a\nb",
            "'mismatched\"",
        ] {
            assert_eq!(filename(source), None);
        }
    }
}
