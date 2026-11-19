use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Weak};

use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, text, text_editor, text_input,
};
use iced::{Element, Font, Length, Task, window};

use crate::ai_manager::{
    Configuration, ProfileId, SkillGuidance, SkillUsageReceipt, SkillUsageSurface,
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::editor_window::{self, AiOrigin, EditorWindow};
use crate::local_ai::{Cancel, Pending, Profile};
use crate::sftp_window::RemoteEditor;
use crate::snippet_analysis::request::MAX_REMOTE_REQUEST_BYTES;
#[cfg(test)]
use crate::snippet_window::SnippetWindow;
use crate::snippet_window::ai_assist::migration;
use crate::snippet_window::ai_assist::{
    Action, Change, Editor, FontView, MAX_TARGET_LANGUAGE_BYTES, Message as ActionMessage, Outcome,
    Recipe, Request, Snapshot, security, translation_language,
};
use crate::snippet_window::languages::mix::{HostFormat, Inspection};
use crate::snippet_window::{self, compare};
use crate::terminal_clipboard::TerminalClipboard;
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::VaultWindow;
use crate::workspace::ai_quota;

use super::super::tab_titles::WindowSettings;
use super::{Message, Workspace};

mod automatic;
mod cli;
pub(super) use automatic::AutomaticCompletion;

type DispatchGuard = Box<dyn Fn() -> Result<(), String> + Send>;

#[derive(Default)]
pub(crate) struct State(Option<Panel>);

struct Panel {
    source: Snapshot,
    language_mix: Option<Inspection>,
    staged_migration: Option<Outcome>,
    recipe: Recipe,
    explicit_profile: bool,
    profiles: Vec<Choice>,
    skills: Vec<(u64, String)>,
    phase: Phase,
    status: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Choice {
    id: ProfileId,
    name: String,
    model: Option<String>,
    destination: String,
    route: Route,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Route {
    Local,
    Https,
    Cli,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let model = self.model.as_deref().unwrap_or("Server selects the model");
        write!(f, "{} · {model}", self.name)?;
        match self.route {
            Route::Local => Ok(()),
            Route::Https => f.write_str(" · HTTPS"),
            Route::Cli => f.write_str(" · CLI"),
        }
    }
}

impl Choice {
    fn cli_reference(&self) -> Result<crate::ai_manager::ProfileRef, String> {
        u64::try_from(self.id)
            .map(crate::ai_manager::ProfileRef::Numeric)
            .map_err(|_| "Choose a saved CLI profile for snippet assistance.".into())
    }

    fn transport(&self, config: &Configuration) -> Result<(Profile, usize), String> {
        match self.route {
            Route::Cli => {
                config.snippet_analysis_cli_transport(&self.cli_reference()?, &self.destination)
            }
            Route::Local | Route::Https => config.snippet_assistance_transport(
                self.id,
                self.model.as_deref(),
                &self.destination,
                self.route == Route::Https,
            ),
        }
    }
}

enum Phase {
    Configure,
    Review(Box<Review>),
    Running {
        ticket: u64,
        config: Arc<Configuration>,
        outbound: Option<Outbound>,
        skill_usage: Option<SkillUsageReceipt>,
        _cancel: Cancel,
    },
    Result(Box<Proposed>),
    Last(Arc<Change>, Box<compare::View>),
}

impl Phase {
    fn font_view(&self, action: Action) -> Option<FontView> {
        let diff_view = |action| match action {
            Action::Describe | Action::CorrectDescription => FontView::Description,
            _ => FontView::Diff,
        };
        match self {
            Self::Last(change, _) => Some(diff_view(change.recipe.action)),
            Self::Result(result) if result.diff.is_some() => Some(diff_view(action)),
            Self::Result(result) if !result.outcome.alternatives.is_empty() => {
                Some(FontView::Alternatives)
            }
            Self::Result(_) if action == Action::SyntaxReview => Some(FontView::Review),
            Self::Result(_) if action == Action::SecurityReview => Some(FontView::SecurityReport),
            _ => None,
        }
    }
}

struct Review {
    config: Arc<Configuration>,
    choice: Choice,
    transport: Profile,
    request: Request,
    outbound: Option<Outbound>,
    cli: Option<cli::Prepared>,
    quota: ai_quota::State,
}

struct Proposed {
    ticket: u64,
    config: Arc<Configuration>,
    outbound: Option<Outbound>,
    outcome: Outcome,
    diff: Option<Box<compare::View>>,
    alternative: Option<usize>,
    alternative_previews: Vec<text_editor::Content>,
    expanded_alternative: Option<usize>,
    selected_findings: BTreeSet<usize>,
    summary_drag: Option<u16>,
}

impl Proposed {
    fn diff_summary_height(&self) -> Option<f32> {
        let migration = self.outcome.migration.as_ref();
        (self.alternative.is_some() || migration.is_some()).then(|| {
            migration.map_or(56.0, |migration| {
                40.0 + migration.notes.len().min(24) as f32 * 20.0
            })
        })
    }

    fn diff_summary(&self) -> Element<'_, ActionMessage> {
        let alternative = self
            .alternative
            .and_then(|index| self.outcome.alternatives.get(index));
        column![
            alternative.map(|alternative| text(&alternative.summary)),
            self.outcome.migration.as_ref().map(|migration| column![
                migration
                    .plan
                    .destination()
                    .map(|platform| text(format!("Proposed platform · {platform}"))),
                migration
                    .plan
                    .language()
                    .map(|language| text(format!("Proposed language · {language}"))),
                column(migration.notes.iter().map(|note| text(note).into())).spacing(3),
            ]
            .spacing(4)),
        ]
        .spacing(4)
        .into()
    }

    fn alternative_visible(&self, ticket: u64, index: usize) -> bool {
        self.ticket == ticket
            && self.diff.is_none()
            && index < self.alternative_previews.len()
            && self
                .expanded_alternative
                .is_none_or(|expanded| expanded == index)
    }

    fn selected_outcome(&self) -> &Outcome {
        self.alternative
            .and_then(|index| self.outcome.alternatives.get(index))
            .map_or(&self.outcome, |alternative| &alternative.outcome)
    }

    fn alternative_card(&self, index: usize, size: u8) -> Element<'_, ActionMessage> {
        let alternative = &self.outcome.alternatives[index];
        let code = &self.alternative_previews[index];
        let expanded = self.expanded_alternative == Some(index);
        let ticket = self.ticket;
        let editor = text_editor(code)
            .font(Font::MONOSPACE)
            .size(u32::from(size))
            .wrapping(iced::widget::text::Wrapping::None)
            .height(if expanded { Length::Fill } else { 180.into() })
            .on_action(move |action| ActionMessage::SelectAlternativeCode(ticket, index, action))
            .key_binding(
                move |event| match text_editor::Binding::from_key_press(event) {
                    Some(text_editor::Binding::Copy) => Some(text_editor::Binding::Custom(
                        ActionMessage::CopyAlternativeCode(ticket, index),
                    )),
                    binding @ Some(
                        text_editor::Binding::Move(_)
                        | text_editor::Binding::Select(_)
                        | text_editor::Binding::SelectAll
                        | text_editor::Binding::Unfocus,
                    ) => binding,
                    _ => None,
                },
            );
        container(
            column![
                text(&alternative.title).size(16),
                (!alternative.summary.is_empty()).then(|| text(&alternative.summary)),
                editor,
                row![
                    button("Review change…")
                        .on_press(ActionMessage::ChooseAlternative(Some(index))),
                    button(if expanded {
                        "Show all alternatives"
                    } else {
                        "Expand preview"
                    })
                    .on_press(ActionMessage::ExpandAlternative(ticket, index)),
                    button("Copy selection").on_press_maybe(
                        code.selection()
                            .is_some_and(|text| !text.is_empty())
                            .then_some(ActionMessage::CopyAlternativeCode(ticket, index)),
                    ),
                ]
                .spacing(8)
                .wrap(),
            ]
            .spacing(6),
        )
        .padding(8)
        .width(Length::Fill)
        .style(container::secondary)
        .into()
    }
}

#[derive(Clone)]
struct Context<'a> {
    policy: &'a ClipboardPolicy,
    vault: Option<&'a VaultWindow>,
    vault_revision: Weak<()>,
    file: Option<&'a FileScope>,
    snippet_translation_language: Option<&'a str>,
    snippet_alternative_count: Option<u8>,
    additional_instructions: bool,
}

#[derive(Clone)]
struct FileScope {
    origin: AiOrigin,
    connection: Option<RemoteEditor>,
    revision: Weak<()>,
}

impl FileScope {
    fn authorize(&self, policy: &ClipboardPolicy, edits: bool) -> Result<(), String> {
        if self.revision.strong_count() == 0 {
            return Err("The file changed. Review a new AI request.".into());
        }
        self.origin.authorize(policy)?;
        if let Some(connection) = &self.connection {
            if connection.is_closed() {
                return Err("The original file connection closed.".into());
            }
            if edits
                && (!connection.writable()
                    || policy.remote_editor != crate::clipboard_policy::RemoteEditorPolicy::Allow)
            {
                return Err("The original file connection is read-only.".into());
            }
            if !connection.allows_source_tool(policy, edits) {
                return Err("Managed policy blocks the original file connection.".into());
            }
        }
        Ok(())
    }
}

enum Outbound {
    Https(HttpsReview),
    Cli(cli::Review),
}

impl Outbound {
    fn matches(&self, context: &Context<'_>) -> bool {
        match self {
            Self::Https(review) => review.matches(&context.policy.terminal_logging, context.vault),
            Self::Cli(review) => review.matches(context),
        }
    }

    fn require_current(&self, config: &Configuration, context: &Context<'_>) -> Result<(), String> {
        match self {
            Self::Https(review) => {
                review.require_current(config, &context.policy.terminal_logging, context.vault)
            }
            Self::Cli(review) => review.require_current(config, context),
        }
    }
}

#[derive(Clone)]
struct HttpsReview {
    policy: Policy,
    vault: Option<Weak<()>>,
    profile: Arc<Profile>,
}

impl HttpsReview {
    fn capture(policy: &Policy, vault: Option<&VaultWindow>, profile: Profile) -> Self {
        Self {
            policy: policy.clone(),
            vault: vault.and_then(VaultWindow::credential_revision),
            profile: Arc::new(profile),
        }
    }

    fn matches(&self, policy: &Policy, vault: Option<&VaultWindow>) -> bool {
        self.policy.equivalent_redactions(policy)
            && match (
                &self.vault,
                vault.and_then(VaultWindow::credential_revision),
            ) {
                (Some(reviewed), Some(current)) => Weak::ptr_eq(reviewed, &current),
                (None, None) => true,
                _ => false,
            }
    }

    fn authorize(&self, config: &Configuration) -> Result<(), String> {
        self.profile.require_current_policy_authority()?;
        let installed = ClipboardPolicy::load();
        if installed.problem.is_none()
            && installed.ai_allowed
            && self
                .policy
                .equivalent_redactions(&installed.terminal_logging)
            && self
                .vault
                .as_ref()
                .is_none_or(|vault| vault.strong_count() > 0)
            && config
                .reload_saved()
                .is_ok_and(|current| current == *config)
        {
            Ok(())
        } else {
            Err("The saved AI settings, vault, or masking policy changed. Review the HTTPS request again.".into())
        }
    }

    fn require_current(
        &self,
        config: &Configuration,
        policy: &Policy,
        vault: Option<&VaultWindow>,
    ) -> Result<(), String> {
        if !self.matches(policy, vault) {
            return Err(
                "The vault or masking policy changed. Review the HTTPS request again.".into(),
            );
        }
        self.authorize(config)
    }
}

impl Phase {
    fn outbound(&self) -> Option<(&Arc<Configuration>, &Outbound)> {
        match self {
            Self::Review(review) => Some((&review.config, review.outbound.as_ref()?)),
            Self::Running {
                config, outbound, ..
            } => Some((config, outbound.as_ref()?)),
            Self::Result(proposed) => Some((&proposed.config, proposed.outbound.as_ref()?)),
            _ => None,
        }
    }
}

impl State {
    pub(super) fn is_inline_completion(&self) -> bool {
        self.0
            .as_ref()
            .and_then(|panel| panel.recipe.completion.as_ref())
            .is_some_and(|target| target.inline)
    }

    pub(super) fn completion_running(&self) -> bool {
        self.0.as_ref().is_some_and(|panel| {
            panel.recipe.completion.is_some() && matches!(panel.phase, Phase::Running { .. })
        })
    }

    pub(super) fn cancel_completion(&mut self, editor: &mut dyn Editor) {
        if self
            .0
            .as_ref()
            .is_some_and(|panel| panel.recipe.completion.is_some())
        {
            if self.completion_running() {
                editor.assist_status("AI suggestions canceled.".into());
            }
            *self = State::default();
            editor.clear_completion_candidates();
        }
    }

    pub(super) fn revalidate_source(&mut self, editor: &mut dyn Editor) {
        if self
            .0
            .as_ref()
            .is_some_and(|panel| panel.recipe.completion.is_some() && !panel.current(editor))
        {
            self.cancel_completion(editor);
        }
        if self
            .0
            .as_ref()
            .is_none_or(|panel| panel.recipe.completion.is_none())
        {
            editor.clear_completion_candidates();
        }
        if let Some(panel) = self.0.as_mut()
            && panel.phase.outbound().is_some()
            && !panel.current(editor)
        {
            panel.phase = Phase::Configure;
            panel.status = Some("The snippet changed. Review a new AI request.".into());
        }
    }

    pub(super) fn view<'a>(
        &'a self,
        editor: &'a dyn Editor,
        settings: Option<&WindowSettings>,
    ) -> Option<Element<'a, ActionMessage>> {
        let panel = self.0.as_ref()?;
        if panel.recipe.completion.is_some()
            && matches!(
                panel.phase,
                Phase::Running { .. } | Phase::Result(_) | Phase::Last(..)
            )
        {
            return None;
        }
        let additional_instructions =
            settings.is_none_or(WindowSettings::snippet_ai_additional_instructions);
        let current_font = panel
            .phase
            .font_view(panel.recipe.action)
            .zip(settings)
            .map(|(view, settings)| (view, settings.snippet_ai_font(view)));
        let font_size = current_font.map(|(_, size)| size);
        let review_size = font_size.map_or(16, u32::from);
        let close = || button("Close").on_press(ActionMessage::Close);
        let title = match &panel.phase {
            Phase::Last(..) => "Last AI change",
            _ if panel.recipe.completion.is_some() => "AI completion suggestions",
            _ => editor.assistant_title(),
        };
        let mut body = column![
            text(title).size(20),
            text(panel.status.as_deref().unwrap_or_default())
        ]
        .spacing(8);
        if let Some((view, size)) = current_font {
            body = body.push(view.controls(size, |size| ActionMessage::FontSize(view, size)));
        }
        let page: Element<'_, ActionMessage> = match &panel.phase {
            Phase::Configure => {
                let selected = panel
                    .profiles
                    .iter()
                    .find(|choice| Some(choice.id) == panel.recipe.profile)
                    .cloned();
                let mut actions: Vec<_> = Action::ALL
                    .into_iter()
                    .filter(|&action| editor.supports_ai_action(action))
                    .collect();
                if panel.recipe.findings.is_some() && editor.supports_ai_action(Action::SecurityFix)
                {
                    actions.push(Action::SecurityFix);
                }
                if panel.can_migrate_script() {
                    actions.push(Action::MigrateScript);
                }
                if matches!(
                    panel.recipe.action,
                    Action::FormatCode | Action::SyntaxReview
                ) {
                    actions.push(panel.recipe.action);
                }
                let mut options = column![
                    panel.recipe.completion.is_none().then(|| pick_list(
                        actions,
                        Some(panel.recipe.action),
                        ActionMessage::Choose
                    )),
                    pick_list(panel.profiles.as_slice(), selected, |choice: Choice| {
                        ActionMessage::Profile(choice.id)
                    })
                    .placeholder("Select an AI profile"),
                    additional_instructions.then(|| {
                        text_input("Additional instructions", &panel.recipe.instructions)
                            .on_input(ActionMessage::Instructions)
                    }),
                ]
                .spacing(8);
                if let Some(mix) = panel.language_mix.as_ref().filter(|mix| mix.mixed()) {
                    let title = mix.host.map_or_else(
                        || "Embedded script languages".into(),
                        |host| format!("{} · script step languages", host.label()),
                    );
                    let mut blocks = column![text(title)].spacing(3);
                    for block in mix.embedded.iter().take(5) {
                        let start = block.lines.start();
                        let end = block.lines.end();
                        let location = if start == end {
                            format!("line {start}")
                        } else {
                            format!("lines {start}–{end}")
                        };
                        blocks = blocks.push(text(format!("{} · {location}", block.language)));
                    }
                    if mix.embedded.len() > 5 {
                        blocks = blocks.push(text(format!("{} more", mix.embedded.len() - 5)));
                    }
                    options = options.push(blocks);
                }
                if panel.recipe.action == Action::Translate {
                    options = options.push(
                        text_input("Target language", &panel.recipe.target_language)
                            .on_input(ActionMessage::TargetLanguage),
                    );
                }
                if panel.offers_migration() {
                    let targets = panel.migration_targets();
                    let language = panel
                        .recipe
                        .migration_language
                        .filter(|language| targets.contains(language));
                    let platforms = panel.migration_platforms();
                    let platform = panel
                        .recipe
                        .migration_platform
                        .filter(|host| platforms.contains(host));
                    options = options.push(
                        column![
                            panel
                                .language_mix
                                .as_ref()
                                .and_then(|mix| mix.host)
                                .map(|host| text(format!("Current format · {host}"))),
                            (!platforms.is_empty()).then(|| row![
                                pick_list(platforms, platform, |host| {
                                    ActionMessage::MigrationPlatform(Some(host))
                                })
                                .placeholder("Keep current platform"),
                                platform.map(|_| button("Clear platform")
                                    .on_press(ActionMessage::MigrationPlatform(None))),
                            ]
                            .spacing(6)),
                            (!targets.is_empty()).then(|| row![
                                pick_list(targets, language, ActionMessage::MigrationLanguage)
                                    .placeholder(if platform.is_some() {
                                        "Keep step languages where supported"
                                    } else if panel.recipe.action == Action::SecurityFix {
                                        "Keep current programming language"
                                    } else {
                                        "Select the target programming language"
                                    }),
                                language.map(|_| button("Clear language")
                                    .on_press(ActionMessage::ClearMigrationLanguage)),
                            ]
                            .spacing(6)),
                        ]
                        .spacing(4),
                    );
                }
                if !panel.skills.is_empty() {
                    options = options.push(text("AI skills for this request"));
                    for (id, label) in &panel.skills {
                        let id = *id;
                        options = options.push(
                            checkbox(panel.recipe.skills.contains(&id))
                                .label(label)
                                .on_toggle(move |selected| ActionMessage::Skill(id, selected)),
                        );
                    }
                }
                column![
                    scrollable(options).height(Length::Fill),
                    row![
                        button("Review request…").on_press_maybe(
                            panel
                                .recipe
                                .profile
                                .filter(|_| panel.recipe.action != Action::MigrateScript
                                    || panel.recipe.migration_language.is_some()
                                    || panel.recipe.migration_platform.is_some())
                                .map(|_| ActionMessage::Review)
                        ),
                        button("Last AI change…").on_press_maybe(
                            editor.last_ai_change().map(|_| ActionMessage::ShowLast)
                        ),
                        close()
                    ]
                    .spacing(8)
                ]
                .spacing(8)
                .into()
            }
            Phase::Review(review) => column![
                (panel.recipe.action == Action::SecurityFix && panel.recipe.includes_migration())
                    .then(|| text(if panel.staged_migration.is_some() {
                        "Security fixes · migrated source"
                    } else {
                        "Language migration · first step"
                    })),
                text(format!(
                    "Send to {} · {}",
                    review.choice, review.choice.destination
                )),
                matches!(review.outbound.as_ref(), Some(Outbound::Https(_))).then(|| text(
                    "This HTTPS provider receives the masked prompts and the selected model."
                )),
                panel.recipe.completion.as_ref().map(|target| text(if target.inline {
                    "Inline suggestions send a new masked code window and known symbols to this profile after typing pauses in this Snippet Manager session."
                } else {
                    "Automatic suggestions send a new masked code window and known symbols to this profile whenever a completion list opens in this Snippet Manager session."
                }).size(13)),
                review.quota.label().map(|label| text(label).size(13)),
                scrollable(if let Some(cli) = &review.cli {
                    cli.view()
                } else {
                    column![
                        text("System instructions").size(16),
                        text(review.request.system.as_ref()),
                        text("Exact masked request payload").size(16),
                        text(review.request.input.as_ref())
                    ]
                    .spacing(8)
                    .into()
                })
                .height(Length::Fill),
                row![
                    button(if review.quota.stale() {
                        "Refresh quota forecast"
                    } else if review.cli.is_some() {
                        "Run reviewed CLI"
                    } else {
                        "Send reviewed request"
                    })
                    .on_press_maybe((!review.quota.loading()).then(|| {
                        review
                            .cli
                            .as_ref()
                            .map_or(ActionMessage::Send, |cli| ActionMessage::RunCli(cli.ticket))
                    })),
                    panel.recipe.completion.is_some().then(|| {
                        button("Send and enable automatically").on_press_maybe(
                            (!review.quota.loading() && !review.quota.stale()).then(|| {
                                ActionMessage::EnableAutomaticCompletion(review.cli.as_ref().map(|cli| cli.ticket))
                            })
                        )
                    }),
                    button("Back").on_press(ActionMessage::Back),
                    close()
                ]
                .spacing(8)
            ]
            .spacing(8)
            .into(),
            Phase::Running { .. } => column![
                text("Waiting for the approved AI request…"),
                button("Cancel request").on_press(ActionMessage::Back),
                close()
            ]
            .spacing(8)
            .into(),
            Phase::Result(result) => {
                if let Some(diff) = &result.diff {
                    let migration_pending = panel.recipe.action == Action::SecurityFix
                        && result
                            .outcome
                            .migration
                            .as_ref()
                            .is_some_and(|migration| !migration.security_fixes);
                    let comparison = move || diff.review_body_with_font(font_size).map(ActionMessage::Diff);
                    let evidence = match (result.diff_summary_height(), settings) {
                        (Some(estimate), Some(settings)) => {
                            crate::snippet_window::diff_summary::view(
                                result.summary_drag.or(settings.snippet_ai_diff_summary_divider),
                                estimate,
                                || result.diff_summary(),
                                comparison,
                                move |event| ActionMessage::SummaryDivider(result.ticket, result.alternative, event),
                            )
                        }
                        (Some(_), None) => column![
                            scrollable(result.diff_summary()).height(Length::Fixed(90.0)),
                            comparison(),
                        ].height(Length::Fill).into(),
                        (None, _) => comparison(),
                    };
                    column![
                        evidence,
                        row![
                            if migration_pending {
                                button("Review security fix request…").on_press(
                                    ActionMessage::ReviewMigratedSecurityFix(result.ticket),
                                )
                            } else {
                                button("Apply reviewed change").on_press(ActionMessage::Apply)
                            },
                            result.alternative.map(|_| {
                                button("Other alternatives")
                                    .on_press(ActionMessage::ChooseAlternative(None))
                            }),
                            button("Discard proposal").on_press(ActionMessage::Back),
                            close()
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                    .into()
                } else if !result.outcome.alternatives.is_empty() {
                    let size = font_size.unwrap_or(FontView::DEFAULT);
                    let alternatives: Element<'_, ActionMessage> =
                        if let Some(index) = result.expanded_alternative {
                            result.alternative_card(index, size)
                        } else {
                            let cards = (0..result.outcome.alternatives.len())
                                .fold(column![].spacing(10), |list, index| {
                                    list.push(result.alternative_card(index, size))
                                });
                            scrollable(cards).height(Length::Fill).into()
                        };
                    column![
                        alternatives,
                        row![button("Back").on_press(ActionMessage::Back), close()].spacing(8)
                    ]
                    .spacing(8)
                    .into()
                } else if panel.recipe.action == Action::SecurityReview {
                    let mut report =
                        column![text(result.outcome.text.as_ref()).size(review_size)].spacing(10);
                    for (index, finding) in result.outcome.findings.iter().enumerate() {
                        report = report.push(
                            column![
                                checkbox(result.selected_findings.contains(&index))
                                    .label(format!("{} · {}", finding.severity, finding.title))
                                    .text_size(review_size)
                                    .on_toggle(move |selected| {
                                        ActionMessage::SelectFinding(result.ticket, index, selected)
                                    }),
                                text(&finding.impact).size(review_size),
                                text(format!("Recommendation · {}", finding.recommendation))
                                    .size(review_size),
                            ]
                            .spacing(4),
                        );
                    }
                    column![
                        (!result.outcome.findings.is_empty()).then(|| {
                            checkbox(
                                result.selected_findings.len() == result.outcome.findings.len(),
                            )
                            .label("Select all findings")
                            .on_toggle(move |selected| {
                                ActionMessage::SelectAllFindings(result.ticket, selected)
                            })
                        }),
                        scrollable(report).height(Length::Fill),
                        row![
                            button("Propose fixes for selected findings…").on_press_maybe(
                                (!result.selected_findings.is_empty()
                                    && editor.supports_ai_action(Action::SecurityFix)
                                    && panel.current(editor))
                                .then_some(ActionMessage::Fix(result.ticket))
                            ),
                            button("Review security again…")
                                .on_press(ActionMessage::ReviewSecurityAgain(result.ticket)),
                            button("Back").on_press(ActionMessage::Back),
                            close()
                        ]
                        .spacing(8),
                    ]
                    .spacing(8)
                    .into()
                } else {
                    column![
                        scrollable(text(result.outcome.text.as_ref()).size(
                            if panel.recipe.action == Action::SyntaxReview {
                                review_size
                            } else {
                                16
                            }
                        ))
                        .height(Length::Fill),
                        row![
                            (panel.recipe.action == Action::CompactCommand).then(|| {
                                button("Copy reviewed command").on_press(ActionMessage::CopyCommand)
                            }),
                            button("Back").on_press(ActionMessage::Back),
                            close()
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                    .into()
                }
            }
            Phase::Last(change, diff) => {
                let current = editor.assist_snapshot();
                let exact = current.as_ref().is_ok_and(|source| change.undoable(source));
                let rerunnable = exact
                    && !matches!(
                        change.recipe.action,
                        Action::SecurityFix | Action::MigrateScript
                    );
                column![
                    diff.review_body_with_font(font_size)
                        .map(ActionMessage::Diff),
                    row![
                        button("Undo last AI change")
                            .on_press_maybe(exact.then_some(ActionMessage::Undo)),
                        button("Rerun with original profile…")
                            .on_press_maybe(rerunnable.then_some(ActionMessage::Rerun)),
                        button("Back").on_press(ActionMessage::Back),
                        close()
                    ]
                    .spacing(8)
                    .wrap()
                ]
                .spacing(8)
                .into()
            }
        };
        body = body.push(page);
        Some(body.padding(12).height(Length::Fill).into())
    }
}

impl Workspace {
    pub(super) fn snippet_ai_zoom(
        &mut self,
        id: window::Id,
        increase: bool,
    ) -> Option<Task<Message>> {
        let surface = self.snippets.get(&id)?;
        let message = if surface.window.can_open_assist()
            && let Some(panel) = &surface.assist.0
        {
            let view = panel.phase.font_view(panel.recipe.action)?;
            view.step(self.window_settings.snippet_ai_font(view), increase)
                .map(|size| snippet_window::Message::Assist(ActionMessage::FontSize(view, size)))
        } else if surface.analysis.shows_ai_diff() {
            let view = FontView::Diff;
            view.step(self.window_settings.snippet_ai_font(view), increase)
                .map(|size| {
                    snippet_window::Message::Analysis(
                        snippet_window::analysis::Message::AiDiffFont(size),
                    )
                })
        } else {
            return None;
        };
        Some(message.map_or_else(Task::none, |message| self.update_snippets(id, message)))
    }

    pub(in crate::workspace) fn cancel_snippet_assistance(&mut self) {
        for surface in self.snippets.values_mut() {
            surface.automatic_completion = None;
            surface.inline_completion = None;
            surface.cancel_inline_completion();
            surface.assist.cancel_completion(&mut surface.window);
            surface.assist = State::default();
        }
        for editor in self.editors.values_mut().chain(
            self.project_files
                .values_mut()
                .filter_map(|tab| tab.editor()),
        ) {
            editor.ai_assist = State::default();
        }
    }

    pub(in crate::workspace) fn revalidate_remote_snippet_assistance(&mut self) {
        let config = self
            .ai_configuration()
            .filter(|_| !self.exiting && !self.scheduler_draining);
        let context = Context {
            policy: &self.clipboard.policy,
            vault: self.vault.as_ref(),
            vault_revision: self.vault_change_revision(),
            file: None,
            snippet_translation_language: None,
            snippet_alternative_count: None,
            additional_instructions: self.window_settings.snippet_ai_additional_instructions(),
        };
        for surface in self.snippets.values_mut() {
            if surface.inline_completion.as_ref().is_some_and(|approval| {
                config
                    .as_ref()
                    .is_none_or(|config| !approval.matches(config, &context))
            }) {
                surface.inline_completion = None;
                surface.cancel_inline_completion();
            }
            if surface
                .automatic_completion
                .as_ref()
                .is_some_and(|approval| {
                    config
                        .as_ref()
                        .is_none_or(|config| !approval.matches(config, &context))
                })
            {
                surface.automatic_completion = None;
                surface.assist.cancel_completion(&mut surface.window);
            }
            if surface
                .assist
                .0
                .as_ref()
                .is_some_and(|panel| matches!(panel.phase.outbound(), Some((_, Outbound::Cli(_)))))
            {
                surface.assist.revalidate_source(&mut surface.window);
            }
            if let Some(panel) = surface.assist.0.as_mut()
                && let Some((approved, outbound)) = panel.phase.outbound()
                && (config
                    .as_ref()
                    .is_none_or(|current| !Arc::ptr_eq(current, approved))
                    || !outbound.matches(&context))
            {
                panel.phase = Phase::Configure;
                if panel.recipe.completion.is_some() {
                    surface.window.clear_completion_candidates();
                }
                panel.status = Some(
                    "AI settings, vault, or masking policy changed. Review the request again."
                        .into(),
                );
            }
        }
        for editor in self.editors.values_mut().chain(
            self.project_files
                .values_mut()
                .filter_map(|tab| tab.editor()),
        ) {
            let scope = FileScope {
                origin: editor.ai_origin.clone(),
                connection: editor.remote_editor().cloned(),
                revision: editor.file_ai_revision_token(),
            };
            let file_context = Context {
                file: Some(&scope),
                ..context.clone()
            };
            if editor.ai_assist.0.as_ref().is_some_and(|panel| {
                (matches!(
                    panel.phase,
                    Phase::Review(_) | Phase::Running { .. } | Phase::Result(_)
                ) && !panel.current(editor))
                    || scope
                        .authorize(context.policy, panel.recipe.action.edits_file())
                        .is_err()
                    || panel.phase.outbound().is_some_and(|(approved, outbound)| {
                        !outbound.matches(&file_context)
                            || config
                                .as_ref()
                                .is_none_or(|current| !Arc::ptr_eq(current, approved))
                    })
            }) {
                editor.ai_assist = State::default();
            }
        }
    }

    pub(super) fn update_snippet_assist(
        &mut self,
        id: window::Id,
        action: ActionMessage,
    ) -> Task<Message> {
        if self.exiting
            || self.scheduler_draining
            || (self.hosted_close_locks(id) && !matches!(action, ActionMessage::QuotaFinished(..)))
        {
            return Task::none();
        }
        if let ActionMessage::FontSize(view, size) = action {
            let active = self.snippets.get(&id).is_some_and(|surface| {
                surface.window.can_open_assist()
                    && surface.assist.0.as_ref().is_some_and(|panel| {
                        panel.phase.font_view(panel.recipe.action) == Some(view)
                    })
            });
            if active && view.bounds().contains(&size) {
                let slot = match view {
                    FontView::Alternatives => {
                        &mut self.window_settings.snippet_ai_alternatives_font_size
                    }
                    FontView::Diff => &mut self.window_settings.snippet_ai_diff_font_size,
                    FontView::Review => &mut self.window_settings.snippet_ai_review_font_size,
                    FontView::Description => {
                        &mut self.window_settings.snippet_ai_description_font_size
                    }
                    FontView::SecurityReport => {
                        &mut self.window_settings.snippet_security_report_font_size
                    }
                };
                if *slot != Some(size) {
                    *slot = Some(size);
                    self.save_window_settings(Some(id));
                }
            }
            return Task::none();
        }
        if let ActionMessage::SummaryDivider(ticket, choice, event) = action {
            let position = self
                .snippets
                .get_mut(&id)
                .and_then(|surface| surface.assist.0.as_mut())
                .and_then(|panel| match &mut panel.phase {
                    Phase::Result(result)
                        if result.ticket == ticket
                            && result.alternative == choice
                            && result.diff.is_some()
                            && result.diff_summary_height().is_some() =>
                    {
                        crate::snippet_window::diff_summary::update(&mut result.summary_drag, event)
                    }
                    _ => None,
                });
            if let Some(position) = position {
                self.save_snippet_diff_summary(id, position);
            }
            return Task::none();
        }
        let config = self.ai_configuration();
        let ticket = self.allocate();
        let context = Context {
            policy: &self.clipboard.policy,
            vault: self.vault.as_ref(),
            vault_revision: self.vault_change_revision(),
            file: None,
            snippet_translation_language: self
                .window_settings
                .snippet_translation_target_language
                .as_deref(),
            snippet_alternative_count: self.window_settings.snippet_alternative_count,
            additional_instructions: self.window_settings.snippet_ai_additional_instructions(),
        };
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let dispatch = Dispatch {
            id,
            ticket,
            file: false,
        };
        if matches!(action, ActionMessage::DisableAutomaticCompletion) {
            surface.automatic_completion = None;
            surface.assist.cancel_completion(&mut surface.window);
            surface
                .window
                .assist_status("Automatic AI suggestions turned off.".into());
            return Task::none();
        }
        if matches!(
            action,
            ActionMessage::DisableInlineCompletion | ActionMessage::StopInlineCompletion
        ) {
            if matches!(action, ActionMessage::DisableInlineCompletion) {
                surface.inline_completion = None;
                surface
                    .window
                    .assist_status("Inline AI suggestions turned off.".into());
            }
            surface.cancel_inline_completion();
            return Task::none();
        }
        if matches!(
            action,
            ActionMessage::Open
                | ActionMessage::OpenCompletionList
                | ActionMessage::OpenInlineCompletion
                | ActionMessage::OpenCompactCommand
                | ActionMessage::OpenToolFallback(_)
                | ActionMessage::Close
        ) {
            surface.cancel_inline_completion();
        }
        if matches!(action, ActionMessage::OpenInlineCompletion)
            && (surface.analysis.blocks_automatic_completion()
                || !surface.window.prepare_inline_completion())
        {
            surface.window.assist_status(
                "Place the caret at the end of an editable line to review inline suggestions."
                    .into(),
            );
            return Task::none();
        }
        if matches!(
            action,
            ActionMessage::StartAutomaticCompletion | ActionMessage::StartInlineCompletion
        ) {
            let inline = matches!(action, ActionMessage::StartInlineCompletion);
            if surface.analysis.blocks_automatic_completion() {
                return Task::none();
            }
            let approval = if inline {
                &mut surface.inline_completion
            } else {
                &mut surface.automatic_completion
            };
            let Some(reviewed) = approval.as_ref() else {
                return Task::none();
            };
            if inline
                && (!surface.assist.can_start_automatic()
                    || !surface.window.prepare_inline_completion())
            {
                return Task::none();
            }
            let result = surface.assist.start_automatic(
                &mut surface.window,
                (reviewed, inline),
                config,
                &context,
                dispatch,
            );
            return match result {
                Ok(task) => task,
                Err(error) => {
                    *approval = None;
                    if inline {
                        surface.cancel_inline_completion();
                    } else {
                        surface.assist.cancel_completion(&mut surface.window);
                    }
                    surface.window.assist_status(error);
                    Task::none()
                }
            };
        }
        let (action, enable_automatic) =
            if let ActionMessage::EnableAutomaticCompletion(cli) = action {
                let Some(approval) = surface
                    .assist
                    .0
                    .as_ref()
                    .and_then(|panel| AutomaticCompletion::from_review(panel, &context))
                else {
                    return Task::none();
                };
                (
                    cli.map_or(ActionMessage::Send, ActionMessage::RunCli),
                    Some(approval),
                )
            } else {
                (action, None)
            };
        let remembers_language = matches!(action, ActionMessage::Review)
            && surface.assist.0.as_ref().is_some_and(|panel| {
                matches!(panel.phase, Phase::Configure) && panel.recipe.action == Action::Translate
            });
        let apply_completion = matches!(action, ActionMessage::ApplyCompletion(..));
        let inline_response =
            matches!(action, ActionMessage::Ready(..)) && surface.assist.is_inline_completion();
        let send_completion = matches!(action, ActionMessage::Send | ActionMessage::RunCli(_));
        let task = surface.assist.update(
            &mut surface.window,
            action,
            config,
            &context,
            dispatch,
            &self.clipboard,
        );
        if let Some(approval) = enable_automatic
            && surface.assist.0.as_ref().is_some_and(|panel| matches!(panel.phase, Phase::Running { ticket: active, .. } if active == ticket))
        {
            if surface.assist.is_inline_completion() {
                surface.inline_completion = Some(approval);
            } else {
                surface.automatic_completion = Some(approval);
            }
        }
        if inline_response
            && let Some(panel) = surface.assist.0.as_ref()
            && matches!(panel.phase, Phase::Configure)
            && let Some(error) = panel.status.clone()
        {
            surface.cancel_inline_completion();
            surface.window.assist_status(error);
        }
        if (apply_completion
            && surface
                .assist
                .0
                .as_ref()
                .is_some_and(|panel| matches!(panel.phase, Phase::Last(..))))
            || (send_completion && surface.assist.completion_running())
        {
            return Task::batch([
                task,
                surface
                    .window
                    .focus_editor()
                    .map(move |message| Message::Snippets(id, message)),
            ]);
        }
        if remembers_language
            && let Some(panel) = surface.assist.0.as_ref()
            && matches!(panel.phase, Phase::Review(_))
            && let Some(language) = translation_language(&panel.recipe.target_language)
            && self
                .window_settings
                .snippet_translation_target_language
                .as_deref()
                != Some(language)
        {
            self.window_settings.snippet_translation_target_language = Some(language.to_owned());
            self.save_window_settings(Some(id));
        }
        task
    }

    pub(in crate::workspace) fn ssh_file_ai_origin(
        &self,
        target: &super::super::SshTarget,
        session: Option<super::super::SessionId>,
        incognito: bool,
    ) -> AiOrigin {
        AiOrigin {
            profile: self
                .saved_terminal_preferences
                .terminal(target)
                .and_then(|settings| settings.ai_profile.clone()),
            target: Some(target.clone()),
            secrets: session
                .and_then(|session| self.sessions.get(&session))
                .and_then(|pane| pane.session.journal_redaction_secrets()),
            incognito,
            local_terminal: false,
        }
    }

    pub(in crate::workspace) fn assisted_file_view<'a>(
        &'a self,
        id: window::Id,
        editor: &'a EditorWindow,
        wrap_editor: impl Fn(editor_window::Message) -> Message + 'a,
    ) -> Element<'a, Message> {
        if let Some(panel) = editor.ai_assist.view(editor, None) {
            return panel.map(move |event| Message::FileAssist(id, event));
        }
        let body = editor
            .view(
                self.editor_content_theme(),
                self.window_settings.editor_appearance(),
                self.window_settings.editor_caret,
            )
            .map(wrap_editor);
        if !editor.file_ai_available()
            || editor.ai_origin.authorize(&self.clipboard.policy).is_err()
        {
            return body;
        }
        column![
            row![
                button("AI assistance…").on_press_maybe(
                    self.ai_configuration()
                        .map(|_| Message::FileAssist(id, ActionMessage::Open))
                )
            ]
            .padding(8),
            body
        ]
        .into()
    }

    pub(in crate::workspace) fn update_file_assist(
        &mut self,
        id: window::Id,
        action: ActionMessage,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining || self.hosted_close_locks(id) {
            return Task::none();
        }
        let config = self.ai_configuration();
        let denied = self.project_remote_denial(id);
        let ticket = self.allocate();
        let vault_revision = self.vault_change_revision();
        let editor = self
            .editors
            .get_mut(&id)
            .or_else(|| self.project_files.get_mut(&id)?.editor());
        let Some(editor) = editor else {
            return Task::none();
        };
        if matches!(action, ActionMessage::Close) {
            editor.ai_assist = State::default();
            return Task::none();
        }
        if !editor.file_ai_available() {
            editor.ai_assist = State::default();
            return Task::none();
        }
        let scope = FileScope {
            origin: editor.ai_origin.clone(),
            connection: editor.remote_editor().cloned(),
            revision: editor.file_ai_revision_token(),
        };
        let edits = matches!(
            action,
            ActionMessage::Apply
                | ActionMessage::Undo
                | ActionMessage::OpenToolFallback(crate::editor_window::tools::Action::Format)
        );
        let installed = ClipboardPolicy::load();
        let authorized = denied.map_or_else(
            || {
                scope
                    .authorize(&self.clipboard.policy, edits)
                    .and_then(|()| scope.authorize(&installed, edits))
            },
            Err,
        );
        if let Err(error) = authorized {
            editor.ai_assist = State::default();
            editor.assist_status(error);
            return Task::none();
        }
        let context = Context {
            policy: &self.clipboard.policy,
            vault: self.vault.as_ref(),
            vault_revision,
            file: Some(&scope),
            snippet_translation_language: None,
            snippet_alternative_count: None,
            additional_instructions: true,
        };
        let mut state = std::mem::take(&mut editor.ai_assist);
        let before = editor.file_ai_revision();
        let task = state.update(
            editor,
            action,
            config,
            &context,
            Dispatch {
                id,
                ticket,
                file: true,
            },
            &self.clipboard,
        );
        editor.ai_assist = state;
        if editor.file_ai_revision() == before {
            return task;
        }
        let outline = editor.update(editor_window::Message::RefreshOutline);
        let outline = if self.project_files.contains_key(&id) {
            outline.map(move |event| {
                Message::ProjectFile(
                    id,
                    super::super::project_access::local_files::Message::Editor(event),
                )
            })
        } else {
            outline.map(move |event| Message::Editor(id, event))
        };
        Task::batch([task, outline])
    }

    pub(in crate::workspace) fn open_file_tool_fallback(
        &mut self,
        id: window::Id,
        mode: crate::editor_window::tools::Action,
        revision: u64,
    ) -> Task<Message> {
        let editor = self
            .editors
            .get_mut(&id)
            .or_else(|| self.project_files.get_mut(&id)?.editor());
        if editor.is_none_or(|editor| editor.file_ai_revision() != revision) {
            return Task::none();
        }
        self.update_file_assist(id, ActionMessage::OpenToolFallback(mode))
    }
}

#[derive(Clone, Copy)]
struct Dispatch {
    id: window::Id,
    ticket: u64,
    file: bool,
}

impl Dispatch {
    fn wrap(self, event: ActionMessage) -> Message {
        if self.file {
            Message::FileAssist(self.id, event)
        } else {
            wrap(self.id, event)
        }
    }

    fn receive(self, reply: async_channel::Receiver<Result<Outcome, String>>) -> Task<Message> {
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The snippet AI worker stopped.".into()))
            },
            move |result| self.wrap(ActionMessage::Ready(self.ticket, result)),
        )
    }
}

impl State {
    fn can_start_automatic(&self) -> bool {
        !self.completion_running()
            && self
                .0
                .as_ref()
                .is_none_or(|panel| panel.recipe.completion.is_some())
    }

    fn start_automatic(
        &mut self,
        editor: &mut dyn Editor,
        (approval, inline): (&AutomaticCompletion, bool),
        config: Option<Arc<Configuration>>,
        context: &Context<'_>,
        dispatch: Dispatch,
    ) -> Result<Task<Message>, String> {
        if !self.can_start_automatic() {
            return Ok(Task::none());
        }
        let config = config
            .filter(|config| approval.matches(config, context))
            .ok_or("The automatic completion approval expired. Review a new request.")?;
        let Some(mut panel) = approval.panel(editor, inline)? else {
            return Ok(Task::none());
        };
        panel.review(editor, Some(config.clone()), context, dispatch.ticket)?;
        approval.check_review(&panel)?;
        let reply = panel.start(
            editor,
            Some(&config),
            context,
            dispatch.ticket,
            Some(Box::new(approval.guard())),
        )?;
        editor.assist_status(
            if inline {
                "Waiting for an inline AI suggestion…"
            } else {
                "Waiting for automatic AI suggestions…"
            }
            .into(),
        );
        *self = State(Some(panel));
        Ok(dispatch.receive(reply))
    }

    fn update(
        &mut self,
        editor: &mut dyn Editor,
        action: ActionMessage,
        config: Option<Arc<Configuration>>,
        context: &Context<'_>,
        dispatch: Dispatch,
        clipboard: &TerminalClipboard,
    ) -> Task<Message> {
        let ticket = dispatch.ticket;
        if matches!(action, ActionMessage::Close) {
            self.cancel_completion(editor);
            *self = State::default();
            return Task::none();
        }
        if matches!(
            action,
            ActionMessage::Open
                | ActionMessage::OpenCompletionList
                | ActionMessage::OpenInlineCompletion
                | ActionMessage::OpenCompactCommand
                | ActionMessage::OpenToolFallback(_)
        ) {
            self.cancel_completion(editor);
            use crate::editor_window::tools::Action as Tool;
            let mode = match action {
                ActionMessage::OpenCompactCommand => Action::CompactCommand,
                ActionMessage::OpenToolFallback(Tool::Format) => Action::FormatCode,
                ActionMessage::OpenToolFallback(Tool::CheckSyntax) => Action::SyntaxReview,
                _ if editor.supports_ai_action(Action::default()) => Action::default(),
                _ => Action::Describe,
            };
            if !editor.supports_ai_action(mode) {
                editor.assist_status(
                    "The editor cannot use this action in its current state.".into(),
                );
                return Task::none();
            }
            let source = match editor.assist_snapshot() {
                Ok(source) => source,
                Err(error) => {
                    editor.assist_status(error);
                    return Task::none();
                }
            };
            let completion = if matches!(
                action,
                ActionMessage::OpenCompletionList | ActionMessage::OpenInlineCompletion
            ) {
                let Some(target) = editor.completion_target(
                    &source,
                    matches!(action, ActionMessage::OpenInlineCompletion),
                ) else {
                    editor.assist_status(
                        "Open the completion list at an editable caret first.".into(),
                    );
                    return Task::none();
                };
                Some(target)
            } else {
                None
            };
            let mut panel = Panel {
                language_mix: source.language_mix(),
                staged_migration: None,
                source,
                recipe: Recipe {
                    action: mode,
                    completion,
                    target_language: context
                        .snippet_translation_language
                        .and_then(translation_language)
                        .unwrap_or_default()
                        .to_owned(),
                    alternative_count: context.snippet_alternative_count,
                    ..Recipe::default()
                },
                profiles: Vec::new(),
                explicit_profile: false,
                skills: Vec::new(),
                phase: Phase::Configure,
                status: None,
            };
            panel.configure(config.as_deref(), context.policy, true);
            if let Some(config) = &config {
                panel.recipe.skills =
                    config.suggest_snippet_skills(&panel.source.language, &panel.source.source);
            }
            *self = State(Some(panel));
            return Task::none();
        }
        let Some(panel) = self.0.as_mut() else {
            return Task::none();
        };
        let opens_review = matches!(
            action,
            ActionMessage::Review
                | ActionMessage::Rerun
                | ActionMessage::ReviewSecurityAgain(_)
                | ActionMessage::ReviewMigratedSecurityFix(_)
        );
        let result = match action {
            ActionMessage::Choose(action)
                if matches!(panel.phase, Phase::Configure)
                    && panel.recipe.completion.is_none()
                    && editor.supports_ai_action(action)
                    && (action != Action::SecurityFix || panel.recipe.findings.is_some())
                    && (action != Action::MigrateScript || panel.can_migrate_script()) =>
            {
                if panel.recipe.action != action {
                    panel.recipe.migration_language = None;
                    panel.recipe.migration_platform = None;
                }
                panel.recipe.action = action;
                if action == Action::SecurityReview {
                    panel.recipe.findings = None;
                }
                panel.recipe.approved_transport = None;
                panel.recipe.approved_cli_revision = None;
                panel.configure(config.as_deref(), context.policy, true);
                Ok(())
            }
            ActionMessage::Profile(profile)
                if matches!(panel.phase, Phase::Configure)
                    && panel.profiles.iter().any(|choice| choice.id == profile) =>
            {
                panel.recipe.profile = Some(profile);
                panel.explicit_profile = true;
                panel.recipe.approved_transport = None;
                panel.recipe.approved_cli_revision = None;
                Ok(())
            }
            ActionMessage::TargetLanguage(value)
                if matches!(panel.phase, Phase::Configure)
                    && value.len() <= MAX_TARGET_LANGUAGE_BYTES =>
            {
                panel.recipe.target_language = value;
                Ok(())
            }
            ActionMessage::MigrationLanguage(language)
                if matches!(panel.phase, Phase::Configure)
                    && panel.offers_migration()
                    && panel.migration_targets().contains(&language) =>
            {
                panel.recipe.migration_language = Some(language);
                Ok(())
            }
            ActionMessage::MigrationPlatform(platform)
                if matches!(panel.phase, Phase::Configure)
                    && panel.offers_migration()
                    && platform.is_none_or(|host| panel.migration_platforms().contains(&host)) =>
            {
                panel.recipe.migration_platform = platform;
                let targets = panel.migration_targets();
                panel.recipe.migration_language = panel
                    .recipe
                    .migration_language
                    .filter(|language| targets.contains(language));
                Ok(())
            }
            ActionMessage::ClearMigrationLanguage
                if matches!(panel.phase, Phase::Configure) && panel.offers_migration() =>
            {
                panel.recipe.migration_language = None;
                Ok(())
            }
            ActionMessage::Instructions(value)
                if matches!(panel.phase, Phase::Configure)
                    && context.additional_instructions
                    && value.len() <= 16 * 1024 =>
            {
                panel.recipe.instructions = value;
                Ok(())
            }
            ActionMessage::Skill(id, selected)
                if matches!(panel.phase, Phase::Configure)
                    && panel.skills.iter().any(|(key, _)| *key == id) =>
            {
                if selected && panel.recipe.skills.len() == 32 && !panel.recipe.skills.contains(&id)
                {
                    Err("Select at most 32 AI skills per request.".into())
                } else {
                    panel.recipe.skills.retain(|&key| key != id);
                    if selected {
                        panel.recipe.skills.push(id);
                    }
                    Ok(())
                }
            }
            ActionMessage::Review if matches!(panel.phase, Phase::Configure) => {
                panel.review(editor, config, context, ticket)
            }
            ActionMessage::QuotaFinished(token, forecast) => {
                if let Phase::Review(review) = &mut panel.phase {
                    review.quota.complete(token, forecast);
                }
                return Task::none();
            }
            action @ (ActionMessage::Send | ActionMessage::RunCli(_))
                if let Phase::Review(review) = &panel.phase =>
            {
                let approval = match action {
                    ActionMessage::RunCli(ticket) => Some(ticket),
                    _ => None,
                };
                if review.cli.as_ref().map(|cli| cli.ticket) != approval {
                    return Task::none();
                }
                if review.quota.loading() {
                    return Task::none();
                }
                if review.quota.stale() {
                    panel.status =
                        Some("Review the refreshed quota forecast before sending.".into());
                    return panel.start_quota(dispatch);
                }
                match panel.start(editor, config.as_ref(), context, ticket, None) {
                    Ok(reply) => {
                        panel.status = None;
                        if panel.recipe.completion.is_some() {
                            editor.assist_status("Waiting for reviewed AI suggestions…".into());
                        }
                        return dispatch.receive(reply);
                    }
                    Err(error) => Err(error),
                }
            }
            ActionMessage::Ready(ticket, result) if matches!(panel.phase,Phase::Running{ticket:active,..} if active==ticket) =>
            {
                let Phase::Running {
                    config: approved,
                    outbound,
                    skill_usage,
                    ..
                } = std::mem::replace(&mut panel.phase, Phase::Configure)
                else {
                    unreachable!("checked ticket");
                };
                if config
                    .as_ref()
                    .is_none_or(|config| !Arc::ptr_eq(config, &approved))
                {
                    Err("The AI configuration changed while the request was running.".into())
                } else if !panel.current(editor) {
                    Err(
                        "The editor changed while AI assistance was running. Review a new request."
                            .into(),
                    )
                } else {
                    result.and_then(|outcome| {
                        if let Some(outbound) = &outbound {
                            outbound.require_current(&approved, context)?;
                            if context.vault.is_some_and(|vault| {
                                vault.summary_contains_secret(outcome.proposed_text())
                                    || outcome.migration.as_ref().is_some_and(|migration| {
                                        migration.notes.iter().any(|note| vault.summary_contains_secret(note))
                                    })
                                    || outcome.findings.iter().flat_map(security::Finding::fields)
                                        .any(|field| vault.summary_contains_secret(field))
                                    || outcome.alternatives.iter().any(|alternative| {
                                        vault.summary_contains_secret(&alternative.title)
                                            || vault.summary_contains_secret(&alternative.summary)
                                            || vault.summary_contains_secret(alternative.outcome.proposed_text())
                                    })
                            }) {
                                return Err("The AI result contains a vault credential. Review a new request.".into());
                            }
                        }
                        let diff = outcome
                            .change
                            .as_ref()
                            .map(|(_, diff)| diff_view(diff.clone()));
                        if let Some(receipt) = skill_usage {
                            receipt.record();
                        }
                        if panel.recipe.completion.is_some() {
                            let count = editor.publish_completions(ticket, &outcome.alternatives);
                            editor.assist_status(if count == 0 {
                                "No matching AI suggestions.".into()
                            } else {
                                format!("AI suggestions added · {count}")
                            });
                        }
                        let alternative_previews = outcome
                            .alternatives
                            .iter()
                            .map(|alternative| text_editor::Content::with_text(alternative.outcome.proposed_text()))
                            .collect();
                        panel.phase = Phase::Result(Box::new(Proposed {
                            ticket,
                            config: approved,
                            outbound,
                            outcome,
                            diff,
                            alternative: None,
                            alternative_previews,
                            expanded_alternative: None,
                            selected_findings: BTreeSet::new(),
                            summary_drag: None,
                        }));
                        Ok(())
                    })
                }
            }
            ActionMessage::ChooseAlternative(choice) => {
                if let Phase::Result(proposed) = &mut panel.phase
                    && !proposed.outcome.alternatives.is_empty()
                    && choice.is_none_or(|index| index < proposed.outcome.alternatives.len())
                {
                    proposed.alternative = choice;
                    proposed.summary_drag = None;
                    proposed.diff = proposed
                        .selected_outcome()
                        .change
                        .as_ref()
                        .map(|(_, diff)| diff_view(diff.clone()));
                }
                Ok(())
            }
            ActionMessage::ExpandAlternative(result_ticket, index) => {
                if let Phase::Result(result) = &mut panel.phase
                    && result.alternative_visible(result_ticket, index)
                {
                    result.expanded_alternative =
                        (result.expanded_alternative != Some(index)).then_some(index);
                }
                return Task::none();
            }
            ActionMessage::SelectAlternativeCode(result_ticket, index, selection) => {
                if let Phase::Result(result) = &mut panel.phase
                    && result.alternative_visible(result_ticket, index)
                    && !selection.is_edit()
                {
                    result.alternative_previews[index].perform(selection);
                }
                return Task::none();
            }
            ActionMessage::CopyAlternativeCode(result_ticket, index) => {
                let Phase::Result(result) = &panel.phase else {
                    return Task::none();
                };
                if !result.alternative_visible(result_ticket, index) {
                    return Task::none();
                }
                let Some(selection) = result.alternative_previews[index]
                    .selection()
                    .filter(|text| !text.is_empty())
                else {
                    return Task::none();
                };
                if !allows_copy(context) {
                    Err("Managed policy blocks writing the clipboard.".into())
                } else if !panel.current(editor)
                    || config
                        .as_ref()
                        .is_none_or(|config| !Arc::ptr_eq(config, &result.config))
                {
                    Err("The source or AI configuration changed. Review a new request.".into())
                } else if let Some(error) = result
                    .outbound
                    .as_ref()
                    .and_then(|outbound| outbound.require_current(&result.config, context).err())
                {
                    Err(error)
                } else if context
                    .vault
                    .is_some_and(|vault| vault.summary_contains_secret(&selection))
                {
                    Err("The selection contains a vault credential.".into())
                } else {
                    panel.status = Some("Alternative selection copied.".into());
                    return super::super::clipboard_access::write_value(clipboard, selection);
                }
            }
            ActionMessage::Apply => {
                if let Phase::Result(proposed) = &panel.phase
                    && panel.recipe.completion.is_none()
                    && proposed.diff.is_some()
                    && !(panel.recipe.action == Action::SecurityFix
                        && proposed
                            .outcome
                            .migration
                            .as_ref()
                            .is_some_and(|migration| !migration.security_fixes))
                {
                    if config
                        .as_ref()
                        .is_none_or(|config| !Arc::ptr_eq(config, &proposed.config))
                    {
                        panel.status =
                            Some("The AI configuration changed. Review a new request.".into());
                        return Task::none();
                    }
                    if let Some(outbound) = &proposed.outbound
                        && let Err(error) = outbound.require_current(&proposed.config, context)
                    {
                        panel.status = Some(error);
                        return Task::none();
                    }
                    editor
                        .accept_ai_change(
                            panel.source.clone(),
                            panel.recipe.clone(),
                            proposed.selected_outcome(),
                        )
                        .map(|()| {
                            if let Some(change) = editor.last_ai_change() {
                                panel.phase =
                                    Phase::Last(change.clone(), diff_view(change.diff.clone()));
                            }
                        })
                } else {
                    Ok(())
                }
            }
            ActionMessage::ApplyCompletion(result_ticket, index) => {
                if let Phase::Result(proposed) = &panel.phase
                    && panel.recipe.completion.is_some()
                    && proposed.ticket == result_ticket
                    && editor.can_apply_completion(result_ticket, index)
                    && let Some(candidate) = proposed.outcome.alternatives.get(index)
                {
                    let result = (|| {
                        if !panel.current(editor)
                            || config
                                .as_ref()
                                .is_none_or(|config| !Arc::ptr_eq(config, &proposed.config))
                        {
                            return Err(
                                "The source or AI configuration changed. Request new suggestions."
                                    .into(),
                            );
                        }
                        if let Some(outbound) = &proposed.outbound {
                            outbound.require_current(&proposed.config, context)?;
                        }
                        editor.accept_ai_change(
                            panel.source.clone(),
                            panel.recipe.clone(),
                            &candidate.outcome,
                        )
                    })();
                    match result {
                        Ok(()) => {
                            if let Some(change) = editor.last_ai_change() {
                                panel.phase =
                                    Phase::Last(change.clone(), diff_view(change.diff.clone()));
                            }
                            editor.assist_status("AI completion inserted.".into());
                        }
                        Err(error) => {
                            editor.clear_completion_candidates();
                            editor.assist_status(error);
                        }
                    }
                }
                return Task::none();
            }
            ActionMessage::CopyCommand => {
                let Phase::Result(proposed) = &panel.phase else {
                    return Task::none();
                };
                if panel.recipe.action != Action::CompactCommand || proposed.diff.is_some() {
                    return Task::none();
                }
                if !allows_copy(context) {
                    Err("Managed policy blocks writing the clipboard.".into())
                } else if !panel.current(editor)
                    || config
                        .as_ref()
                        .is_none_or(|config| !Arc::ptr_eq(config, &proposed.config))
                {
                    Err("The snippet or AI configuration changed. Review a new request.".into())
                } else if let Some(error) = proposed
                    .outbound
                    .as_ref()
                    .and_then(|outbound| outbound.require_current(&proposed.config, context).err())
                {
                    Err(error)
                } else if context.vault.is_some_and(|vault| {
                    vault.summary_contains_secret(proposed.outcome.proposed_text())
                }) {
                    Err("The proposed command contains a vault credential.".into())
                } else {
                    let command = proposed.outcome.proposed_text().to_owned();
                    panel.status = Some("Reviewed snippet command copied.".into());
                    return super::super::clipboard_access::write_value(clipboard, command);
                }
            }
            ActionMessage::ShowLast => {
                if let Some(change) = editor.last_ai_change() {
                    panel.phase = Phase::Last(change.clone(), diff_view(change.diff.clone()));
                }
                Ok(())
            }
            ActionMessage::Undo => {
                if let Phase::Last(change, _) = &panel.phase {
                    editor
                        .undo_ai_change(change)
                        .and_then(|()| panel.back(editor))
                } else {
                    Ok(())
                }
            }
            ActionMessage::Rerun if let Phase::Last(change, _) = &panel.phase => {
                let change = change.clone();
                editor
                    .assist_snapshot()
                    .and_then(|source| change.rerun_source(source))
                    .and_then(|source| {
                        panel.recipe = change.recipe.clone();
                        panel.language_mix = source.language_mix();
                        panel.source = source;
                        panel.phase = Phase::Configure;
                        panel.review(editor, config, context, ticket)
                    })
            }
            ActionMessage::SelectFinding(ticket, index, selected) => {
                if let Phase::Result(result) = &mut panel.phase
                    && result.ticket == ticket
                    && index < result.outcome.findings.len()
                {
                    if selected {
                        result.selected_findings.insert(index);
                    } else {
                        result.selected_findings.remove(&index);
                    }
                }
                Ok(())
            }
            ActionMessage::SelectAllFindings(ticket, selected) => {
                if let Phase::Result(result) = &mut panel.phase
                    && result.ticket == ticket
                {
                    result.selected_findings = if selected {
                        (0..result.outcome.findings.len()).collect()
                    } else {
                        BTreeSet::new()
                    };
                }
                Ok(())
            }
            ActionMessage::Fix(ticket) => {
                if let Phase::Result(result) = &panel.phase
                    && result.ticket == ticket
                    && panel.recipe.action == Action::SecurityReview
                    && editor.supports_ai_action(Action::SecurityFix)
                    && panel.current(editor)
                    && config
                        .as_ref()
                        .is_some_and(|config| Arc::ptr_eq(config, &result.config))
                {
                    if let Some(outbound) = &result.outbound
                        && let Err(error) = outbound.require_current(&result.config, context)
                    {
                        Err(error)
                    } else {
                        security::Selection::new(
                            panel.source.clone(),
                            result
                                .selected_findings
                                .iter()
                                .filter_map(|&index| result.outcome.findings.get(index)),
                        )
                        .map(|selection| {
                            panel.recipe.findings = Some(selection);
                            panel.recipe.action = Action::SecurityFix;
                            panel.recipe.approved_transport = None;
                            panel.recipe.approved_cli_revision = None;
                            panel.phase = Phase::Configure;
                            panel.configure(config.as_deref(), context.policy, false);
                        })
                    }
                } else {
                    Ok(())
                }
            }
            ActionMessage::ReviewSecurityAgain(result_ticket)
                if panel.recipe.action == Action::SecurityReview
                    && matches!(&panel.phase, Phase::Result(result) if result.ticket == result_ticket) =>
            {
                panel
                    .back(editor)
                    .and_then(|()| panel.review(editor, config, context, ticket))
            }
            ActionMessage::ReviewMigratedSecurityFix(result_ticket)
                if panel.recipe.action == Action::SecurityFix
                    && let Phase::Result(result) = &panel.phase
                    && result.ticket == result_ticket
                    && result
                        .outcome
                        .migration
                        .as_ref()
                        .is_some_and(|migration| !migration.security_fixes)
                    && config
                        .as_ref()
                        .is_some_and(|config| Arc::ptr_eq(config, &result.config)) =>
            {
                let authority = result.outbound.as_ref().map_or(Ok(()), |outbound| {
                    outbound.require_current(&result.config, context)
                });
                let migrated = result.outcome.clone();
                authority.and_then(|()| {
                    panel.staged_migration = Some(migrated);
                    if let Err(error) = panel.review(editor, config, context, ticket) {
                        panel.staged_migration = None;
                        return Err(error);
                    }
                    Ok(())
                })
            }
            ActionMessage::Back => panel.back(editor),
            ActionMessage::Diff(message) => {
                let view = match &mut panel.phase {
                    Phase::Result(result) => match &mut result.diff {
                        Some(view) => view,
                        None => return Task::none(),
                    },
                    Phase::Last(_, view) => view,
                    _ => return Task::none(),
                };
                return view
                    .update(message)
                    .map(move |event| dispatch.wrap(ActionMessage::Diff(event)));
            }
            _ => return Task::none(),
        };
        panel.status = result.err();
        if matches!(panel.phase, Phase::Configure) {
            panel.staged_migration = None;
        }
        if opens_review && panel.status.is_none() {
            panel.start_quota(dispatch)
        } else {
            Task::none()
        }
    }
}

impl Panel {
    fn offers_migration(&self) -> bool {
        self.recipe.action == Action::MigrateScript
            || self.recipe.action == Action::SecurityFix && self.can_migrate_script()
    }

    fn can_migrate_script(&self) -> bool {
        !self.migration_targets().is_empty() || !self.migration_platforms().is_empty()
    }

    fn migration_targets(&self) -> Vec<migration::Language> {
        if let Some(host) = self.recipe.migration_platform {
            return migration::Language::platform_targets(host);
        }
        self.language_mix
            .as_ref()
            .map_or_else(Vec::new, migration::Language::available)
    }

    fn migration_platforms(&self) -> Vec<HostFormat> {
        let Some(source) = self
            .language_mix
            .as_ref()
            .and_then(|mix| mix.host)
            .filter(|host| HostFormat::CONVERTIBLE.contains(host))
        else {
            return Vec::new();
        };
        HostFormat::CONVERTIBLE
            .into_iter()
            .filter(|host| *host != source)
            .collect()
    }

    fn configure(
        &mut self,
        config: Option<&Configuration>,
        policy: &ClipboardPolicy,
        prefer_role: bool,
    ) {
        self.profiles = config
            .map(|config| {
                config
                    .snippet_assistance_profiles()
                    .map(|(id, name, model, destination, remote)| Choice {
                        id,
                        name: name.into(),
                        model: model.map(str::to_owned),
                        destination,
                        route: if remote { Route::Https } else { Route::Local },
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(config) = config.filter(|_| cli::allowed(policy)) {
            self.profiles
                .extend(config.snippet_analysis_cli_profiles().map(
                    |(id, name, model, destination)| Choice {
                        id,
                        name: name.into(),
                        model: Some(model.into()),
                        destination,
                        route: Route::Cli,
                    },
                ));
        }
        self.skills = config
            .map(Configuration::snippet_skill_choices)
            .unwrap_or_default();
        self.recipe
            .skills
            .retain(|id| self.skills.iter().any(|(available, _)| available == id));
        if prefer_role {
            let available = |id| self.profiles.iter().any(|choice| choice.id == id);
            self.explicit_profile &= self.recipe.profile.is_some_and(available);
            if !self.explicit_profile {
                self.recipe.profile = config
                    .and_then(|config| {
                        config.preferred_snippet_assistance_profile_id(
                            self.recipe.action,
                            self.source.connection_profile.as_ref(),
                            available,
                        )
                    })
                    .or_else(|| self.profiles.first().map(|choice| choice.id));
            }
        }
        self.status = self
            .profiles
            .is_empty()
            .then(|| "Enable AI features and configure an available profile in AI Manager.".into());
    }

    fn current(&self, editor: &dyn Editor) -> bool {
        self.recipe
            .completion
            .as_ref()
            .is_none_or(|target| editor.completion_current(target))
            && editor
                .assist_snapshot()
                .is_ok_and(|current| self.source.same_source(&current))
    }

    fn back(&mut self, editor: &dyn Editor) -> Result<(), String> {
        if self.recipe.completion.is_some() && !self.current(editor) {
            return Err("Open the completion list again for the current caret.".into());
        }
        self.phase = Phase::Configure;
        self.staged_migration = None;
        self.source = editor.assist_snapshot()?;
        self.language_mix = self.source.language_mix();
        if self
            .recipe
            .findings
            .as_ref()
            .is_some_and(|findings| !findings.matches(&self.source))
        {
            self.recipe.findings = None;
            if self.recipe.action == Action::SecurityFix {
                self.recipe.action = Action::SecurityReview;
                self.recipe.approved_transport = None;
                self.recipe.approved_cli_revision = None;
            }
        }
        Ok(())
    }

    fn review(
        &mut self,
        editor: &dyn Editor,
        config: Option<Arc<Configuration>>,
        context: &Context<'_>,
        ticket: u64,
    ) -> Result<(), String> {
        if !self.current(editor) {
            return Err("The source changed. Reopen AI assistance for the current draft.".into());
        }
        if !editor.supports_ai_action(self.recipe.action) {
            return Err("The editor cannot use this action in its current state.".into());
        }
        if !context.additional_instructions {
            self.recipe.instructions.clear();
        }
        self.configure(config.as_deref(), context.policy, false);
        let config = config.ok_or("AI features are disabled or unavailable.")?;
        let choice = self
            .profiles
            .iter()
            .find(|choice| Some(choice.id) == self.recipe.profile)
            .cloned()
            .ok_or("The original AI profile is unavailable. Select a ready profile.")?;
        let (transport, limit) = choice.transport(&config)?;
        transport.require_current_policy_authority()?;
        if self
            .recipe
            .approved_transport
            .as_ref()
            .is_some_and(|original| original != &transport)
        {
            return Err("The original AI profile changed. Start a new reviewed request.".into());
        }
        if let Some(scope) = context.file {
            scope.authorize(context.policy, self.recipe.action.edits_file())?;
        }
        let (request, _) = self.masked_request(&config, &transport, limit, context)?;
        let (outbound, cli) = match choice.route {
            Route::Local => (None, None),
            Route::Https => (
                Some(Outbound::Https(HttpsReview::capture(
                    &context.policy.terminal_logging,
                    context.vault,
                    transport.clone(),
                ))),
                None,
            ),
            Route::Cli => {
                let review = cli::Review::capture(&config, &choice, context)?;
                if self
                    .recipe
                    .approved_cli_revision
                    .is_some_and(|original| original != review.revision())
                {
                    return Err(
                        "The original AI profile changed. Start a new reviewed request.".into(),
                    );
                }
                let cli = cli::Prepared::new(ticket, &transport, &request)?;
                (Some(Outbound::Cli(review)), Some(cli))
            }
        };
        self.phase = Phase::Review(Box::new(Review {
            config,
            choice,
            transport,
            request,
            outbound,
            cli,
            quota: ai_quota::State::default(),
        }));
        Ok(())
    }

    fn start_quota(&mut self, dispatch: Dispatch) -> Task<Message> {
        let ticket = dispatch.ticket;
        let Phase::Review(review) = &mut self.phase else {
            return Task::none();
        };
        review.quota.start_messages(
            ticket,
            &review.transport,
            [
                review.request.system.as_ref(),
                review.request.input.as_ref(),
            ],
            move |forecast| dispatch.wrap(ActionMessage::QuotaFinished(ticket, forecast)),
        )
    }

    fn start(
        &mut self,
        editor: &dyn Editor,
        config: Option<&Arc<Configuration>>,
        context: &Context<'_>,
        ticket: u64,
        automatic: Option<DispatchGuard>,
    ) -> Result<async_channel::Receiver<Result<Outcome, String>>, String> {
        let Phase::Review(review) = std::mem::replace(&mut self.phase, Phase::Configure) else {
            return Err("Review the request before sending it.".into());
        };
        if !self.current(editor)
            || !editor.supports_ai_action(self.recipe.action)
            || config.is_none_or(|config| !Arc::ptr_eq(config, &review.config))
        {
            return Err("The source or AI configuration changed. Review the request again.".into());
        }
        if review
            .outbound
            .as_ref()
            .is_some_and(|outbound| !outbound.matches(context))
        {
            return Err("The vault or masking policy changed. Review the request again.".into());
        }
        let (transport, limit) = review.choice.transport(&review.config)?;
        transport.require_current_policy_authority()?;
        let file = context.file.cloned();
        let edits = self.recipe.action.edits_file();
        if let Some(scope) = &file {
            scope.authorize(context.policy, edits)?;
        }
        let (request, guidance) =
            self.masked_request(&review.config, &transport, limit, context)?;
        if transport != review.transport
            || request.system != review.request.system
            || request.input != review.request.input
        {
            return Err("The profile or masked request changed. Review the request again.".into());
        }
        let saved = Arc::clone(&review.config);
        let skill_usage = request
            .system
            .contains(&guidance.text)
            .then(|| {
                review
                    .config
                    .skill_usage(&guidance, &transport, SkillUsageSurface::Workflow, false)
            })
            .flatten();
        let authorize_dispatch = move || {
            authorize_file(&file, edits)?;
            automatic.as_ref().map_or(Ok(()), |guard| guard())
        };
        let Pending { reply, cancel } = match (&review.outbound, review.cli) {
            (Some(Outbound::Cli(cli)), Some(prepared)) => {
                cli.require_current(&saved, context)?;
                let guard = cli.guard(saved);
                request.start_cli(transport.clone(), prepared.request, move || {
                    authorize_dispatch()?;
                    guard()
                })
            }
            (Some(Outbound::Https(https)), None) => {
                let https = https.clone();
                request.start(transport.clone(), move || {
                    authorize_dispatch()?;
                    https.authorize(&saved)
                })
            }
            (None, None) => request.start(transport.clone(), authorize_dispatch),
            _ => {
                return Err(
                    "The reviewed request no longer matches the selected AI profile.".into(),
                );
            }
        }?;
        self.recipe.approved_transport = Some(transport);
        self.recipe.approved_cli_revision = match &review.outbound {
            Some(Outbound::Cli(cli)) => Some(cli.revision()),
            _ => None,
        };
        self.phase = Phase::Running {
            ticket,
            config: review.config,
            outbound: review.outbound,
            skill_usage,
            _cancel: cancel,
        };
        Ok(reply)
    }

    fn masked_request(
        &self,
        config: &Configuration,
        transport: &Profile,
        limit: usize,
        context: &Context<'_>,
    ) -> Result<(Request, SkillGuidance), String> {
        let mut recipe = self.recipe.clone();
        if recipe.action == Action::SecurityFix
            && recipe.includes_migration()
            && self.staged_migration.is_none()
        {
            recipe.action = Action::MigrateScript;
        }
        if !context.additional_instructions {
            recipe.instructions.clear();
        } else if context.file.is_none() {
            recipe.instructions = recipe.instructions.trim().to_owned();
        }
        let policy = &context.policy.terminal_logging;
        let vault = context.vault;
        let guidance = config.reviewed_snippet_skill_guidance(&self.recipe.skills);
        let secrets = Secrets::default();
        let outbound = transport.is_remote() || transport.is_cli();
        let cli_vault = vault
            .filter(|_| transport.is_cli())
            .map(VaultWindow::journal_question_secrets);
        if outbound {
            if let Some(credential) = transport.completion_secret() {
                secrets.add(&credential);
            }
            if let Some(vault) = vault.filter(|_| transport.is_remote()) {
                for text in [
                    self.source.name.as_str(),
                    self.source.language.as_str(),
                    self.source.source.as_ref(),
                    self.source.description(),
                    recipe.instructions.as_str(),
                    self.recipe.target_language.as_str(),
                    guidance.text.as_str(),
                ]
                .into_iter()
                .chain(
                    self.staged_migration
                        .iter()
                        .map(|migration| migration.text.as_ref()),
                )
                .chain(
                    self.recipe
                        .findings
                        .iter()
                        .flat_map(|selection| &selection.findings)
                        .flat_map(security::Finding::fields),
                ) {
                    vault.add_summary_secrets(text, &secrets);
                }
            }
        }
        let secrets = secrets
            .seal()
            .map_err(|_| "Could not safely mask the snippet AI request.")?;
        let redactor = if let Some(session_secrets) =
            context.file.and_then(|file| file.origin.secrets.clone())
        {
            Redactor::for_journal(session_secrets, policy.clone())
                .with_overlay(&secrets)
                .map_err(|_| "Could not safely mask the file AI request.")?
        } else {
            Redactor::for_journal(secrets, policy.clone())
        };
        let mask: crate::snippet_window::ai_assist::Mask = Arc::new(move |text: &str| {
            let overlay = cli_vault
                .as_ref()
                .map(|vault| {
                    let secrets = Secrets::default();
                    vault.add_matches(text, &secrets);
                    redactor.with_overlay(&secrets.seal()?)
                })
                .transpose()
                .map_err(|_| "Could not safely mask the snippet AI request.")?;
            super::super::terminal_ai::mask::prompt_counted(
                overlay.as_ref().unwrap_or(&redactor),
                text,
            )
            .map(|(masked, _)| masked)
        });
        let prose_language = config.code_text_language().explicit_code();
        let mut request = if let Some(migrated) = &self.staged_migration {
            Request::with_migration(
                self.source.clone(),
                &recipe,
                prose_language,
                &guidance.text,
                limit,
                Arc::clone(&mask),
                Some(migrated),
            )
        } else {
            Request::new(
                self.source.clone(),
                &recipe,
                prose_language,
                &guidance.text,
                limit,
                Arc::clone(&mask),
            )
        }?;
        let suffix = transport.structured_system_suffix();
        let base = suffix.map(|suffix| format!("{}{suffix}", request.system));
        let rendered = transport.render_system_prompt(base.as_deref().unwrap_or(&request.system));
        if outbound || suffix.is_some() || matches!(rendered, std::borrow::Cow::Owned(_)) {
            let system = mask(&rendered)?;
            let system = if transport.is_remote()
                && let Some(vault) = vault
            {
                let secrets = Secrets::default();
                vault.add_summary_secrets(&system, &secrets);
                let redactor = Redactor::for_journal(
                    secrets
                        .seal()
                        .map_err(|_| "Could not safely mask the snippet AI system prompt.")?,
                    policy.clone(),
                );
                super::super::terminal_ai::mask::prompt_counted(&redactor, &system)?.0
            } else {
                system
            };
            if suffix.is_some_and(|suffix| !system.ends_with(suffix)) {
                return Err("Masking changed a required AI provider instruction.".into());
            }
            if transport.is_remote()
                && system.len().saturating_add(request.input.len()) > MAX_REMOTE_REQUEST_BYTES
            {
                return Err(
                    "The reviewed HTTPS prompts exceed 512 KiB. Reduce the context.".into(),
                );
            }
            if transport.is_cli()
                && system
                    .chars()
                    .count()
                    .saturating_add(request.input.chars().count())
                    > limit
            {
                return Err(
                    "The reviewed CLI prompts exceed the AI profile's selection limit.".into(),
                );
            }
            if system.len().saturating_add(request.input.len()) > 4 * 1024 * 1024 {
                return Err("The reviewed AI request exceeds the 4 MiB payload limit.".into());
            }
            request.system = system.into();
        }
        Ok((request, guidance))
    }
}

fn authorize_file(file: &Option<FileScope>, edits: bool) -> Result<(), String> {
    if let Some(file) = file {
        file.authorize(&ClipboardPolicy::load(), edits)?;
    }
    Ok(())
}

fn allows_copy(context: &Context<'_>) -> bool {
    context.policy.write_allowed
        && context.policy.problem.is_none()
        && context.file.is_none_or(|_| {
            let installed = ClipboardPolicy::load();
            installed.problem.is_none()
                && installed.write_allowed
                && (!installed.internal || context.policy.internal)
        })
}

fn diff_view(diff: Arc<compare::Comparison>) -> Box<compare::View> {
    let mut view = compare::View::default();
    let _ = view.update(compare::Message::Loaded(Ok(diff)));
    Box::new(view)
}

fn wrap(id: window::Id, event: ActionMessage) -> Message {
    Message::Snippets(id, snippet_window::Message::Assist(event))
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;
    use std::io::Write as _;
    use std::time::Duration;

    use iced::futures::{StreamExt, executor::block_on};
    use iced::widget::text_editor::{Action as EditorAction, Edit};
    use serde_json::json;

    use crate::local_ai::test_support::{accept, fixture_response, listener, request};
    use crate::snippets::tests::Profile as Scratch;

    use super::*;

    #[test]
    fn alternatives_expand_copy_and_open_diff_without_changing_source() {
        use crate::snippet_window::ai_assist::{Alternative, Target};

        let scratch = Scratch::new();
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let original = "echo original";
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new(original.into())),
        )));
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(Configuration::default());
        workspace.clipboard.policy.internal = true;
        let config = workspace.ai_configuration().unwrap();
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| workspace.update_snippet_assist(id, action);
        let _ = send(&mut workspace, ActionMessage::Open);
        let make = |code: &str| {
            let diff = compare::Request::texts(
                ("Before".into(), original.into()),
                ("After".into(), code.into()),
            )
            .compare()
            .unwrap();
            Alternative {
                title: code.into(),
                summary: String::new(),
                outcome: Outcome {
                    text: Arc::from(code),
                    change: Some((Target::Source, Arc::new(diff))),
                    selection: None,
                    alternatives: Vec::new(),
                    findings: Vec::new(),
                    migration: None,
                },
            }
        };
        let alternatives = vec![make("echo first"), make("echo second")];
        let alternative_previews = alternatives
            .iter()
            .map(|alternative| text_editor::Content::with_text(alternative.outcome.proposed_text()))
            .collect();
        let panel = workspace
            .snippets
            .get_mut(&id)
            .unwrap()
            .assist
            .0
            .as_mut()
            .unwrap();
        panel.recipe.action = Action::Alternatives;
        panel.phase = Phase::Result(Box::new(Proposed {
            ticket: 42,
            config,
            outbound: None,
            outcome: Outcome {
                text: Arc::from(""),
                change: None,
                selection: None,
                alternatives,
                findings: Vec::new(),
                migration: None,
            },
            diff: None,
            alternative: None,
            alternative_previews,
            expanded_alternative: None,
            selected_findings: BTreeSet::new(),
            summary_drag: None,
        }));

        let _ = send(&mut workspace, ActionMessage::FontSize(FontView::Diff, 50));
        let _ = send(
            &mut workspace,
            ActionMessage::FontSize(FontView::Alternatives, 25),
        );
        assert_eq!(
            workspace.window_settings.snippet_ai_font(FontView::Diff),
            14
        );
        assert_eq!(
            workspace
                .window_settings
                .snippet_ai_font(FontView::Alternatives),
            25
        );
        let _ = send(&mut workspace, ActionMessage::ExpandAlternative(42, 1));
        let _ = send(
            &mut workspace,
            ActionMessage::SelectAlternativeCode(42, 0, EditorAction::SelectAll),
        );
        let _ = send(&mut workspace, ActionMessage::CopyAlternativeCode(42, 0));
        assert!(workspace.clipboard.buffer.get().is_none());
        let _ = send(
            &mut workspace,
            ActionMessage::SelectAlternativeCode(42, 1, EditorAction::SelectAll),
        );
        let _ = send(
            &mut workspace,
            ActionMessage::SelectAlternativeCode(
                42,
                1,
                EditorAction::Edit(Edit::Paste(Arc::new("unapproved".into()))),
            ),
        );
        workspace.clipboard.policy.write_allowed = false;
        let _ = send(&mut workspace, ActionMessage::CopyAlternativeCode(42, 1));
        assert!(workspace.clipboard.buffer.get().is_none());
        workspace.clipboard.policy.write_allowed = true;
        let _ = send(&mut workspace, ActionMessage::CopyAlternativeCode(42, 1));
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("echo second")
        );
        let _ = send(&mut workspace, ActionMessage::ChooseAlternative(Some(1)));
        let _ = send(&mut workspace, ActionMessage::FontSize(FontView::Diff, 49));
        assert!(
            workspace
                .snippet_shortcut(
                    id,
                    &iced::keyboard::Key::Character("=".into()),
                    iced::keyboard::Modifiers::CTRL,
                )
                .is_some()
        );
        let _ = send(&mut workspace, ActionMessage::FontSize(FontView::Diff, 73));
        assert_eq!(
            workspace.window_settings.snippet_ai_font(FontView::Diff),
            50
        );
        assert_eq!(
            workspace
                .window_settings
                .snippet_ai_font(FontView::Alternatives),
            25
        );
        assert_eq!(
            workspace.window_settings.snippet_ai_font(FontView::Review),
            14
        );
        workspace
            .snippets
            .get_mut(&id)
            .unwrap()
            .assist
            .0
            .as_mut()
            .unwrap()
            .recipe
            .action = Action::CorrectDescription;
        assert_eq!(
            workspace
                .window_settings
                .snippet_ai_font(FontView::Description),
            32
        );
        let _ = send(&mut workspace, ActionMessage::FontSize(FontView::Diff, 30));
        let _ = send(
            &mut workspace,
            ActionMessage::FontSize(FontView::Description, 19),
        );
        assert_eq!(
            workspace.window_settings.snippet_ai_font(FontView::Diff),
            50
        );
        assert_eq!(
            workspace
                .window_settings
                .snippet_ai_font(FontView::Description),
            19
        );

        let panel = workspace
            .snippets
            .get_mut(&id)
            .unwrap()
            .assist
            .0
            .as_mut()
            .unwrap();
        panel.recipe.action = Action::SecurityReview;
        let Phase::Result(result) = &mut panel.phase else {
            panic!("expected review result");
        };
        result.diff = None;
        result.outcome.alternatives.clear();
        let _ = send(
            &mut workspace,
            ActionMessage::FontSize(FontView::Review, 30),
        );
        let _ = send(
            &mut workspace,
            ActionMessage::FontSize(FontView::SecurityReport, 20),
        );
        assert_eq!(
            workspace.window_settings.snippet_ai_font(FontView::Review),
            14
        );
        assert_eq!(
            workspace
                .window_settings
                .snippet_ai_font(FontView::SecurityReport),
            20
        );
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            original
        );
    }

    #[test]
    fn unavailable_file_tool_opens_assistance_without_sending_a_request() {
        use crate::editor_window::tools::{Action as Tool, Message as ToolMessage};

        let scratch = Scratch::new();
        let path = scratch.path().with_file_name("fallback.zig");
        std::fs::write(&path, "const value=1;\n").unwrap();
        let document = editor_window::open_terminal_document(&path, true).unwrap();
        let (editor, _) = EditorWindow::with_document(document, None, None);
        let revision = editor.file_ai_revision();
        let mut workspace = Workspace::default();
        let id = window::Id::unique();
        workspace.editors.insert(id, editor);

        let open = |workspace: &mut Workspace, message| {
            workspace.update(Message::Editor(id, editor_window::Message::Tools(message)))
        };
        let _ = open(&mut workspace, ToolMessage::Open(Tool::Format));
        assert!(workspace.editors[&id].ai_assist.0.is_none());
        let mut task = iced_runtime::task::into_stream(open(&mut workspace, ToolMessage::UseAi(1)))
            .expect("confirmed fallback dispatches to file assistance");
        let Some(iced_runtime::Action::Output(event)) = block_on(task.next()) else {
            panic!("the fallback returned no editor event");
        };
        let _ = workspace.update(Message::Editor(
            id,
            editor_window::Message::AiToolFallback(Tool::Format, revision.wrapping_add(1)),
        ));
        assert!(workspace.editors[&id].ai_assist.0.is_none());
        let _ = workspace.update(event);
        let panel = workspace.editors[&id].ai_assist.0.as_ref().unwrap();
        assert_eq!(panel.recipe.action, Action::FormatCode);
        assert!(matches!(panel.phase, Phase::Configure));
    }

    #[test]
    fn file_actions_select_security_then_origin_and_preserve_explicit_choice() {
        use crate::ai_manager::ProfileRef;

        let scratch = Scratch::new();
        let path = scratch.path().with_file_name("review.sh");
        std::fs::write(&path, "echo fixture\n").unwrap();
        let document = editor_window::open_terminal_document(&path, true).unwrap();
        let (mut editor, _) = EditorWindow::with_document(document, None, None);
        editor.ai_origin.profile = Some(ProfileRef::Numeric(4));
        let profiles: Vec<_> = (1..=5).map(|id| json!({
            "id": id, "name": format!("Fixture {id}"), "endpoint": "127.0.0.1:1", "model": "fixture"
        })).collect();
        let config = serde_json::from_value(json!({
            "profiles": {"profiles": profiles, "default": 1, "last_id": 5},
            "local": {"roles": [2, 3, null, null], "security_profile": 5}
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let id = window::Id::unique();
        workspace.editors.insert(id, editor);
        let profile = |workspace: &Workspace| {
            workspace.editors[&id]
                .ai_assist
                .0
                .as_ref()
                .unwrap()
                .recipe
                .profile
        };
        let _ = workspace.update_file_assist(id, ActionMessage::Open);
        assert_eq!(profile(&workspace), Some(4));
        let _ = workspace.update_file_assist(id, ActionMessage::Choose(Action::SecurityReview));
        assert_eq!(profile(&workspace), Some(5));
        let _ = workspace.update_file_assist(id, ActionMessage::Choose(Action::Improve));
        assert_eq!(profile(&workspace), Some(4));
        let _ = workspace.update_file_assist(id, ActionMessage::Profile(2));
        let _ = workspace.update_file_assist(id, ActionMessage::Choose(Action::SecurityReview));
        assert_eq!(profile(&workspace), Some(2));
        let _ = workspace.update_file_assist(id, ActionMessage::Review);
        let Phase::Review(review) = &workspace.editors[&id].ai_assist.0.as_ref().unwrap().phase
        else {
            panic!("Expected a review");
        };
        assert_eq!(review.choice.id, 2);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "echo fixture\n");
    }

    #[test]
    fn snippet_one_liner_copies_locally_and_reviews_the_ai_fallback_before_copying() {
        use crate::local_ai::test_support::exclusive_run;
        use crate::snippets::one_liner::Mode;

        let _exclusive = exclusive_run();
        let scratch = Scratch::new();
        let mut library = crate::snippets::Library::open(scratch.path()).unwrap();
        let source = "cat <<'MARK'\n# literal\nMARK\n";
        let snippet = library
            .save(
                None,
                crate::snippets::Input {
                    name: "Here document fixture".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: source.into(),
                },
            )
            .unwrap();
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::Open(snippet));
        let (listener, _) = listener();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Local fixture", "endpoint": listener.local_addr().unwrap().to_string(),
                    "model": "fixture"
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        workspace.set_test_ai_configuration(config);
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let _ = workspace.update_snippets(id, snippet_window::Message::CopyCommand(Mode::Embedded));
        let embedded = workspace.clipboard.buffer.get().unwrap();
        assert!(embedded.ends_with("| base64 -d | bash"));

        let _ = workspace.update_snippets(id, snippet_window::Message::CopyCommand(Mode::Compact));
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        assert_eq!(panel.recipe.action, Action::CompactCommand);
        assert!(matches!(panel.phase, Phase::Configure));
        let send = |workspace: &mut Workspace, action| {
            workspace.update_snippets(id, snippet_window::Message::Assist(action))
        };
        let _ = send(&mut workspace, ActionMessage::Send);
        let _ = send(&mut workspace, ActionMessage::Review);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some(embedded.as_str())
        );

        let command = "printf '%s\\n' '# literal'";
        let server = std::thread::spawn(move || {
            let mut socket = accept(&listener);
            let body = request(&mut socket);
            assert!(
                body["messages"][1]["content"]
                    .as_str()
                    .unwrap()
                    .contains("cat <<")
            );
            let response = fixture_response("200 OK", "", &json!({"text":command}).to_string());
            socket.get_mut().write_all(response.as_bytes()).unwrap();
        });
        let mut task =
            iced_runtime::task::into_stream(send(&mut workspace, ActionMessage::Send)).unwrap();
        while let Some(action) = block_on(task.next()) {
            if let iced_runtime::Action::Output(event) = action {
                let _ = workspace.update(event);
            }
        }
        server.join().unwrap();
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some(embedded.as_str())
        );
        assert!(matches!(
            workspace.snippets[&id].assist.0.as_ref().unwrap().phase,
            Phase::Result(_)
        ));
        workspace.clipboard.policy.write_allowed = false;
        let _ = send(&mut workspace, ActionMessage::CopyCommand);
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some(embedded.as_str())
        );
        workspace.clipboard.policy.write_allowed = true;
        let _ = send(&mut workspace, ActionMessage::CopyCommand);
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some(command));
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            source
        );
    }

    #[test]
    fn https_snippet_assist_reviews_exact_prompts_and_rejects_changed_authority() {
        use kortty_vault::{CredentialInput, Secret, Vault};

        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let (listener, _) = listener();
        let endpoint = format!("https://{}/v1", listener.local_addr().unwrap());
        let configuration = |model: &str| -> Configuration {
            serde_json::from_value(json!({
                "profiles": {
                    "profiles": [{
                        "id": 1, "name": "Fixture HTTPS", "endpoint": endpoint, "model": model,
                        "connection_mode": "anthropic", "trusted_remote_endpoint": true
                    }],
                    "default": 1, "last_id": 1
                }
            }))
            .unwrap()
        };
        configuration("reviewed-model")
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();

        let secret = r#"sensitive "quoted" vault value"#;
        let description = format!("This description contains {secret}.");
        let mut library = crate::snippets::Library::open(scratch.path()).unwrap();
        let snippet = library
            .save_with_metadata(
                None,
                crate::snippets::Input {
                    name: "Fixture".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: format!("printf visible\n# {secret}").into(),
                },
                Some(crate::snippets::Metadata {
                    description,
                    ..Default::default()
                }),
            )
            .unwrap();
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::Open(snippet));
        let path = root.join("test-vault.bin");
        let mut vault =
            Vault::create(&path, &crate::vault_window::test_vault_password()[..]).unwrap();
        vault
            .save(
                CredentialInput {
                    label: "Synthetic vault credential".into(),
                    ..CredentialInput::default()
                },
                Secret::new(secret.into()),
            )
            .unwrap();
        let mut workspace = Workspace {
            vault: Some(VaultWindow::with_unlocked_fixture(path, vault)),
            ..Workspace::default()
        };
        workspace.set_test_ai_configuration(saved.clone());
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| {
            workspace.update_snippets(id, snippet_window::Message::Assist(action))
        };
        let review = |workspace: &mut Workspace| {
            let _ = send(workspace, ActionMessage::Open);
            let _ = send(workspace, ActionMessage::Choose(Action::CorrectDescription));
            let _ = send(workspace, ActionMessage::Review);
        };
        let expect_no_connection = || {
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        };

        review(&mut workspace);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        let Phase::Review(reviewed) = &panel.phase else {
            panic!("expected HTTPS review: {:?}", panel.status);
        };
        assert_eq!(reviewed.choice.destination, format!("{endpoint}/messages"));
        assert_eq!(reviewed.choice.model.as_deref(), Some("reviewed-model"));
        assert!(matches!(
            reviewed.outbound.as_ref(),
            Some(Outbound::Https(_))
        ));
        let suffix = reviewed.transport.structured_system_suffix().unwrap();
        assert!(reviewed.request.system.ends_with(suffix));
        assert_eq!(reviewed.request.system.matches(suffix).count(), 1);
        let payload: serde_json::Value = serde_json::from_str(&reviewed.request.input).unwrap();
        assert_eq!(payload["description"], "This description contains ***.");
        assert_eq!(payload["source"], "printf visible\n# ***");
        expect_no_connection();

        workspace.clipboard.policy.ai_allowed = false;
        workspace.revalidate_remote_snippet_assistance();
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        expect_no_connection();

        workspace.clipboard.policy.ai_allowed = true;
        review(&mut workspace);
        let _ = workspace
            .vault
            .as_mut()
            .unwrap()
            .update(crate::vault_window::Message::Lock);
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        expect_no_connection();

        review(&mut workspace);
        configuration("changed-on-disk")
            .save_fixture_at(&root, &saved)
            .unwrap();
        let mut task =
            iced_runtime::task::into_stream(send(&mut workspace, ActionMessage::Send)).unwrap();
        let Some(iced_runtime::Action::Output(Message::Snippets(id, event))) =
            block_on(task.next())
        else {
            panic!("expected completion from the refused worker");
        };
        let _ = workspace.update_snippets(id, event);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        assert!(matches!(panel.phase, Phase::Configure));
        assert!(
            panel
                .status
                .as_deref()
                .unwrap()
                .contains("saved AI settings")
        );
        expect_no_connection();

        let _ = workspace.update_snippets(id, snippet_window::Message::Name("Changed".into()));
        review(&mut workspace);
        assert!(matches!(
            workspace.snippets[&id].assist.0.as_ref().unwrap().phase,
            Phase::Review(_)
        ));
        let _ = workspace.request_snippet_close(id);
        assert!(workspace.snippets[&id].assist.0.is_none());
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        expect_no_connection();
    }

    #[test]
    fn https_snippet_assist_caps_rendered_prompts_after_masking() {
        let scratch = Scratch::new();
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new("\"".repeat(140_000))),
        )));
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Fixture HTTPS", "endpoint": "https://127.0.0.1:1/v1",
                    "model": "fixture", "connection_mode": "remote-open-ai",
                    "trusted_remote_endpoint": true
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        let (transport, limit) = config.snippet_analysis_transport(1).unwrap();
        let panel = Panel {
            source: editor.assist_snapshot().unwrap(),
            language_mix: None,
            staged_migration: None,
            recipe: Recipe {
                action: Action::Describe,
                ..Default::default()
            },
            profiles: Vec::new(),
            explicit_profile: false,
            skills: Vec::new(),
            phase: Phase::Configure,
            status: None,
        };
        let policy = ClipboardPolicy::default();
        let context = Context {
            policy: &policy,
            vault: None,
            vault_revision: Weak::new(),
            file: None,
            snippet_translation_language: None,
            snippet_alternative_count: None,
            additional_instructions: false,
        };
        assert!(matches!(
            panel.masked_request(&config, &transport, limit, &context),
            Err(error) if error.contains("512 KiB")
        ));
    }

    #[test]
    fn cli_snippet_assistance_runs_only_current_exact_review_and_applies_separately() {
        use kortty_vault::{CredentialInput, Secret, Vault};

        const ENV: &str = "KORTTY_SNIPPET_EDITOR_CLI_FIXTURE";
        let Some(home) = std::env::var_os(ENV).map(std::path::PathBuf::from) else {
            let scratch = Scratch::new();
            let home = scratch.0.canonicalize().unwrap();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "workspace::snippet_access::ai_assist::tests::cli_snippet_assistance_runs_only_current_exact_review_and_applies_separately", "--nocapture"])
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
        let root = home.join("profile");
        std::fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        crate::profile::set_runtime_root(root.clone());
        let (executable, fingerprint) = crate::local_ai::cli::Program::inspect(
            &crate::local_ai::cli::fixture_executable(&root),
        )
        .unwrap();
        let config = |answer: &str| -> Configuration {
            serde_json::from_value(json!({
                "profiles":{"profiles":[{
                    "id":1, "name":"Snippet fixture CLI", "connection_mode":"cli",
                    "endpoint":"", "vision":"disabled", "prompt_preset":"qwen",
                    "cli":{"path":executable,"fingerprint":fingerprint,
                        "arguments":["fixture_scheduled_proposal", answer, "{promptFile}"]}
                }],"default":1,"last_id":1}
            }))
            .unwrap()
        };
        config(r#"{"text":"fixture-added"}"#)
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();
        let path = root.join("vault.bin");
        let mut vault =
            Vault::create(&path, &crate::vault_window::test_vault_password()[..]).unwrap();
        vault
            .save(
                CredentialInput {
                    label: "Editor CLI fixture value".into(),
                    ..CredentialInput::default()
                },
                Secret::new("fixture-vault-value".into()),
            )
            .unwrap();
        let mut workspace = Workspace {
            vault: Some(VaultWindow::with_unlocked_fixture(path, vault)),
            ..Workspace::default()
        };
        workspace.set_test_ai_configuration(saved.clone());
        let mut editor = SnippetWindow::open(root.join("snippets.json")).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new("printf ".into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| {
            workspace.update_snippets(id, snippet_window::Message::Assist(action))
        };
        let _ = send(&mut workspace, ActionMessage::Open);
        let _ = send(
            &mut workspace,
            ActionMessage::Instructions("Preserve fixture comment".into()),
        );
        assert!(
            workspace.snippets[&id]
                .assist
                .0
                .as_ref()
                .unwrap()
                .recipe
                .instructions
                .is_empty()
        );
        workspace.window_settings.snippet_ai_additional_instructions = Some(true);
        let review = |workspace: &mut Workspace| {
            let _ = send(workspace, ActionMessage::Open);
            let _ = send(
                workspace,
                ActionMessage::Instructions("fixture-vault-value password=fixture-generic".into()),
            );
            let _ = send(workspace, ActionMessage::Review);
            let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
            let Phase::Review(review) = &panel.phase else {
                panic!("Expected a CLI review: {:?}", panel.status);
            };
            let cli = review.cli.as_ref().unwrap();
            assert_eq!(review.choice.route, Route::Cli);
            assert_eq!(cli.request.primary().0, "Exact UTF-8 prompt.txt");
            assert!(
                cli.request
                    .command_review()
                    .contains("fixture_scheduled_proposal")
            );
            assert!(cli.request.primary().1.contains("*** password=***"));
            assert!(!cli.request.primary().1.contains("fixture-vault-value"));
            assert!(!cli.request.primary().1.contains("fixture-generic"));
            assert!(
                cli.request
                    .primary()
                    .1
                    .contains("Do not execute commands or use tools")
            );
            (cli.ticket, cli.request.primary().1.to_owned())
        };
        let (disabled, _) = review(&mut workspace);
        workspace.window_settings.snippet_ai_additional_instructions = Some(false);
        assert_eq!(
            send(&mut workspace, ActionMessage::RunCli(disabled)).units(),
            0
        );
        assert!(!root.join("fixture-scheduled-prompt").exists());
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        assert!(matches!(panel.phase, Phase::Configure));
        assert!(
            panel
                .status
                .as_deref()
                .unwrap()
                .contains("masked request changed")
        );
        let _ = send(&mut workspace, ActionMessage::Review);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        let Phase::Review(without_instructions) = &panel.phase else {
            panic!("Expected a fresh CLI review: {:?}", panel.status);
        };
        assert!(!without_instructions.request.input.contains("password="));
        workspace.window_settings.snippet_ai_additional_instructions = Some(true);
        let (stale, _) = review(&mut workspace);
        let _ = workspace.update_snippets(id, snippet_window::Message::Name("Edited".into()));
        assert_eq!(
            send(&mut workspace, ActionMessage::RunCli(stale)).units(),
            0
        );
        let (current, exact) = review(&mut workspace);
        let marker = root.join("fixture-scheduled-prompt");
        assert_eq!(
            send(&mut workspace, ActionMessage::RunCli(stale)).units(),
            0
        );
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        assert!(!marker.exists());
        let task = send(&mut workspace, ActionMessage::RunCli(current));
        let mut result = iced_runtime::task::into_stream(task).expect("reviewed CLI starts");
        let Some(iced_runtime::Action::Output(Message::Snippets(id, event))) =
            block_on(result.next())
        else {
            panic!("expected the CLI result");
        };
        let _ = workspace.update_snippets(id, event);
        assert_eq!(std::fs::read(marker).unwrap(), exact.as_bytes());
        let source = |workspace: &Workspace| {
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
        };
        assert_eq!(source(&workspace).as_ref(), "printf ");
        let _ = send(&mut workspace, ActionMessage::Apply);
        assert_eq!(source(&workspace).as_ref(), "printf fixture-added");

        config(r#"{"text":"unreviewed"}"#)
            .save_fixture_at(&root, &saved)
            .unwrap();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let _ = send(&mut workspace, ActionMessage::Rerun);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        assert!(
            panel
                .status
                .as_deref()
                .is_some_and(|message| message.contains("original AI profile changed"))
        );
    }

    #[test]
    fn reviewed_ai_completions_join_the_local_list_and_apply_only_a_visible_candidate() {
        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let scratch = Scratch::new();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Fixture", "endpoint": endpoint, "model": "fixture"
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let original = "items = [1, 2]\r\nfor item in ra";
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Name(
            "Library-only fixture title".into(),
        ));
        let _ = editor.update(snippet_window::Message::Language("python".into()));
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new(original.into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| workspace.update_snippet_assist(id, action);
        let _ = workspace.update_snippets(id, snippet_window::Message::Complete);
        let _ = send(&mut workspace, ActionMessage::OpenCompletionList);
        let _ = send(&mut workspace, ActionMessage::Review);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        let Phase::Review(review) = &panel.phase else {
            panic!("expected the completion review: {:?}", panel.status);
        };
        let approved = review.request.input.clone();
        let payload: serde_json::Value = serde_json::from_str(&approved).unwrap();
        assert_eq!(payload["selected_text"], "ra");
        assert_eq!(payload["language"], "python");
        assert!(
            payload["known_symbols"]
                .as_array()
                .unwrap()
                .contains(&json!("items"))
        );
        assert!(!approved.contains("Library-only fixture title"));
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let body = request(&mut stream);
            assert_eq!(
                body["messages"][1]["content"].as_str(),
                Some(approved.as_ref())
            );
            let response = json!({"completions": [
                "range(10)", "range(20)", "range(20)", "unmatched()"
            ]})
            .to_string();
            stream
                .get_mut()
                .write_all(fixture_response("200 OK", "", &response).as_bytes())
                .unwrap();
        });
        let task = send(&mut workspace, ActionMessage::Send);
        let surface = &workspace.snippets[&id];
        assert!(surface.assist.completion_running());
        assert!(surface.assist.view(&surface.window, None).is_none());
        let mut stream = iced_runtime::task::into_stream(task).unwrap();
        let (owner, event) = loop {
            match block_on(stream.next()) {
                Some(iced_runtime::Action::Output(Message::Snippets(owner, event))) => {
                    break (owner, event);
                }
                Some(_) => {}
                None => panic!("expected the completion result"),
            }
        };
        assert_eq!(owner, id);
        let _ = workspace.update_snippets(id, event);
        server.join().unwrap();
        let surface = &workspace.snippets[&id];
        let Phase::Result(result) = &surface.assist.0.as_ref().unwrap().phase else {
            panic!("expected the suggestions to be available");
        };
        let ticket = result.ticket;
        assert_eq!(result.outcome.alternatives.len(), 3);
        assert!(!surface.window.can_apply_completion(ticket, 0));
        assert!(surface.window.can_apply_completion(ticket, 1));
        assert!(!surface.window.can_apply_completion(ticket, 2));
        assert_eq!(
            surface.window.assist_snapshot().unwrap().source.as_ref(),
            original
        );
        let _ = send(&mut workspace, ActionMessage::ApplyCompletion(ticket, 2));
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            original
        );
        let _ = send(&mut workspace, ActionMessage::ApplyCompletion(ticket, 1));
        let changed = workspace.snippets[&id].window.assist_snapshot().unwrap();
        assert_eq!(
            changed.source.as_ref(),
            "items = [1, 2]\r\nfor item in range(20)"
        );
        assert_eq!(
            changed.selection,
            changed.source.len()..changed.source.len()
        );
        let window = &mut workspace.snippets.get_mut(&id).unwrap().window;
        let change = window.last_ai_change().unwrap();
        window.undo_ai_change(&change).unwrap();
        assert_eq!(window.assist_snapshot().unwrap().source.as_ref(), original);
    }

    #[test]
    fn automatic_completion_uses_the_reviewed_profile_until_disabled() {
        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let scratch = Scratch::new();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Fixture", "endpoint": endpoint, "model": "fixture"
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Language("python".into()));
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new("items = [1, 2]\nfor item in ra".into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| workspace.update_snippet_assist(id, action);
        let open = |workspace: &mut Workspace| {
            workspace.update_snippets(id, snippet_window::Message::Complete)
        };
        let drain = |workspace: &mut Workspace, task| {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            loop {
                match block_on(stream.next()) {
                    Some(iced_runtime::Action::Output(Message::Snippets(owner, event))) => {
                        assert_eq!(owner, id);
                        let _ = workspace.update_snippets(id, event);
                        break;
                    }
                    Some(_) => {}
                    None => panic!("expected the completion result"),
                }
            }
            let surface = &workspace.snippets[&id];
            let Phase::Result(result) = &surface.assist.0.as_ref().unwrap().phase else {
                panic!(
                    "expected suggestions: {:?}",
                    surface.assist.0.as_ref().unwrap().status
                );
            };
            assert!(surface.window.can_apply_completion(result.ticket, 0));
        };
        let _ = open(&mut workspace);
        let _ = send(
            &mut workspace,
            ActionMessage::EnableAutomaticCompletion(None),
        );
        assert!(workspace.snippets[&id].automatic_completion.is_none());
        let _ = send(&mut workspace, ActionMessage::OpenCompletionList);
        let _ = send(&mut workspace, ActionMessage::Review);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let server = std::thread::spawn(move || {
            for prefix in ["ra", "ran"] {
                let mut stream = accept(&listener);
                let body = request(&mut stream);
                assert_eq!(body["model"], "fixture");
                let payload: serde_json::Value =
                    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                assert_eq!(payload["selected_text"], prefix);
                assert!(
                    payload["known_symbols"]
                        .as_array()
                        .unwrap()
                        .contains(&json!("items"))
                );
                let response = json!({"completions": ["range(20)"]}).to_string();
                stream
                    .get_mut()
                    .write_all(fixture_response("200 OK", "", &response).as_bytes())
                    .unwrap();
            }
            listener
        });
        let task = send(
            &mut workspace,
            ActionMessage::EnableAutomaticCompletion(None),
        );
        assert!(workspace.snippets[&id].automatic_completion.is_some());
        drain(&mut workspace, task);
        let _ = workspace.update_snippets(
            id,
            snippet_window::Message::Edit(EditorAction::Edit(Edit::Insert('n'))),
        );
        let task = open(&mut workspace);
        drain(&mut workspace, task);
        let listener = server.join().unwrap();
        let _ = send(&mut workspace, ActionMessage::DisableAutomaticCompletion);
        assert!(workspace.snippets[&id].automatic_completion.is_none());
        let _ = open(&mut workspace);
        assert!(!workspace.snippets[&id].assist.completion_running());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn inline_completion_waits_for_a_typing_pause_and_rechecks_the_active_candidate() {
        let _exclusive = crate::local_ai::test_support::exclusive_run();
        let scratch = Scratch::new();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{"id": 1, "name": "Fixture", "endpoint": endpoint, "model": "fixture"}],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Language("python".into()));
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new("items = [1, 2]\nfor item in ra".into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| workspace.update_snippet_assist(id, action);
        let edit = |workspace: &mut Workspace, action| {
            workspace.update_snippets(id, snippet_window::Message::Edit(action))
        };
        let drain = |workspace: &mut Workspace, task| {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            while let Some(event) = block_on(stream.next()) {
                if let iced_runtime::Action::Output(Message::Snippets(owner, event)) = event {
                    assert_eq!(owner, id);
                    let _ = workspace.update_snippets(id, event);
                    let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
                    let Phase::Result(result) = &panel.phase else {
                        panic!("expected inline suggestions: {:?}", panel.status);
                    };
                    return result.ticket;
                }
            }
            panic!("expected an inline completion result");
        };
        let _ = send(&mut workspace, ActionMessage::OpenInlineCompletion);
        let _ = send(&mut workspace, ActionMessage::Review);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        let server = std::thread::spawn(move || {
            for prefix in ["ra", "ran"] {
                let mut stream = accept(&listener);
                let body = request(&mut stream);
                let input: serde_json::Value =
                    serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                assert_eq!(input["selected_text"], prefix);
                let response =
                    json!({"completions": ["other()", "range(10)", "range(20):\n    pass"]})
                        .to_string();
                stream
                    .get_mut()
                    .write_all(fixture_response("200 OK", "", &response).as_bytes())
                    .unwrap();
            }
        });
        let initial = send(
            &mut workspace,
            ActionMessage::EnableAutomaticCompletion(None),
        );
        let first = drain(&mut workspace, initial);
        assert!(workspace.snippets[&id].automatic_completion.is_none());
        assert!(workspace.snippets[&id].inline_completion.is_some());
        assert!(
            !workspace.snippets[&id]
                .window
                .can_apply_completion(first, 0)
        );
        assert!(
            workspace.snippets[&id]
                .window
                .can_apply_completion(first, 1)
        );
        let _ = edit(&mut workspace, EditorAction::Edit(Edit::Insert('n')));
        assert!(
            !workspace.snippets[&id]
                .window
                .can_apply_completion(first, 1)
        );
        let due = workspace.snippets[&id].inline_completion_due.unwrap();
        assert!(
            iced_runtime::task::into_stream(
                workspace.poll_inline_completion(id, due - Duration::from_millis(1))
            )
            .is_none()
        );
        let repeat = workspace.poll_inline_completion(id, due);
        let second = drain(&mut workspace, repeat);
        server.join().unwrap();
        let _ = workspace.update_snippets(id, snippet_window::Message::StepInlineCompletion(true));
        assert!(
            !workspace.snippets[&id]
                .window
                .can_apply_completion(second, 1)
        );
        assert!(
            workspace.snippets[&id]
                .window
                .can_apply_completion(second, 2)
        );
        let _ = send(&mut workspace, ActionMessage::ApplyCompletion(second, 2));
        let surface = workspace.snippets.get_mut(&id).unwrap();
        assert_eq!(
            surface.window.assist_snapshot().unwrap().source.as_ref(),
            "items = [1, 2]\nfor item in range(20):\n    pass"
        );
        let change = surface.window.last_ai_change().unwrap();
        surface.window.undo_ai_change(&change).unwrap();
        assert_eq!(
            surface.window.assist_snapshot().unwrap().source.as_ref(),
            "items = [1, 2]\nfor item in ran"
        );
        let _ = edit(&mut workspace, EditorAction::Edit(Edit::Insert('g')));
        let before_blur = workspace.snippets[&id].inline_completion_due.unwrap();
        let _ = workspace.update_snippets(id, snippet_window::Message::EditorUnfocused);
        assert!(
            iced_runtime::task::into_stream(workspace.poll_inline_completion(id, before_blur))
                .is_none()
        );
    }

    #[test]
    fn migrated_security_fix_waits_for_a_second_review_before_sending() {
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let config: Configuration = serde_json::from_value(json!({
            "profiles": {
                "profiles": [{
                    "id": 1, "name": "Fixture", "endpoint": endpoint, "model": "fixture",
                    "usage_budget": {"token_limit": 20_000}
                }],
                "default": 1, "last_id": 1
            }
        }))
        .unwrap();
        config
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let original = "#!/usr/bin/env bash\npython3 -c 'print(\"hello\")'";
        let migrated = "#!/usr/bin/env python3\nprint(\"hello\")";
        let fixed = "#!/usr/bin/env python3\nimport sys\nprint(\"hello\", file=sys.stderr)";
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Language("bash".into()));
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new(original.into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| workspace.update_snippet_assist(id, action);
        let deliver = |workspace: &mut Workspace, task| {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(Message::Snippets(owner, event))) =
                block_on(stream.next())
            else {
                panic!("expected an assistant event");
            };
            assert_eq!(owner, id);
            let _ = workspace.update_snippets(id, event);
        };
        let _ = send(&mut workspace, ActionMessage::Open);
        let panel = workspace
            .snippets
            .get_mut(&id)
            .unwrap()
            .assist
            .0
            .as_mut()
            .unwrap();
        let finding = security::Finding {
            severity: security::Severity::Low,
            title: "Separate diagnostics".into(),
            impact: "Line 2 adds a diagnostic to machine-readable output.".into(),
            recommendation: "Write the diagnostic to standard error.".into(),
        };
        panel.recipe.findings =
            Some(security::Selection::new(panel.source.clone(), [&finding].into_iter()).unwrap());
        let _ = send(&mut workspace, ActionMessage::Choose(Action::SecurityFix));
        let _ = send(
            &mut workspace,
            ActionMessage::MigrationLanguage(migration::Language::Python),
        );
        let forecast = send(&mut workspace, ActionMessage::Review);
        deliver(&mut workspace, forecast);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let serve = |listener, response: serde_json::Value| {
            std::thread::spawn(move || {
                let mut stream = accept(&listener);
                let body = request(&mut stream);
                let response = fixture_response("200 OK", "", &response.to_string());
                stream.get_mut().write_all(response.as_bytes()).unwrap();
                let payload = body["messages"][1]["content"].as_str().unwrap();
                serde_json::from_str::<serde_json::Value>(payload).unwrap()
            })
        };
        let first = serve(
            listener.try_clone().unwrap(),
            json!({"text": migrated, "notes": []}),
        );
        let running = send(&mut workspace, ActionMessage::Send);
        deliver(&mut workspace, running);
        let payload = first.join().unwrap();
        assert_eq!(payload["source"], original);
        assert!(payload["security_findings"].is_null());
        let Phase::Result(result) = &workspace.snippets[&id].assist.0.as_ref().unwrap().phase
        else {
            panic!("expected the migration proposal");
        };
        let ticket = result.ticket;
        let _ = send(&mut workspace, ActionMessage::Apply);
        let source = |workspace: &Workspace| {
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
        };
        assert_eq!(source(&workspace).as_ref(), original);
        let forecast = send(
            &mut workspace,
            ActionMessage::ReviewMigratedSecurityFix(ticket),
        );
        deliver(&mut workspace, forecast);
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

        let second = serve(listener, json!({"text": fixed}));
        let running = send(&mut workspace, ActionMessage::Send);
        deliver(&mut workspace, running);
        let payload = second.join().unwrap();
        assert_eq!(payload["source"], migrated);
        assert_eq!(payload["language"], "python");
        assert_eq!(payload["security_findings"][0]["title"], finding.title);
        assert_eq!(source(&workspace).as_ref(), original);
        let _ = send(&mut workspace, ActionMessage::Apply);
        assert_eq!(source(&workspace).as_ref(), fixed);
    }

    #[test]
    fn snippet_assist_sends_reviewed_masked_payload_and_rechecks_rerun_profile() {
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let configuration = |model: Option<&str>| -> Configuration {
            serde_json::from_value(json!({
                "profiles": {
                    "profiles": [{
                        "id": 1, "name": "Fixture", "endpoint": endpoint,
                        "model": model.unwrap_or_default(),
                        "model_mode": if model.is_some() { "manual" } else { "server-default" },
                        "usage_budget": {"token_limit": 2000}, "max_output_tokens": 321
                    }, {
                        "id": 2, "name": "Named fallback", "endpoint": endpoint, "model": "fallback"
                    }],
                    "default":2,"last_id":2,
                },
                "local": {"roles": [null, 1, null, null]}
            }))
            .unwrap()
        };
        configuration(None)
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        let original = "token=fixture-private-value\nprintf ";
        let mut editor = SnippetWindow::open(scratch.path()).unwrap();
        let _ = editor.update(snippet_window::Message::New);
        let _ = editor.update(snippet_window::Message::Edit(EditorAction::Edit(
            Edit::Paste(Arc::new(original.into())),
        )));
        let id = window::Id::unique();
        workspace
            .snippets
            .insert(id, super::super::Surface::new(editor, id));
        let send = |workspace: &mut Workspace, action| {
            workspace.update_snippets(id, snippet_window::Message::Assist(action))
        };
        let quota_event = |task: Task<Message>| {
            let mut stream = iced_runtime::task::into_stream(task).unwrap();
            let Some(iced_runtime::Action::Output(Message::Snippets(owner, event))) =
                block_on(stream.next())
            else {
                panic!("expected the quota forecast");
            };
            assert_eq!(owner, id);
            assert!(matches!(
                event,
                snippet_window::Message::Assist(ActionMessage::QuotaFinished(..))
            ));
            event
        };
        let expect_no_connection = || {
            assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        };
        let _ = send(&mut workspace, ActionMessage::Open);
        let first_forecast = quota_event(send(&mut workspace, ActionMessage::Review));
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        let Phase::Review(review) = &panel.phase else {
            panic!("expected reviewed request: {:?}", panel.status);
        };
        assert!(review.quota.loading());
        assert_eq!(review.choice.id, 1);
        assert!(
            review
                .choice
                .to_string()
                .contains("Server selects the model")
        );
        let approved_payload = review.request.input.clone();
        let approved_system = review.request.system.clone();
        assert!(!approved_payload.contains("fixture-private-value"));
        assert!(approved_payload.contains("token=***"));
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        expect_no_connection();
        let _ = workspace.update_snippets(id, first_forecast.clone());
        let panel = workspace
            .snippets
            .get_mut(&id)
            .unwrap()
            .assist
            .0
            .as_mut()
            .unwrap();
        let Phase::Review(review) = &mut panel.phase else {
            panic!("expected a quota forecast in the reviewed request");
        };
        let forecast = review.quota.label().unwrap();
        let input = ai_quota::estimate_input([approved_system.as_ref(), approved_payload.as_ref()]);
        assert!(forecast.contains(&format!("about {input} input tokens")));
        assert!(forecast.contains("321 output tokens"));
        review.quota.expire();
        let refresh = send(&mut workspace, ActionMessage::Send);
        let _ = workspace.update_snippets(id, first_forecast);
        assert_eq!(send(&mut workspace, ActionMessage::Send).units(), 0);
        let _ = workspace.update_snippets(id, quota_event(refresh));
        expect_no_connection();
        assert!(!root.join("ai-usage").exists());

        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener);
            let body = request(&mut stream);
            let content = json!({"text":"\"done\""}).to_string();
            let response = fixture_response("200 OK", "", &content);
            stream.get_mut().write_all(response.as_bytes()).unwrap();
            body
        });
        let mut task =
            iced_runtime::task::into_stream(send(&mut workspace, ActionMessage::Send)).unwrap();
        let Some(iced_runtime::Action::Output(Message::Snippets(id, message))) =
            block_on(task.next())
        else {
            panic!("expected snippet assistance result");
        };
        let _ = workspace.update_snippets(id, message);
        let wire = server.join().unwrap();
        assert!(wire.get("model").is_none());
        assert_eq!(wire["max_tokens"], 321);
        assert_eq!(wire["messages"][0]["content"], approved_system.as_ref());
        assert_eq!(
            wire["messages"][1]["content"].as_str(),
            Some(approved_payload.as_ref())
        );
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            original
        );
        let _ = send(&mut workspace, ActionMessage::Apply);
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            format!("{original}\"done\"")
        );

        let rerun_forecast = quota_event(send(&mut workspace, ActionMessage::Rerun));
        let _ = workspace.update_snippets(id, rerun_forecast);
        let Phase::Review(review) = &workspace.snippets[&id].assist.0.as_ref().unwrap().phase
        else {
            panic!("expected an original-profile rerun review");
        };
        assert!(
            review
                .quota
                .label()
                .unwrap()
                .contains("Recorded usage is incomplete")
        );
        let _ = send(&mut workspace, ActionMessage::ShowLast);
        workspace.set_test_ai_configuration(configuration(Some("fixture-changed")));
        let _ = send(&mut workspace, ActionMessage::Rerun);
        let panel = workspace.snippets[&id].assist.0.as_ref().unwrap();
        assert!(
            panel
                .status
                .as_deref()
                .is_some_and(|status| status.contains("original AI profile changed"))
        );

        let _ = send(&mut workspace, ActionMessage::ShowLast);
        let _ = send(&mut workspace, ActionMessage::Undo);
        assert_eq!(
            workspace.snippets[&id]
                .window
                .assist_snapshot()
                .unwrap()
                .source
                .as_ref(),
            original
        );
    }
}
