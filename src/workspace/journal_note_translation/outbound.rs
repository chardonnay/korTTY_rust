//! Exact, one-use HTTPS and CLI reviews for journal note translation.

use std::path::PathBuf;
use std::sync::{Arc, Weak};
use std::time::Instant;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Length};
use zeroize::Zeroizing;

use crate::ai_manager::{Configuration, JournalNoteProfile};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{self, Client, Error, Profile};
use crate::session_journal::{note_translation, topic_ai_settings};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::JournalQuestionSecrets;
use crate::workspace::{tab_titles::WindowSettings, terminal_ai::mask};

use super::Event;

pub(super) type Source = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

const CHANGED: &str =
    "The journal source, AI profile, or masking policy changed. Review the translation again.";

pub(super) fn allowed(policy: &ClipboardPolicy, profile: &Profile) -> bool {
    topic_ai_settings::allowed(policy)
        && (!profile.is_cli() || local_ai::cli::allowed(policy, false))
}

pub(super) struct Scope {
    pub(super) config: Arc<Configuration>,
    pub(super) profile: JournalNoteProfile,
    pub(super) policy: Policy,
    pub(super) preferences: topic_ai_settings::Preferences,
    pub(super) vault: JournalQuestionSecrets,
    pub(super) vault_change: Weak<()>,
    pub(super) credentials: Option<Weak<()>>,
}

pub(super) struct Authority {
    scope: Scope,
    root: PathBuf,
    credential: Option<Zeroizing<String>>,
    source: Source,
}

impl Authority {
    fn new(scope: Scope, source: Source) -> Result<Self, String> {
        let root = scope.config.saved_root().ok_or(CHANGED)?.to_owned();
        let credential = scope.profile.transport.completion_secret();
        let authority = Self {
            scope,
            root,
            credential,
            source,
        };
        authority.require_live()?;
        Ok(authority)
    }

    pub(super) fn require_live(&self) -> Result<(), String> {
        if self.scope.vault_change.strong_count() == 0
            || self
                .scope
                .credentials
                .as_ref()
                .is_some_and(|revision| revision.strong_count() == 0)
        {
            return Err(CHANGED.into());
        }
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        self.require_live()?;
        let installed = ClipboardPolicy::load();
        let transport = &self.scope.profile.transport;
        if !allowed(&installed, transport)
            || self.scope.policy.journal_directory()
                != installed.terminal_logging.journal_directory()
            || !self
                .scope
                .policy
                .equivalent_redactions(&installed.terminal_logging)
            || transport.completion_secret() != self.credential
            || !crate::profile::root().is_ok_and(|root| root == self.root)
        {
            return Err(CHANGED.into());
        }
        let window =
            WindowSettings::read(&self.root.join("window-settings.json")).map_err(|_| CHANGED)?;
        if !window.journal_ai.enabled || window.journal_ai != self.scope.preferences {
            return Err(CHANGED.into());
        }
        let saved = self.scope.config.reload_saved().map_err(|_| CHANGED)?;
        let profile = saved.journal_note_translation_profile().ok_or(CHANGED)??;
        if !profile_matches(&self.scope.profile, &profile) {
            return Err(CHANGED.into());
        }
        transport.require_current_policy_authority()?;
        (self.source)()
    }

    fn matches(&self, current: &Scope) -> bool {
        current.config.saved_root() == Some(self.root.as_path())
            && profile_matches(&self.scope.profile, &current.profile)
            && self.scope.preferences == current.preferences
            && self.scope.policy.journal_directory() == current.policy.journal_directory()
            && self.scope.policy.equivalent_redactions(&current.policy)
            && self.scope.vault_change.ptr_eq(&current.vault_change)
            && match (&self.scope.credentials, &current.credentials) {
                (Some(reviewed), Some(current)) => reviewed.ptr_eq(current),
                (None, None) => true,
                _ => false,
            }
    }

    fn mask(&self, input: &str, pane: Option<&Redactor>) -> Result<Zeroizing<String>, String> {
        let secrets = Secrets::default();
        if let Some(credential) = &self.credential {
            secrets.add(credential);
        }
        self.scope.vault.add_matches(input, &secrets);
        let secrets = secrets.seal().map_err(|error| error.to_string())?;
        let redactor = match pane {
            Some(pane) => pane
                .with_overlay(&secrets)
                .map_err(|error| error.to_string())?,
            None => Redactor::for_journal(secrets, self.scope.policy.clone()),
        };
        mask::prompt(&redactor, input).map(Zeroizing::new)
    }
}

fn profile_matches(left: &JournalNoteProfile, right: &JournalNoteProfile) -> bool {
    left.id == right.id
        && left.transport == right.transport
        && left.selection_limit == right.selection_limit
        && left.destination == right.destination
}

pub(super) struct Review {
    pub(super) ticket: u64,
    pub(super) original: Zeroizing<String>,
    pub(super) language: String,
    authority: Arc<Authority>,
    pane: Option<Redactor>,
    destination: Zeroizing<String>,
    system: Zeroizing<String>,
    prompt: Zeroizing<String>,
    cli: Option<local_ai::cli::Request>,
    limit: usize,
}

impl Review {
    pub(super) fn prepare(
        ticket: u64,
        original: &str,
        language: &str,
        scope: Scope,
        source: Source,
        pane: Option<Redactor>,
        limit: usize,
    ) -> Result<Self, String> {
        let authority = Arc::new(Authority::new(scope, source)?);
        let language = note_translation::prepare_language(language, None, Instant::now())?;
        let mask = |input: &str| authority.mask(input, pane.as_ref());
        if mask(language)?.as_str() != language {
            return Err("The target language could not be prepared safely".into());
        }
        let destination = mask(&authority.scope.profile.destination)?;
        let transport = &authority.scope.profile.transport;
        let system = mask(&transport.render_system_prompt(note_translation::SYSTEM))?;
        let prompt = note_translation::prepare_masked(original, language, limit, mask)?;
        if system.chars().count() + prompt.chars().count() > authority.scope.profile.selection_limit
        {
            return Err("The journal note exceeds the selected AI profile's input limit.".into());
        }
        let cli = if transport.is_cli() {
            Some(
                transport
                    .prepare_cli_chat(&system, &[], &prompt)
                    .map_err(Error::message)?,
            )
        } else {
            None
        };
        Ok(Self {
            ticket,
            original: Zeroizing::new(original.to_owned()),
            language: language.into(),
            authority,
            pane,
            destination,
            system,
            prompt,
            cli,
            limit,
        })
    }

    pub(super) fn approve(
        &self,
        scope: &Scope,
        pane: Option<&Redactor>,
    ) -> Result<Arc<Authority>, String> {
        if !self.authority.matches(scope)
            || !match (&self.pane, pane) {
                (Some(old), Some(current)) => old.matches_secrets(current),
                (None, None) => true,
                _ => false,
            }
        {
            return Err(CHANGED.into());
        }
        self.authority.require_live()?;
        Ok(Arc::clone(&self.authority))
    }

    pub(super) fn run(self, client: &Client) -> Result<String, String> {
        let check = || self.authority.check();
        let deadline = Instant::now() + note_translation::TIMEOUT;
        let response = if let Some(cli) = self.cli {
            client.complete_cli_request_before_guarded(cli, deadline, check)
        } else {
            note_translation::request(client, deadline, |response_format| {
                let credential = self.authority.credential.clone();
                if response_format {
                    client.complete_reviewed_https_text_before(
                        &self.system,
                        &self.prompt,
                        deadline,
                        credential,
                        check,
                    )
                } else {
                    client.complete_reviewed_https_plain_text_before(
                        &self.system,
                        &self.prompt,
                        deadline,
                        credential,
                        check,
                    )
                }
            })
        }
        .map_err(Error::message)?;
        let translation = note_translation::finish(
            client,
            &Zeroizing::new(response),
            self.limit,
            deadline,
            |text| self.authority.mask(text, self.pane.as_ref()),
        )?;
        check()?;
        Ok(translation)
    }

    pub(super) fn view<'a, M: Clone + 'a>(
        &'a self,
        enabled: bool,
        map: impl Fn(Event) -> M + Copy + 'a,
    ) -> Element<'a, M> {
        let prompt = |value: &'a str| {
            text(value)
                .font(iced::Font::MONOSPACE)
                .wrapping(text::Wrapping::WordOrGlyph)
        };
        let mut details = column![text(self.destination.as_str())].spacing(6);
        if let Some(request) = &self.cli {
            let (label, primary) = request.primary();
            details = details
                .push(text(request.command_review()))
                .push(text(
                    "The approved program can access files, accounts or the network.",
                ))
                .push(text(label))
                .push(prompt(primary));
            if let Some(secondary) = request.secondary() {
                details = details
                    .push(text("Exact UTF-8 user-prompt.txt"))
                    .push(prompt(secondary));
            }
        } else {
            details = details
                .push(prompt(&self.system))
                .push(prompt(&self.prompt));
        }
        column![
            text("Review note translation").size(16),
            scrollable(details).height(Length::Fixed(144.0)),
            row![
                button("Cancel").on_press(map(Event::Cancel)),
                button(if self.cli.is_some() {
                    "Run reviewed CLI"
                } else {
                    "Send reviewed note"
                })
                .on_press_maybe(enabled.then(|| map(Event::Approve(self.ticket)))),
            ]
            .spacing(6),
        ]
        .spacing(6)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use serde_json::json;

    use super::*;
    use crate::local_ai::cli::{Program, fixture_executable};

    #[test]
    fn journal_note_outbound_cli_checks_source_and_uses_exact_reviewed_files() {
        const ENV: &str = "KORTTY_JOURNAL_NOTE_CLI_FIXTURE";
        let Some(home) = std::env::var_os(ENV).map(PathBuf::from) else {
            let scratch = crate::snippets::tests::Profile::new();
            let home = scratch.0.canonicalize().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "workspace::journal_note_translation::outbound::tests::journal_note_outbound_cli_checks_source_and_uses_exact_reviewed_files", "--nocapture"])
                .env(ENV, &home).env("TEMP", &home).env("TMP", &home).env("TMPDIR", &home)
                .output().unwrap();
            assert!(
                child.status.success()
                    && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
                "{}\n{}",
                String::from_utf8_lossy(&child.stdout),
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        };
        let root = home.join("private-profile");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        crate::profile::set_runtime_root(root.clone());
        let (executable, fingerprint) = Program::inspect(&fixture_executable(&root)).unwrap();
        let settings: Configuration = serde_json::from_value(json!({
            "profiles": {"profiles":[
                {"id":1,"name":"Journal note CLI","connection_mode":"cli","endpoint":"","vision":"disabled",
                 "prompt_preset":"qwen","cli":{"path":executable,"fingerprint":fingerprint,
                    "arguments":["fixture_files","{promptFile}"]}}
            ],"default":1,"last_id":1}
        })).unwrap();
        settings
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let config = Arc::new(Configuration::load_fixture_from(&root).unwrap());
        let mut window = WindowSettings::default();
        window.journal_ai.enabled = true;
        window.save(&root.join("window-settings.json")).unwrap();
        let revision = Arc::new(());
        let scope = || Scope {
            config: Arc::clone(&config),
            profile: config.journal_note_translation_profile().unwrap().unwrap(),
            policy: ClipboardPolicy::load().terminal_logging,
            preferences: window.journal_ai.clone(),
            vault: JournalQuestionSecrets::default(),
            vault_change: Arc::downgrade(&revision),
            credentials: None,
        };
        let active = Arc::new(AtomicBool::new(true));
        let source: Source = Arc::new({
            let active = Arc::clone(&active);
            move || {
                active
                    .load(Ordering::Acquire)
                    .then_some(())
                    .ok_or_else(|| "source changed".into())
            }
        });
        let pane = || {
            let secrets = Secrets::default();
            secrets.add("hidden-session-value");
            Redactor::for_journal(
                secrets.seal().unwrap(),
                ClipboardPolicy::load().terminal_logging,
            )
        };
        let review = || {
            Review::prepare(
                1,
                "Note hidden-session-value password=local-secret",
                "de",
                scope(),
                Arc::clone(&source),
                Some(pane()),
                4096,
            )
            .unwrap()
        };
        let stale = review();
        let marker = root.join("fixture-prompt.txt");
        active.store(false, Ordering::Release);
        stale.approve(&scope(), Some(&pane())).unwrap();
        let transport = scope().profile.transport;
        let pending = local_ai::start(transport, move |client| stale.run(client)).unwrap();
        assert_eq!(
            pending.reply.recv_blocking().unwrap().unwrap_err(),
            "source changed"
        );
        assert!(!marker.exists());
        active.store(true, Ordering::Release);
        let approved = review();
        let request = approved.cli.as_ref().unwrap();
        let exact = request.primary().1.to_owned();
        assert!(exact.contains("Note *** password=***"));
        assert!(exact.contains("target_language"));
        assert!(!exact.contains("hidden-session-value") && !exact.contains("local-secret"));
        assert_ne!(approved.system.as_str(), note_translation::SYSTEM);
        assert!(!marker.exists());
        approved.approve(&scope(), Some(&pane())).unwrap();
        let transport = scope().profile.transport;
        let pending = local_ai::start(transport, move |client| approved.run(client)).unwrap();
        let reply = pending.reply.recv_blocking().unwrap().unwrap();
        assert_eq!(reply, "fixture final answer");
        assert_eq!(std::fs::read(marker).unwrap(), exact.as_bytes());
    }
}
