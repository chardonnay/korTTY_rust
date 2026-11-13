//! Profile defaults and local usage estimates for interactive Swarm journals.

use std::io::{self, Read};
use std::path::Path;

use iced::widget::{button, column, row, text};
use iced::{Element, Task, window};

use crate::private_files::{Directory, FileStamp, require_single_link};
use crate::scheduler_window::{AutomationEstimate, SwarmJournalDraft, SwarmJournalEdit};
use crate::session_journal::automation::{Options, Retention};
use crate::session_journal::viewer::Source;

use super::{ClipboardPolicy, Desktop, Message, RunMessage, Workspace, snippet};

pub(super) const SOURCE: &str = "interactive-swarm";
const SETTINGS: &str = "swarm-journal-settings.json";
const MAX_SETTINGS_BYTES: u64 = 4096;

pub(super) struct State {
    pub(super) draft: SwarmJournalDraft,
    pub(super) links: Vec<Source>,
    estimate: Option<AutomationEstimate>,
    pending: bool,
    epoch: u64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            draft: SwarmJournalDraft::from_options(Some(&defaults())),
            links: Vec::new(),
            estimate: None,
            pending: false,
            epoch: 0,
        }
    }
}

impl State {
    pub(super) fn load_defaults(&mut self) -> io::Result<()> {
        let root = crate::profile::root()?;
        let options = load(&root)?;
        self.draft = SwarmJournalDraft::from_options(Some(&options));
        Ok(())
    }

    pub(super) fn save_defaults(&self) -> Result<bool, String> {
        let root = crate::profile::root().map_err(|error| error.to_string())?;
        let options = self.draft.build_options()?;
        save(&root, &options, || {
            if ClipboardPolicy::load().automation_journal_allowed()
                && crate::profile::root().is_ok_and(|current| current == root)
            {
                Ok(())
            } else {
                Err(io::Error::other(
                    "The Swarm journal policy or profile changed.",
                ))
            }
        })
        .map_err(|error| error.to_string())
    }

    pub(super) fn view(&self, targets: usize) -> Element<'_, Message> {
        let mut controls = column![
            self.draft.swarm_view().map(Message::Journal),
            row![
                button("Save journal defaults").on_press(Message::SaveJournalDefaults),
                self.draft.estimate_mode().map(|_| {
                    button("Estimate journal summary usage").on_press_maybe(
                        (targets != 0 && !self.pending).then_some(Message::EstimateJournal),
                    )
                }),
            ]
            .spacing(6)
            .wrap(),
        ]
        .spacing(6);
        if self.pending {
            controls = controls.push(text("Reading journal usage…").size(12));
        } else if let Some(estimate) = &self.estimate {
            for line in estimate.lines() {
                controls = controls.push(text(line).size(12));
            }
        }
        controls.into()
    }

    pub(super) fn edit(&mut self, edit: SwarmJournalEdit) {
        if !matches!(edit, SwarmJournalEdit::Input(_)) {
            self.draft.edit(edit);
            self.clear_estimate();
        }
    }

    pub(super) fn clear_estimate(&mut self) {
        self.epoch += 1;
        self.pending = false;
        self.estimate = None;
    }

    pub(super) fn finish_estimate(
        &mut self,
        epoch: u64,
        result: Result<AutomationEstimate, &'static str>,
    ) {
        if self.pending && epoch == self.epoch {
            self.pending = false;
            self.estimate = Some(result.unwrap_or_else(|_| AutomationEstimate::worker_stopped()));
        }
    }

    pub(super) fn estimate(
        &mut self,
        workspace: &Workspace,
        id: window::Id,
        targets: usize,
    ) -> Task<Desktop> {
        let Some(mode) = self
            .draft
            .estimate_mode()
            .filter(|_| targets != 0 && !self.pending)
        else {
            return Task::none();
        };
        let profile = workspace
            .journal_summaries_allowed()
            .then(|| workspace.configured_journal_ai(false))
            .flatten();
        let Some((profile, root)) = profile.zip(workspace.journal_directory().ok()) else {
            self.estimate = Some(AutomationEstimate::unavailable());
            return Task::none();
        };
        self.epoch += 1;
        self.pending = true;
        let epoch = self.epoch;
        snippet::work(
            "kortty-swarm-journal-estimate",
            move || {
                let estimate = if ClipboardPolicy::load().automation_journal_allowed() {
                    let (model, budget) = profile.usage_estimate_profile();
                    AutomationEstimate::swarm(&root, SOURCE, targets, mode, model, budget)
                } else {
                    AutomationEstimate::unavailable()
                };
                Ok(estimate)
            },
            move |result| {
                Desktop::TerminalAgent(
                    id,
                    RunMessage::Swarm(Message::JournalEstimate(epoch, result)),
                )
            },
        )
    }
}

fn defaults() -> Options {
    Options {
        retention: Retention::Forever,
        deduplicate: false,
        ..Options::default()
    }
}

fn load(root: &Path) -> io::Result<Options> {
    let file = match Directory::open(root, false)
        .and_then(|directory| directory.open_private_file(SETTINGS))
    {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(defaults()),
        result => result?,
    };
    require_single_link(&file)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_SETTINGS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    stamp.require_unchanged(&file)?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(io::Error::other(
            "The Swarm journal defaults exceed their size limit.",
        ));
    }
    let mut options: Options = serde_json::from_slice(&bytes)?;
    options.validate()?;
    options.capture_input = false;
    Ok(options)
}

fn save(root: &Path, options: &Options, check: impl Fn() -> io::Result<()>) -> io::Result<bool> {
    options.validate()?;
    check()?;
    let directory = Directory::open(root, true)?;
    let lock = directory.open_lock(".swarm-journal-settings.lock")?;
    require_single_link(&lock)?;
    lock.try_lock().map_err(io::Error::other)?;
    directory.replace_with(
        SETTINGS,
        |file| serde_json::to_writer(file, options).map_err(io::Error::other),
        check,
    )?;
    Ok(directory.sync().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_journal::automation::{Closing, Keep};

    #[test]
    fn swarm_journal_defaults_persist_and_recheck_policy_before_publication() -> io::Result<()> {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize()?;
        assert_eq!(load(&root)?, defaults());
        let options = Options {
            enabled: true,
            closing: Closing::Failures,
            keep: Keep::Failures,
            retention: Retention::Days { days: 30 },
            max_runs: 20,
            max_storage_mib: 512,
            deduplicate: true,
            ..Options::default()
        };
        let _ = save(&root, &options, || Ok(()))?;
        assert_eq!(load(&root)?, options);
        let attempt = std::cell::Cell::new(0);
        assert!(
            save(&root, &defaults(), || {
                attempt.set(attempt.get() + 1);
                if attempt.get() == 1 {
                    Ok(())
                } else {
                    Err(io::ErrorKind::PermissionDenied.into())
                }
            })
            .is_err()
        );
        assert_eq!(load(&root)?, options);
        Ok(())
    }
}
