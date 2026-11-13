use std::fmt;
use std::sync::{Arc, Weak};

use iced::widget::{button, checkbox, column, pick_list, row, scrollable, text, text_input};
use iced::{Element, Length, Task, window};

use crate::ai_manager::{
    Configuration, ProfileId, SkillGuidance, SkillUsageReceipt, SkillUsageSurface,
};
use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::{Cancel, Pending, Profile};
use crate::snippet_analysis::request::MAX_REMOTE_REQUEST_BYTES;
use crate::snippet_window::ai_assist::{
    Action, Change, Message as ActionMessage, Outcome, Recipe, Request, Snapshot,
};
use crate::snippet_window::{self, SnippetWindow, compare};
use crate::terminal_logging::{Policy, Redactor, Secrets};
use crate::vault_window::VaultWindow;
use crate::workspace::ai_quota;

use super::{Message, Workspace};

mod cli;

#[derive(Default)]
pub(super) struct State(Option<Panel>);

struct Panel {
    source: Snapshot,
    recipe: Recipe,
    profiles: Vec<Choice>,
    skills: Vec<(u64, String)>,
    phase: Phase,
    status: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Choice {
    id: ProfileId,
    name: String,
    model: String,
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
        write!(f, "{} · {}", self.name, self.model)?;
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
            Route::Local | Route::Https => config.snippet_analysis_transport(self.id),
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
    config: Arc<Configuration>,
    outbound: Option<Outbound>,
    outcome: Outcome,
    diff: Option<Box<compare::View>>,
}

struct Context<'a> {
    policy: &'a ClipboardPolicy,
    vault: Option<&'a VaultWindow>,
    vault_revision: Weak<()>,
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
    pub(super) fn revalidate_source(&mut self, editor: &SnippetWindow) {
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
        editor: &'a SnippetWindow,
    ) -> Option<Element<'a, ActionMessage>> {
        let panel = self.0.as_ref()?;
        let close = || button("Close").on_press(ActionMessage::Close);
        let title = match &panel.phase {
            Phase::Last(..) => "Last AI change",
            _ => "Snippet AI assistance",
        };
        let mut body = column![
            text(title).size(20),
            text(panel.status.as_deref().unwrap_or_default())
        ]
        .spacing(8);
        let page: Element<'_, ActionMessage> = match &panel.phase {
            Phase::Configure => {
                let selected = panel
                    .profiles
                    .iter()
                    .find(|choice| Some(choice.id) == panel.recipe.profile)
                    .cloned();
                let mut actions = Action::ALL.to_vec();
                if panel.recipe.findings.is_some() {
                    actions.push(Action::SecurityFix);
                }
                let mut options = column![
                    pick_list(actions, Some(panel.recipe.action), ActionMessage::Choose),
                    pick_list(panel.profiles.as_slice(), selected, |choice: Choice| {
                        ActionMessage::Profile(choice.id)
                    })
                    .placeholder("Select an AI profile"),
                    text_input("Additional instructions", &panel.recipe.instructions)
                        .on_input(ActionMessage::Instructions),
                ]
                .spacing(8);
                if panel.recipe.action == Action::Translate {
                    options = options.push(
                        text_input("Target language", &panel.recipe.target_language)
                            .on_input(ActionMessage::TargetLanguage),
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
                        button("Review request…")
                            .on_press_maybe(panel.recipe.profile.map(|_| ActionMessage::Review)),
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
                text(format!(
                    "Send to {} · {}",
                    review.choice, review.choice.destination
                )),
                matches!(review.outbound.as_ref(), Some(Outbound::Https(_))).then(|| text(
                    "This HTTPS provider receives the masked prompts and the selected model."
                )),
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
                    column![
                        diff.review_body().map(ActionMessage::Diff),
                        row![
                            button("Apply reviewed change").on_press(ActionMessage::Apply),
                            button("Discard proposal").on_press(ActionMessage::Back),
                            close()
                        ]
                        .spacing(8)
                    ]
                    .spacing(8)
                    .into()
                } else {
                    column![
                        scrollable(text(result.outcome.text.as_ref())).height(Length::Fill),
                        row![
                            (panel.recipe.action == Action::CompactCommand).then(|| {
                                button("Copy reviewed command").on_press(ActionMessage::CopyCommand)
                            }),
                            button("Propose security fixes…").on_press_maybe(
                                (panel.recipe.action == Action::SecurityReview)
                                    .then_some(ActionMessage::Fix)
                            ),
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
                let exact = editor
                    .assist_snapshot()
                    .is_ok_and(|source| change.undoable(&source));
                column![
                    diff.review_body().map(ActionMessage::Diff),
                    row![
                        button("Undo last AI change")
                            .on_press_maybe(exact.then_some(ActionMessage::Undo)),
                        button("Rerun with original profile…")
                            .on_press_maybe(exact.then_some(ActionMessage::Rerun)),
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
    pub(in crate::workspace) fn cancel_snippet_assistance(&mut self) {
        for surface in self.snippets.values_mut() {
            surface.assist = State::default();
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
        };
        for surface in self.snippets.values_mut() {
            if surface
                .assist
                .0
                .as_ref()
                .is_some_and(|panel| matches!(panel.phase.outbound(), Some((_, Outbound::Cli(_)))))
            {
                surface.assist.revalidate_source(&surface.window);
            }
            if let Some(panel) = surface.assist.0.as_mut()
                && let Some((approved, outbound)) = panel.phase.outbound()
                && (config
                    .as_ref()
                    .is_none_or(|current| !Arc::ptr_eq(current, approved))
                    || !outbound.matches(&context))
            {
                panel.phase = Phase::Configure;
                panel.status = Some(
                    "AI settings, vault, or masking policy changed. Review the request again."
                        .into(),
                );
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
        let config = self.ai_configuration();
        let ticket = self.allocate();
        let context = Context {
            policy: &self.clipboard.policy,
            vault: self.vault.as_ref(),
            vault_revision: self.vault_change_revision(),
        };
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        if matches!(action, ActionMessage::Close) {
            surface.assist = State::default();
            return Task::none();
        }
        if matches!(
            action,
            ActionMessage::Open | ActionMessage::OpenCompactCommand
        ) {
            let source = match surface.window.assist_snapshot() {
                Ok(source) => source,
                Err(error) => {
                    surface.window.assist_status(error);
                    return Task::none();
                }
            };
            let mut panel = Panel {
                source,
                recipe: Recipe {
                    action: if matches!(action, ActionMessage::OpenCompactCommand) {
                        Action::CompactCommand
                    } else {
                        Action::default()
                    },
                    ..Recipe::default()
                },
                profiles: Vec::new(),
                skills: Vec::new(),
                phase: Phase::Configure,
                status: None,
            };
            panel.configure(config.as_deref(), context.policy, true);
            if let Some(config) = &config {
                panel.recipe.skills =
                    config.suggest_snippet_skills(&panel.source.language, &panel.source.source);
            }
            surface.assist = State(Some(panel));
            return Task::none();
        }
        let Some(panel) = surface.assist.0.as_mut() else {
            return Task::none();
        };
        let opens_review = matches!(action, ActionMessage::Review | ActionMessage::Rerun);
        let result = match action {
            ActionMessage::Choose(action) if matches!(panel.phase, Phase::Configure) => {
                panel.recipe.action = action;
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
                panel.recipe.approved_transport = None;
                panel.recipe.approved_cli_revision = None;
                Ok(())
            }
            ActionMessage::TargetLanguage(value)
                if matches!(panel.phase, Phase::Configure) && value.len() <= 80 =>
            {
                panel.recipe.target_language = value;
                Ok(())
            }
            ActionMessage::Instructions(value)
                if matches!(panel.phase, Phase::Configure) && value.len() <= 16 * 1024 =>
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
                panel.review(&surface.window, config, &context, ticket)
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
                    return panel.start_quota(id, ticket);
                }
                match panel.start(&surface.window, config.as_ref(), &context, ticket) {
                    Ok(reply) => {
                        panel.status = None;
                        return Task::perform(
                            async move {
                                reply.recv().await.unwrap_or_else(|_| {
                                    Err("The snippet AI worker stopped.".into())
                                })
                            },
                            move |result| wrap(id, ActionMessage::Ready(ticket, result)),
                        );
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
                } else if !panel.current(&surface.window) {
                    Err(
                        "The editor changed while AI assistance was running. Review a new request."
                            .into(),
                    )
                } else {
                    result.and_then(|outcome| {
                        if let Some(outbound) = &outbound {
                            outbound.require_current(&approved, &context)?;
                            if context.vault.is_some_and(|vault| vault.summary_contains_secret(outcome.proposed_text())) {
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
                        panel.phase = Phase::Result(Box::new(Proposed {
                            config: approved,
                            outbound,
                            outcome,
                            diff,
                        }));
                        Ok(())
                    })
                }
            }
            ActionMessage::Apply => {
                if let Phase::Result(proposed) = &panel.phase
                    && proposed.diff.is_some()
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
                        && let Err(error) = outbound.require_current(&proposed.config, &context)
                    {
                        panel.status = Some(error);
                        return Task::none();
                    }
                    surface
                        .window
                        .accept_ai_change(
                            panel.source.clone(),
                            panel.recipe.clone(),
                            &proposed.outcome,
                        )
                        .map(|()| {
                            if let Some(change) = surface.window.last_ai_change() {
                                panel.phase =
                                    Phase::Last(change.clone(), diff_view(change.diff.clone()));
                            }
                        })
                } else {
                    Ok(())
                }
            }
            ActionMessage::CopyCommand => {
                let Phase::Result(proposed) = &panel.phase else {
                    return Task::none();
                };
                if panel.recipe.action != Action::CompactCommand || proposed.diff.is_some() {
                    return Task::none();
                }
                if !self.clipboard.policy.write_allowed || self.clipboard.policy.problem.is_some() {
                    Err("Managed policy blocks writing the clipboard.".into())
                } else if !panel.current(&surface.window)
                    || config
                        .as_ref()
                        .is_none_or(|config| !Arc::ptr_eq(config, &proposed.config))
                {
                    Err("The snippet or AI configuration changed. Review a new request.".into())
                } else if let Some(error) = proposed
                    .outbound
                    .as_ref()
                    .and_then(|outbound| outbound.require_current(&proposed.config, &context).err())
                {
                    Err(error)
                } else if context.vault.is_some_and(|vault| {
                    vault.summary_contains_secret(proposed.outcome.proposed_text())
                }) {
                    Err("The proposed command contains a vault credential.".into())
                } else {
                    let command = proposed.outcome.proposed_text().to_owned();
                    panel.status = Some("Reviewed snippet command copied.".into());
                    return self.write_clipboard(command);
                }
            }
            ActionMessage::ShowLast => {
                if let Some(change) = surface.window.last_ai_change() {
                    panel.phase = Phase::Last(change.clone(), diff_view(change.diff.clone()));
                }
                Ok(())
            }
            ActionMessage::Undo => {
                if let Phase::Last(change, _) = &panel.phase {
                    surface
                        .window
                        .undo_ai_change(change)
                        .and_then(|()| panel.back(&surface.window))
                } else {
                    Ok(())
                }
            }
            ActionMessage::Rerun if let Phase::Last(change, _) = &panel.phase => {
                let change = change.clone();
                surface
                    .window
                    .assist_snapshot()
                    .and_then(|source| change.rerun_source(source))
                    .and_then(|source| {
                        panel.recipe = change.recipe.clone();
                        panel.source = source;
                        panel.phase = Phase::Configure;
                        panel.review(&surface.window, config, &context, ticket)
                    })
            }
            ActionMessage::Fix => {
                if let Phase::Result(result) = &panel.phase
                    && result.diff.is_none()
                    && panel.recipe.action == Action::SecurityReview
                    && panel.current(&surface.window)
                    && config
                        .as_ref()
                        .is_some_and(|config| Arc::ptr_eq(config, &result.config))
                {
                    if let Some(outbound) = &result.outbound
                        && let Err(error) = outbound.require_current(&result.config, &context)
                    {
                        Err(error)
                    } else {
                        panel.recipe.findings = Some(result.outcome.text.clone());
                        panel.recipe.action = Action::SecurityFix;
                        panel.phase = Phase::Configure;
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
            ActionMessage::Back => panel.back(&surface.window),
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
                    .map(move |event| wrap(id, ActionMessage::Diff(event)));
            }
            _ => return Task::none(),
        };
        panel.status = result.err();
        if opens_review && panel.status.is_none() {
            panel.start_quota(id, ticket)
        } else {
            Task::none()
        }
    }
}

impl Panel {
    fn configure(
        &mut self,
        config: Option<&Configuration>,
        policy: &ClipboardPolicy,
        prefer_role: bool,
    ) {
        self.profiles = config
            .map(|config| {
                config
                    .snippet_analysis_profiles()
                    .map(|(id, name, model, destination, remote)| Choice {
                        id,
                        name: name.into(),
                        model: model.into(),
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
                        model: model.into(),
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
            self.recipe.profile = config
                .and_then(|config| {
                    config.preferred_snippet_assistance_profile_id(
                        self.recipe.action.uses_text_profile(),
                    )
                })
                .filter(|id| self.profiles.iter().any(|choice| choice.id == *id))
                .or_else(|| self.profiles.first().map(|choice| choice.id));
        }
        self.status = self
            .profiles
            .is_empty()
            .then(|| "Enable AI features and configure an available profile in AI Manager.".into());
    }

    fn current(&self, editor: &SnippetWindow) -> bool {
        editor
            .assist_snapshot()
            .is_ok_and(|current| self.source.same_source(&current))
    }

    fn back(&mut self, editor: &SnippetWindow) -> Result<(), String> {
        self.phase = Phase::Configure;
        self.source = editor.assist_snapshot()?;
        Ok(())
    }

    fn review(
        &mut self,
        editor: &SnippetWindow,
        config: Option<Arc<Configuration>>,
        context: &Context<'_>,
        ticket: u64,
    ) -> Result<(), String> {
        if !self.current(editor) {
            return Err("The snippet changed. Reopen AI assistance for the current draft.".into());
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
        let (request, _) = self.masked_request(
            &config,
            &transport,
            limit,
            &context.policy.terminal_logging,
            context.vault,
        )?;
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

    fn start_quota(&mut self, id: window::Id, ticket: u64) -> Task<Message> {
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
            move |forecast| wrap(id, ActionMessage::QuotaFinished(ticket, forecast)),
        )
    }

    fn start(
        &mut self,
        editor: &SnippetWindow,
        config: Option<&Arc<Configuration>>,
        context: &Context<'_>,
        ticket: u64,
    ) -> Result<async_channel::Receiver<Result<Outcome, String>>, String> {
        let Phase::Review(review) = std::mem::replace(&mut self.phase, Phase::Configure) else {
            return Err("Review the request before sending it.".into());
        };
        if !self.current(editor) || config.is_none_or(|config| !Arc::ptr_eq(config, &review.config))
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
        let (request, guidance) = self.masked_request(
            &review.config,
            &transport,
            limit,
            &context.policy.terminal_logging,
            context.vault,
        )?;
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
        let Pending { reply, cancel } = match (&review.outbound, review.cli) {
            (Some(Outbound::Cli(cli)), Some(prepared)) => {
                cli.require_current(&saved, context)?;
                request.start_cli(transport.clone(), prepared.request, cli.guard(saved))
            }
            (Some(Outbound::Https(https)), None) => {
                let https = https.clone();
                request.start(transport.clone(), move || https.authorize(&saved))
            }
            (None, None) => request.start(transport.clone(), || Ok(())),
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
        policy: &Policy,
        vault: Option<&VaultWindow>,
    ) -> Result<(Request, SkillGuidance), String> {
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
                    self.recipe.instructions.as_str(),
                    self.recipe.target_language.as_str(),
                    self.recipe.findings.as_deref().unwrap_or_default(),
                    guidance.text.as_str(),
                ] {
                    vault.add_summary_secrets(text, &secrets);
                }
            }
        }
        let secrets = secrets
            .seal()
            .map_err(|_| "Could not safely mask the snippet AI request.")?;
        let redactor = Redactor::for_journal(secrets, policy.clone());
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
        let mut request = Request::new(
            self.source.clone(),
            &self.recipe,
            config.code_text_language().explicit_code(),
            &guidance.text,
            limit,
            Arc::clone(&mask),
        )?;
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

    use iced::futures::{StreamExt, executor::block_on};
    use iced::widget::text_editor::{Action as EditorAction, Edit};
    use serde_json::json;

    use crate::local_ai::test_support::{accept, fixture_response, listener, request};
    use crate::snippets::tests::Profile as Scratch;

    use super::*;

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
        let mut vault = Vault::create(&path, b"snippet-assist-fixture").unwrap();
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
        assert_eq!(reviewed.choice.model, "reviewed-model");
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
            recipe: Recipe {
                action: Action::Describe,
                ..Default::default()
            },
            profiles: Vec::new(),
            skills: Vec::new(),
            phase: Phase::Configure,
            status: None,
        };
        assert!(matches!(
            panel.masked_request(&config, &transport, limit, &Policy::default(), None),
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
        let mut vault = Vault::create(&path, b"editor-cli-fixture").unwrap();
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
    fn snippet_assist_sends_reviewed_masked_payload_and_rechecks_rerun_profile() {
        let scratch = Scratch::new();
        let root = scratch.0.canonicalize().unwrap();
        let (listener, _) = listener();
        let endpoint = listener.local_addr().unwrap().to_string();
        let configuration = |model: &str| -> Configuration {
            serde_json::from_value(json!({
                "profiles": {
                    "profiles": [{
                        "id": 1, "name": "Fixture", "endpoint": endpoint, "model": model,
                        "usage_budget": {"token_limit": 2000}, "max_output_tokens": 321
                    }],
                    "default":1,"last_id":1,
                }
            }))
            .unwrap()
        };
        configuration("fixture-original")
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
        workspace.set_test_ai_configuration(configuration("fixture-changed"));
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
