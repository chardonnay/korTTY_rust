//! Local saved Swarm history and a fresh, reviewed continuation.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};
use kortty_jobs::{
    MAX_SWARM_QUESTION_BYTES, MAX_SWARM_TARGET_DETAIL_BYTES, RunOutcome, SwarmReport,
    SwarmTargetReport,
};

use crate::ai_manager::{Configuration, ProfileRef};
use crate::terminal_agent::swarm::headless::Review as RouteReview;
use crate::terminal_agent::swarm::report::{self, Format};
use crate::terminal_logging::{Redactor, Secrets};
use crate::workspace::saved_chats::swarm::{self as store, Record};

use super::{
    Choice, ClipboardPolicy, Desktop, FollowUpResult, Group, Key, Message, Mode, ProfileChoice,
    Queue, RunMessage, Status, SwarmMode, Target, Workspace, headless, mask, require_swarm_policy,
    snippet, swarm_profile_entries,
};

#[cfg(test)]
mod tests;

pub(super) struct Conversation {
    pub(super) root: PathBuf,
    pub(super) record: Record,
    pub(super) profile_selection: ProfileRef,
    pub(super) loading: bool,
    pub(super) review: Option<Reviewed>,
    pub(super) active: Option<Active>,
    pub(super) saving: bool,
    review_epoch: u64,
    search: String,
    search_result: usize,
}

pub(super) struct Initial {
    pub(super) profile: ProfileRef,
    pub(super) profile_name: String,
    pub(super) mode: SwarmMode,
    pub(super) redactor: Redactor,
    pub(super) turn: store::Turn,
}

impl Initial {
    pub(super) fn save(self, root: &Path, title: Option<String>) -> Result<Record, &'static str> {
        let check = || {
            let policy = ClipboardPolicy::load();
            require_swarm_policy(&policy, self.mode).map_err(io::Error::other)?;
            if policy.ai_chat_allowed
                && self.redactor.uses_redactions(&policy.terminal_logging)
                && crate::profile::root().is_ok_and(|current| current == root)
                && Configuration::load().is_ok_and(|current| current.enabled())
            {
                Ok(())
            } else {
                Err(io::Error::other("Saved AI chat settings changed."))
            }
        };
        match title {
            Some(title) => store::save_interactive_named(
                root, self.profile, &self.profile_name, title, self.turn, check,
            ),
            None => store::save_interactive(root, self.profile, &self.profile_name, self.turn, check),
        }
        .map_err(|_| "Could not save the Swarm conversation. Check the current policy and available storage.")
    }
}

pub(super) struct Draft {
    previous: Option<Record>,
    profile: ProfileRef,
    profile_name: String,
    profile_revision: [u8; 32],
    mode: SwarmMode,
    redaction_policy: crate::terminal_logging::Policy,
    routes: Vec<(headless::Source, store::SavedRoute)>,
}

pub(super) struct Rename {
    previous: Record,
    redaction_policy: crate::terminal_logging::Policy,
    vault: Weak<()>,
}

impl Rename {
    pub(super) fn save(self, root: &Path, title: String) -> Result<Record, &'static str> {
        let check = || {
            let policy = ClipboardPolicy::load();
            if policy.ai_swarm_allowed()
                && policy.ai_chat_allowed
                && self
                    .redaction_policy
                    .equivalent_redactions(&policy.terminal_logging)
                && self.vault.strong_count() != 0
                && crate::profile::root().is_ok_and(|current| current == root)
                && Configuration::load().is_ok_and(|current| current.enabled())
            {
                Ok(())
            } else {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }
        };
        store::rename(root, &self.previous, title, check).map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound =>
                "The saved Swarm conversation changed. Close and reopen it before renaming.",
            io::ErrorKind::PermissionDenied => "Saved AI chat settings changed. Review the title again.",
            _ => "Could not rename the saved Swarm conversation. Check the title and profile storage.",
        })
    }
}

impl Draft {
    pub(super) fn save(self, root: &Path, title: String) -> Result<Record, &'static str> {
        let routes = self.routes.iter().map(|(_, route)| route.clone()).collect();
        let check = || {
            let policy = ClipboardPolicy::load();
            require_swarm_policy(&policy, self.mode).map_err(io::Error::other)?;
            if policy.ai_chat_allowed
                && self
                    .redaction_policy
                    .equivalent_redactions(&policy.terminal_logging)
                && crate::profile::root().is_ok_and(|current| current == root)
                && Configuration::load()
                    .ok()
                    .filter(Configuration::enabled)
                    .and_then(|config| config.agent_profile_reference_revision(&self.profile))
                    == Some(self.profile_revision)
                && self
                    .routes
                    .iter()
                    .all(|(source, _)| source.route_allowed(&policy) && source.revision_current())
            {
                Ok(())
            } else {
                Err(io::Error::other(
                    "The Swarm profile, routes or managed policy changed.",
                ))
            }
        };
        match &self.previous {
            Some(previous) => store::save_named_draft_changes(
                root,
                previous,
                self.profile.clone(),
                &self.profile_name,
                title,
                routes,
                check,
            ),
            None => store::save_draft(
                root,
                self.profile.clone(),
                &self.profile_name,
                title,
                routes,
                check,
            ),
        }
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists | io::ErrorKind::NotFound if self.previous.is_some() => {
                "The saved Swarm draft changed or was removed. Refresh Saved Chats."
            }
            _ => "Could not save the Swarm draft. Check the current targets, profile and storage.",
        })
    }
}

pub(super) struct Reviewed {
    pub(super) epoch: u64,
    pub(super) question: String,
    mode: SwarmMode,
    profile: ProfileChoice,
    profile_name: String,
    profile_display: String,
    profile_revision: [u8; 32],
    redactor: Redactor,
    targets: Vec<ReviewedTarget>,
}

struct ReviewedTarget {
    source: headless::Source,
    route: Arc<RouteReview>,
}

pub(super) struct Active {
    pub(super) reviewed: Arc<Reviewed>,
    conclusions: HashMap<Key, String>,
}

impl Conversation {
    pub(super) fn new(root: PathBuf, record: Record, profile: Option<&ProfileChoice>) -> Self {
        Self {
            root,
            profile_selection: profile.map_or_else(
                || record.profile_id.clone(),
                |profile| profile.reference.clone(),
            ),
            record,
            loading: true,
            review: None,
            active: None,
            saving: false,
            review_epoch: 0,
            search: String::new(),
            search_result: 0,
        }
    }

    pub(super) fn set_search(&mut self, search: String) {
        if search.len() <= 256 {
            self.search = search;
            self.search_result = 0;
        }
    }

    pub(super) fn move_search(&mut self, forward: bool) {
        let count = self.search_matches().len();
        if count != 0 {
            let current = self.search_result.min(count - 1);
            self.search_result = if forward {
                (current + 1) % count
            } else {
                (current + count - 1) % count
            };
        }
    }

    fn search_matches(&self) -> Vec<usize> {
        let query = self.search.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let Ok(pattern) = regex::RegexBuilder::new(&regex::escape(query))
            .case_insensitive(true)
            .build()
        else {
            return Vec::new();
        };
        self.record
            .turns
            .iter()
            .enumerate()
            .filter_map(|(index, turn)| {
                (pattern.is_match(&turn.question)
                    || turn
                        .comparison
                        .as_deref()
                        .is_some_and(|text| pattern.is_match(text))
                    || turn.targets.iter().any(|target| {
                        pattern.is_match(&target.label) || pattern.is_match(&target.detail)
                    })
                    || turn.routes.iter().flatten().any(|route| {
                        route
                            .directory
                            .iter()
                            .chain(&route.group)
                            .any(|text| pattern.is_match(text))
                    }))
                .then_some(index)
            })
            .collect()
    }

    pub(super) fn history(&self) -> Element<'_, Message> {
        if self.record.is_draft() {
            return column![
                text(self.record.title()).size(22),
                text(format!(
                    "Saved AI Swarm draft · {}",
                    self.record.profile_name
                ))
                .size(13),
            ]
            .spacing(6)
            .into();
        }
        let searching = !self.search.trim().is_empty();
        let matches = self.search_matches();
        let current = self.search_result.min(matches.len().saturating_sub(1));
        let mut history = column![].spacing(8);
        for (index, turn) in self.record.turns.iter().enumerate() {
            if searching && matches.get(current) != Some(&index) {
                continue;
            }
            history = history.push(text(&turn.question).size(15));
            for target in &turn.targets {
                history = history
                    .push(
                        text(format!(
                            "{} · {}",
                            target.label,
                            outcome_label(target.outcome)
                        ))
                        .size(13),
                    )
                    .push(text(&target.detail).size(13));
                if let Some(route) = turn
                    .routes
                    .iter()
                    .flatten()
                    .find(|route| route.target == target.target)
                {
                    if let Some(group) = &route.group {
                        history =
                            history.push(text(format!("Connection group · {group}")).size(12));
                    }
                    if let Some(directory) = &route.directory {
                        history =
                            history.push(text(format!("Remote directory · {directory}")).size(12));
                    }
                }
            }
            if let Some(comparison) = &turn.comparison {
                history = history
                    .push(text("AI comparison").size(14))
                    .push(text(comparison).font(iced::Font::MONOSPACE).size(13));
            }
        }
        column![
            text(self.record.title()).size(22),
            text(format!("Saved AI Swarm conversation · {}", self.record.profile_name)).size(13),
            row![
                text_input("Search conversation", &self.search).on_input(Message::SearchHistory),
                button("Previous").on_press_maybe((matches.len() > 1).then_some(Message::MoveHistorySearch(false))),
                button("Next").on_press_maybe((matches.len() > 1).then_some(Message::MoveHistorySearch(true))),
                searching.then(|| text(if matches.is_empty() {
                    "No matching turns".into()
                } else {
                    format!("{} of {} matching turns", current + 1, matches.len())
                }).size(13)),
            ].spacing(6).wrap(),
            scrollable(history).height(Length::Fixed(165.0)),
            text("Saved history stays here. A follow-up sends a fresh question with the current target context after you review it.").size(13),
            text("A completed follow-up saves the masked question and final target summaries.").size(13),
        ].spacing(6).into()
    }

    pub(super) fn export(&self, format: Format) -> String {
        let markdown = format == Format::Markdown;
        let mut result = if markdown {
            String::from("# AI Swarm conversation\n\n")
        } else {
            String::from("AI Swarm conversation\n\n")
        };
        for (index, turn) in self.record.turns.iter().enumerate() {
            let heading = if markdown { "## Question" } else { "Question" };
            let _ = writeln!(result, "{heading} {}\n", index + 1);
            report::append_body(&mut result, format, &turn.question);
            for target in &turn.targets {
                let details = format!(
                    "{} · {}\n{}",
                    target.label,
                    outcome_label(target.outcome),
                    target.detail
                );
                report::append_body(&mut result, format, &details);
            }
            if let Some(comparison) = &turn.comparison {
                let heading = if markdown {
                    "### AI comparison"
                } else {
                    "AI comparison"
                };
                let _ = writeln!(result, "{heading}\n");
                report::append_body(&mut result, format, comparison);
            }
        }
        result
    }

    pub(super) fn catalog_result(
        &mut self,
        choices: &mut Vec<Choice>,
        selected: &HashSet<Key>,
        sources: Vec<headless::Source>,
    ) {
        let mut pending = sources
            .into_iter()
            .map(|source| (source.reference.id, source))
            .collect::<Vec<_>>();
        for choice in choices.iter_mut() {
            if let Key::Saved(id) = choice.target.key {
                if let Some(index) = pending.iter().position(|(candidate, _)| *candidate == id) {
                    let (_, source) = pending.remove(index);
                    choice.target.label = format!("Saved SSH connection {id}");
                    choice.saved = Some(source);
                    choice.directory.get_or_insert_with(|| {
                        self.record
                            .target_route(id)
                            .and_then(|route| route.directory.clone())
                            .unwrap_or_default()
                    });
                } else {
                    choice.target.label = format!("Saved SSH connection {id} · unavailable");
                    choice.saved = None;
                    choice.directory = None;
                }
            }
        }
        choices.retain(|choice| choice.saved.is_some() || selected.contains(&choice.target.key));
        choices.extend(
            pending
                .into_iter()
                .map(|(id, source)| saved_choice(id, Some(source))),
        );
    }
}

pub(super) fn check_export(source: Option<&(PathBuf, Record)>) -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    if policy.problem.is_some() {
        return Err(io::Error::other("Managed policy is unavailable."));
    }
    if let Some((root, record)) = source
        && (!policy.ai_chat_allowed
            || !policy.ai_swarm_allowed()
            || !crate::profile::root().is_ok_and(|current| &current == root)
            || !store::load(root, &record.id).is_ok_and(|current| current.same_revision(record)))
    {
        return Err(io::Error::other(
            "The saved conversation or its policy changed.",
        ));
    }
    Ok(())
}

pub(super) fn saved_choice(id: u64, source: Option<headless::Source>) -> Choice {
    let label = if source.is_some() {
        format!("Saved SSH connection {id}")
    } else {
        format!("Saved SSH connection {id} · unavailable")
    };
    Choice {
        target: Target {
            key: Key::Saved(id),
            label,
            private: false,
        },
        directory: source.as_ref().map(|_| String::new()),
        saved: source,
    }
}

impl Reviewed {
    pub(super) fn view(&self, first_run: bool) -> Element<'_, Message> {
        let mut view = column![
            text(if first_run {
                "Review the first run"
            } else {
                "Review the fresh follow-up"
            })
            .size(18),
            text(format!("AI profile · {}", self.profile_display)).size(13),
            text(format!("Workflow · {}", self.mode)).size(13),
            text("Masked question"),
            text(&self.question),
        ]
        .spacing(6);
        for item in &self.targets {
            let target = &item.source.reference;
            view = view
                .push(text(format!(
                    "Saved connection {} · {}",
                    target.id,
                    target.target.label()
                )))
                .push(text(format!("Remote directory · {}", item.route.directory())).size(13));
            if let Some(jump) = &target.jump {
                view = view.push(text(format!("Jump host · {}", jump.label())).size(13));
            }
            for (endpoint, fingerprint) in item.route.fingerprints() {
                view = view.push(text(format!("{endpoint} · {fingerprint}")).size(12));
            }
        }
        view.push(
            button(if first_run {
                "Start reviewed run"
            } else {
                "Start reviewed follow-up"
            })
            .on_press(Message::StartFollowUp(self.epoch)),
        )
        .into()
    }

    pub(super) fn source(&self, key: Key) -> Option<&headless::Source> {
        let Key::Saved(id) = key else { return None };
        self.targets
            .iter()
            .find(|target| target.source.reference.id == id)
            .map(|target| &target.source)
    }

    pub(super) fn check(&self, root: &Path) -> io::Result<()> {
        let policy = ClipboardPolicy::load();
        require_swarm_policy(&policy, self.mode).map_err(io::Error::other)?;
        if !policy.ai_chat_allowed || !crate::profile::root().is_ok_and(|current| current == root) {
            return Err(io::Error::other(
                "Saved AI chats are unavailable for the current profile.",
            ));
        }
        if !self
            .targets
            .iter()
            .all(|target| target.source.route_allowed(&policy) && target.source.revision_current())
        {
            return Err(io::Error::other(
                "The reviewed model profile, SSH route, host keys, or managed policy changed.",
            ));
        }
        Ok(())
    }
}

impl Active {
    pub(super) fn reset_target(&mut self, key: Key) {
        self.conclusions.remove(&key);
    }

    pub(super) fn conclusion(&mut self, key: Key, result: &str) {
        let detail = plain_line(result, MAX_SWARM_TARGET_DETAIL_BYTES);
        if !detail.is_empty() {
            self.conclusions.insert(key, detail);
        }
    }

    fn report(&self, queue: &Queue) -> Option<store::Turn> {
        if self.conclusions.is_empty() || !queue.complete() {
            return None;
        }
        let targets = queue
            .rows()
            .iter()
            .map(|row| {
                let Key::Saved(target) = row.target.key else {
                    return None;
                };
                let outcome = match row.status {
                    Status::Succeeded => RunOutcome::Success,
                    Status::Failed => RunOutcome::Failed { exit_code: None },
                    Status::Blocked => RunOutcome::Blocked,
                    _ => return None,
                };
                Some(SwarmTargetReport {
                    target,
                    label: format!("Saved SSH connection {target}"),
                    outcome,
                    detail: self
                        .conclusions
                        .get(&row.target.key)
                        .cloned()
                        .unwrap_or_else(|| row.status.label().into()),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let mut turn = store::Turn::from(SwarmReport {
            question: self.reviewed.question.clone(),
            targets,
            comparison: None,
        });
        turn.routes = Some(
            self.reviewed
                .targets
                .iter()
                .map(|target| {
                    let connection = target.source.reference.id;
                    store::SavedRoute {
                        target: connection,
                        connection,
                        directory: unmasked_metadata(
                            &self.reviewed.redactor,
                            target.route.directory(),
                            4096,
                        ),
                        group: target.source.group.as_deref().and_then(|value| {
                            unmasked_metadata(
                                &self.reviewed.redactor,
                                value,
                                crate::connection_store::MAX_GROUP_PATH_BYTES,
                            )
                        }),
                    }
                })
                .collect(),
        );
        Some(turn)
    }
}

impl Workspace {
    pub(super) fn swarm_rename_snapshot(
        &self,
        group: &Group,
        root: &Path,
        title: &str,
    ) -> Result<Rename, String> {
        let chat = group
            .conversation
            .as_ref()
            .filter(|chat| !chat.loading && !chat.record.is_draft() && chat.root == root)
            .ok_or("Open a completed saved Swarm conversation before renaming it.")?;
        let policy = ClipboardPolicy::load();
        let redaction_policy = &self.clipboard.policy.terminal_logging;
        if !self.swarm_chat_access_allowed()
            || !policy.ai_swarm_allowed()
            || !policy.ai_chat_allowed
            || !redaction_policy.equivalent_redactions(&policy.terminal_logging)
        {
            return Err("Saved AI chat settings changed. Review the title again.".into());
        }
        let credential = group
            .profile
            .as_ref()
            .zip(self.ai_chat_configuration())
            .and_then(|(profile, config)| profile.transport(&config).ok())
            .and_then(|(transport, _)| transport.completion_secret());
        if self.mask_saved_title(title)? != title
            || super::request_review::mask_https_text(
                title,
                credential.as_deref().map(String::as_str),
                self.vault.as_ref(),
            )? != title
        {
            return Err("Choose a title that contains no private or redacted text.".into());
        }
        Ok(Rename {
            previous: chat.record.clone(),
            redaction_policy: redaction_policy.clone(),
            vault: self.vault_change_revision(),
        })
    }

    pub(super) fn swarm_draft_snapshot(
        &self,
        group: &Group,
        root: &Path,
        title: &str,
    ) -> Result<Draft, String> {
        let unavailable = "Saved AI Swarm drafts are unavailable for the current settings.";
        let previous = match &group.conversation {
            Some(chat) if chat.record.is_draft() && !chat.loading && chat.root == root => {
                Some(chat.record.clone())
            }
            Some(_) => return Err(unavailable.into()),
            None => None,
        };
        let policy = self.swarm_policy_for(group.mode)?;
        let local = &self.clipboard.policy;
        if group.mode == SwarmMode::Snippet
            || !policy.ai_chat_allowed
            || !local
                .terminal_logging
                .equivalent_redactions(&policy.terminal_logging)
        {
            return Err(unavailable.into());
        }
        let profile = group
            .profile
            .as_ref()
            .ok_or("Choose a current AI profile.")?;
        let config = self.ai_chat_configuration().ok_or(unavailable)?;
        let (transport, _) = profile.transport(&config)?;
        let profile_revision = config
            .agent_profile_reference_revision(&profile.reference)
            .ok_or("The selected AI profile is unavailable.")?;
        let profile_name = swarm_profile_entries(&config)
            .find_map(|(id, name, _, _)| (id == profile.id).then_some(name))
            .ok_or("The selected AI profile is unavailable.")?;
        let credential = transport
            .completion_secret()
            .filter(|_| transport.is_remote());
        let secrets = Secrets::default()
            .seal()
            .map_err(|error| error.to_string())?;
        let redactor = Redactor::for_journal(secrets, local.terminal_logging.clone());
        let masked = |value: &str| -> Result<String, String> {
            let text = super::request_review::mask_https_text(
                value,
                credential.as_deref().map(String::as_str),
                self.vault.as_ref(),
            )?;
            mask::prompt(&redactor, &text)
        };
        if masked(title)? != title {
            return Err("Choose a title that contains no private or redacted text.".into());
        }
        let mut routes = Vec::new();
        let mut connections = HashSet::new();
        for choice in group
            .choices
            .iter()
            .filter(|choice| group.selected.contains(&choice.target.key))
        {
            let source = group
                .catalog_source(choice)
                .filter(|source| {
                    !choice.target.private
                        && source.route_allowed(local)
                        && source.route_allowed(&policy)
                })
                .ok_or("Select available saved SSH connections for the draft.")?;
            let pane_redactor = if let Some((session, stream)) = choice.target.key.pane() {
                let (_, pane, _) = self
                    .agent_pane(session)
                    .filter(|(_, pane, tab)| pane.stream_generation == stream && !tab.incognito)
                    .filter(|(_, pane, _)| {
                        pane.ssh.as_ref().is_some_and(|ssh| {
                            ssh.saved_id() == Some(source.reference.id)
                                && ssh.matches_endpoints(
                                    &source.reference.target,
                                    source.reference.jump.as_ref(),
                                )
                        })
                    })
                    .ok_or("A selected SSH pane changed. Refresh the targets before saving.")?;
                let redactor = self.agent_source_redactor(pane)?;
                if mask::prompt(&redactor, title)? != title {
                    return Err("Choose a title that contains no private or redacted text.".into());
                }
                Some(redactor)
            } else {
                None
            };
            let safe_metadata = |value: &str, limit| {
                if masked(value).ok().as_deref() != Some(value)
                    || pane_redactor.as_ref().is_some_and(|redactor| {
                        mask::prompt(redactor, value).ok().as_deref() != Some(value)
                    })
                {
                    return None;
                }
                unmasked_metadata(&redactor, value, limit)
            };
            let connection = source.reference.id;
            if !connections.insert(connection) {
                return Err("Choose each saved SSH connection only once for the draft.".into());
            }
            let directory =
                choice
                    .directory
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| {
                        value.starts_with('/').then(|| safe_metadata(value, 4096)).flatten()
                        .ok_or("Use an absolute remote directory without private or redacted text.")
                    })
                    .transpose()?;
            let group = source.group.as_deref().and_then(|value| {
                safe_metadata(value, crate::connection_store::MAX_GROUP_PATH_BYTES)
            });
            routes.push((
                source.clone(),
                store::SavedRoute {
                    target: connection,
                    connection,
                    directory,
                    group,
                },
            ));
        }
        if routes.is_empty() || routes.len() != group.selected.len() {
            return Err("Select available saved SSH connections for the draft.".into());
        }
        Ok(Draft {
            previous,
            profile: profile.reference.clone(),
            profile_name: plain_line(&masked(profile_name)?, 256),
            profile_revision,
            mode: group.mode,
            redaction_policy: local.terminal_logging.clone(),
            routes,
        })
    }

    pub(super) fn review_saved_swarm_follow_up(&self, group: &mut Group) -> Result<(), String> {
        let Some(chat) = group.conversation.as_mut() else {
            return Ok(());
        };
        chat.review = None;
        if chat.loading
            || group.goal.trim().is_empty()
            || !(1..=super::engine::MAX_TARGETS).contains(&group.selected.len())
        {
            return Err(
                "Enter a new question and select one to eight saved SSH connections.".into(),
            );
        }
        if !self.ai_chat_enabled() || group.mode == SwarmMode::Snippet {
            return Err(
                "Saved AI Swarm conversations are unavailable under the current settings.".into(),
            );
        }
        let policy = self.swarm_policy_for(group.mode)?;
        if !policy.ai_chat_allowed
            || !self
                .clipboard
                .policy
                .terminal_logging
                .equivalent_redactions(&policy.terminal_logging)
        {
            return Err("Saved chat policy or the managed redaction rules changed.".into());
        }
        let profile = group
            .profile
            .as_ref()
            .ok_or("Choose a current AI profile.")?;
        let config = self
            .ai_configuration()
            .ok_or("Choose a current AI profile.")?;
        let (transport, _) = profile.transport(&config)?;
        let (_, name, model, destination) = swarm_profile_entries(&config)
            .find(|entry| entry.0 == profile.id)
            .ok_or("The selected AI profile is unavailable.")?;
        let profile_revision = config
            .agent_profile_reference_revision(&profile.reference)
            .ok_or("The selected AI profile is unavailable.")?;
        if Configuration::load()
            .ok()
            .filter(|saved| saved.resolve_profile_reference(&profile.reference) == Some(profile.id))
            .and_then(|saved| saved.agent_profile_reference_revision(&profile.reference))
            != Some(profile_revision)
        {
            return Err("The selected AI profile changed. Refresh and review it again.".into());
        }
        let mode = match group.mode {
            SwarmMode::Agent => Mode::Run,
            _ => Mode::Ask,
        };
        let trust = crate::ssh_connection::trust_store()
            .map_err(|_| "The SSH host-key store is unavailable.")?;
        let secrets = Secrets::default();
        let https_credential = transport
            .completion_secret()
            .filter(|_| transport.is_remote());
        if let Some(credential) = &https_credential {
            secrets.add(credential);
        }
        let selected = group
            .choices
            .iter()
            .filter(|choice| group.selected.contains(&choice.target.key));
        let mut targets = Vec::new();
        for choice in selected {
            let source = choice
                .saved
                .as_ref()
                .ok_or("Remove unavailable targets or refresh the saved connections.")?;
            let directory = choice
                .directory
                .as_deref()
                .filter(|directory| directory.starts_with('/'))
                .ok_or("Enter an absolute remote directory for every selected target.")?;
            let connection = self.saved_swarm_connection(source, mode, true)?;
            let (credentials, _) = self.saved_swarm_credentials(source, &connection)?;
            credentials.add_journal_secrets(&secrets);
            let route = RouteReview::capture(source.reference.clone(), directory.to_owned(), &trust)
                .map_err(|_| "Verify the absolute directories and the current pinned host keys for all selected SSH routes.")?;
            targets.push((source.clone(), Arc::new(route)));
        }
        let vault_revision = self
            .vault
            .as_ref()
            .and_then(|vault| vault.credential_revision());
        let redactor = Redactor::for_journal(
            secrets
                .seal()
                .map_err(|_| "Could not verify the saved SSH credential mask.")?,
            self.clipboard.policy.terminal_logging.clone(),
        );
        let mask = |value: &str| {
            let value = mask::prompt(&redactor, value)?;
            if profile.is_reviewed_outbound() {
                super::super::request_review::mask_https_text(
                    &value,
                    https_credential.as_deref().map(String::as_str),
                    self.vault.as_ref(),
                )
            } else {
                Ok(value)
            }
        };
        let question = mask(group.goal.trim())?;
        if question.len() > MAX_SWARM_QUESTION_BYTES
            || !crate::terminal_agent::valid_target_label(&question)
        {
            return Err("Enter a single-line follow-up question of at most 4 KiB.".into());
        }
        let profile_name = plain_line(&mask(name)?, 256);
        let profile_display = mask(&format!("{name} · {model} · {destination}"))?;
        let targets = targets
            .into_iter()
            .map(|(source, route)| {
                if mask(route.directory())? != route.directory() {
                    return Err(
                        "A selected remote directory contains text that requires masking.".into(),
                    );
                }
                Ok(ReviewedTarget {
                    source: source.bind_follow_up(
                        Arc::clone(&route),
                        profile.clone(),
                        profile_revision,
                        redactor.clone(),
                        vault_revision.clone(),
                    ),
                    route,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        chat.review_epoch += 1;
        chat.review = Some(Reviewed {
            epoch: chat.review_epoch,
            question,
            mode: group.mode,
            profile: profile.clone(),
            profile_name,
            profile_display,
            profile_revision,
            redactor,
            targets,
        });
        group.notice.clear();
        Ok(())
    }

    pub(super) fn start_saved_swarm_follow_up(
        &mut self,
        group: &mut Group,
        epoch: u64,
    ) -> Result<(), String> {
        let Some(chat) = group.conversation.as_mut() else {
            return Ok(());
        };
        if chat
            .review
            .as_ref()
            .is_none_or(|review| review.epoch != epoch)
        {
            return Ok(());
        }
        let review = chat.review.take().expect("current review");
        if !self.ai_chat_enabled()
            || !review
                .redactor
                .uses_redactions(&self.clipboard.policy.terminal_logging)
            || self
                .ai_configuration()
                .filter(|config| review.profile.transport(config).is_ok())
                .and_then(|config| {
                    config.agent_profile_reference_revision(&review.profile.reference)
                })
                != Some(review.profile_revision)
        {
            return Err(
                "The reviewed AI profile or managed rules changed. Review the follow-up again."
                    .into(),
            );
        }
        review
            .check(&chat.root)
            .map_err(|error| error.to_string())?;
        let current = store::load(&chat.root, &chat.record.id)
            .map_err(|_| "The saved Swarm conversation is no longer available.")?;
        if !chat.record.same_revision(&current) {
            return Err(
                "The saved Swarm conversation changed. Close and reopen it before continuing."
                    .into(),
            );
        }
        chat.active = Some(Active {
            reviewed: Arc::new(review),
            conclusions: HashMap::new(),
        });
        group.round += 1;
        group.goal.clear();
        group.journal_run = None;
        self.start_reviewed_swarm_queue(group);
        Ok(())
    }

    pub(super) fn save_completed_swarm_follow_up(
        &self,
        id: window::Id,
        group: &mut Group,
    ) -> Option<Task<Desktop>> {
        let chat = group.conversation.as_mut()?;
        let queue = group.queue.as_ref().filter(|queue| queue.complete())?;
        let active = chat.active.take()?;
        let report = active.report(queue)?;
        if group.closing
            || !self.ai_chat_enabled()
            || self.swarm_policy_for(active.reviewed.mode).is_err()
        {
            return None;
        }
        let reviewed = active.reviewed;
        let root = chat.root.clone();
        let previous = chat.record.clone();
        let round = group.round;
        chat.saving = true;
        Some(snippet::work(
            "kortty-swarm-conversation",
            move || {
                store::append(
                    &root,
                    &previous,
                    reviewed.profile.reference.clone(),
                    &reviewed.profile_name,
                    report,
                    || reviewed.check(&root),
                )
                .map_err(|_| "Could not append the reviewed follow-up. Reopen the conversation before trying again.")
            },
            move |result| {
                Desktop::TerminalAgent(
                    id,
                    RunMessage::Swarm(Message::FollowUpSaved(round, FollowUpResult(result))),
                )
            },
        ))
    }
}

pub(super) fn unmasked_metadata(redactor: &Redactor, value: &str, limit: usize) -> Option<String> {
    (!value.trim().is_empty()
        && value.len() <= limit
        && value
            .chars()
            .all(crate::session_journal::is_display_character)
        && !value.contains(['\n', '\t', '\u{2028}', '\u{2029}'])
        && mask::prompt(redactor, value).is_ok_and(|masked| masked == value))
    .then(|| value.to_owned())
}

pub(super) fn plain_line(value: &str, limit: usize) -> String {
    let mut line = String::new();
    for word in value.split_whitespace() {
        for character in word.chars() {
            if character.is_control()
                || matches!(character, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
            {
                continue;
            }
            if line.len() + character.len_utf8() > limit {
                return line.trim_end().into();
            }
            line.push(character);
        }
        if !line.is_empty() && line.len() < limit {
            line.push(' ');
        }
    }
    line.trim_end().into()
}

fn outcome_label(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Success => "Completed",
        RunOutcome::Blocked => "Blocked",
        RunOutcome::Failed { .. } => "Failed",
        RunOutcome::Cancelled => "Cancelled",
    }
}
