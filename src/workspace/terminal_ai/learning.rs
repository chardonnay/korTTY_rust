use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use iced::widget::{button, column, text, text_editor};
use iced::{Element, Length, Task, window};
use zeroize::Zeroizing;

use crate::ai_manager::{
    Configuration, RuleOrigin, RuleProposal, RuleSource, SavedRule, SavedSkillRule,
    SkillRuleProposal, SkillRuleTarget, review_memory_rule,
};
use crate::clipboard_policy::AiMemoryPolicy;

use super::{Chat, ConnectionMemory, Desktop, Message, edit, learning_view, metadata};

const MAX_DRAFT_BYTES: usize = 8 * 1024;
const UNDO_WINDOW: Duration = Duration::from_secs(30);
const NOT_WRITABLE: &str = "Rules are read-only or disabled under the current settings.";
const SKILLS_BUSY: &str = "Finish the AI Skills edit or wait for AI settings to finish saving.";

#[derive(Clone, Eq, PartialEq)]
pub struct RuleDestination(Target);

#[derive(Clone, Eq, PartialEq)]
enum Target {
    Global,
    Connection(u64, String),
    Skill(SkillRuleTarget),
}

impl RuleDestination {
    pub(super) fn is_skill(&self) -> bool {
        matches!(self.0, Target::Skill(_))
    }
}

impl fmt::Display for RuleDestination {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Target::Global => formatter.write_str("Global memory"),
            Target::Connection(_, name) => write!(formatter, "Connection memory: {name}"),
            Target::Skill(target) => fmt::Display::fmt(target, formatter),
        }
    }
}

struct Draft {
    input: text_editor::Content,
    connection: Option<RuleDestination>,
    origin: RuleOrigin,
    targets: Vec<RuleDestination>,
    selected: Option<RuleDestination>,
    explicit_target: bool,
    reviewed: Option<Reviewed>,
}

impl Draft {
    fn edit(&mut self, action: text_editor::Action) {
        let previous = (action.is_edit() && self.origin.topic.is_some())
            .then(|| Zeroizing::new(self.input.text()));
        edit(&mut self.input, action, MAX_DRAFT_BYTES);
        if previous.is_some_and(|text| text.as_str() != Zeroizing::new(self.input.text()).as_str())
        {
            self.origin.topic = None;
        }
    }

    fn retarget(&mut self, config: &Configuration, policy: &AiMemoryPolicy, host: Option<&str>) {
        let input = Zeroizing::new(self.input.text());
        let global = config
            .rule_budget(policy)
            .is_some()
            .then_some(RuleDestination(Target::Global));
        let connection = self.connection.as_ref().filter(|_| global.is_some());
        let mut suggested = connection
            .filter(|origin| {
                let Target::Connection(_, name) = &origin.0 else {
                    return false;
                };
                let lower = input.to_lowercase();
                [Some(name.as_str()), host]
                    .into_iter()
                    .flatten()
                    .any(|name| name.chars().count() >= 3 && lower.contains(&name.to_lowercase()))
            })
            .cloned();
        let mut targets: Vec<_> = connection
            .into_iter()
            .cloned()
            .chain(global.iter().cloned())
            .collect();
        if let Some(options) = config.skill_rule_targets(policy, &input, &self.origin) {
            suggested = suggested.or_else(|| options.suggested.map(skill_destination));
            targets.extend(options.targets.into_iter().map(skill_destination));
        }
        if !self.explicit_target
            || self
                .selected
                .as_ref()
                .is_none_or(|target| !targets.contains(target))
        {
            self.explicit_target = false;
            self.selected = suggested
                .filter(|target| targets.contains(target))
                .or(global)
                .or_else(|| targets.first().cloned());
        }
        self.targets = targets;
    }
}

fn skill_destination(target: SkillRuleTarget) -> RuleDestination {
    RuleDestination(Target::Skill(target))
}

#[derive(Clone)]
struct Binding {
    root: PathBuf,
    route: Option<Arc<ConnectionMemory>>,
}

impl Binding {
    fn connection(&self) -> Result<Option<u64>, String> {
        self.route
            .as_ref()
            .map(|route| {
                route.current_id().ok_or_else(|| {
                    "The saved connection changed. Choose an available memory destination.".into()
                })
            })
            .transpose()
    }
}

#[derive(Clone)]
enum Reviewed {
    Memory(Binding, Arc<RuleProposal>),
    Skill(PathBuf, Arc<SkillRuleProposal>),
}

impl Reviewed {
    fn preview(&self) -> (&str, &str) {
        match self {
            Self::Memory(_, proposal) => (proposal.current_text(), proposal.proposed_text()),
            Self::Skill(_, proposal) => (proposal.current_text(), proposal.proposed_text()),
        }
    }
}

#[derive(Clone)]
enum Saved {
    Memory(Binding, Arc<SavedRule>),
    Skill(PathBuf, Arc<SavedSkillRule>, Instant),
}

impl Saved {
    fn remaining(&self) -> Duration {
        match self {
            Self::Memory(_, rule) => rule.remaining(),
            Self::Skill(_, _, deadline) => deadline.saturating_duration_since(Instant::now()),
        }
    }
}

struct Receipt {
    token: u64,
    rule: Saved,
    _cancel_expiry: Option<mpsc::Sender<()>>,
}

#[derive(Clone)]
pub struct RuleOutcome(Result<Effect, String>);

impl RuleOutcome {
    pub(super) fn configuration(&self) -> Option<Arc<Configuration>> {
        match &self.0 {
            Ok(Effect::Saved(_, config) | Effect::Undone(config)) => config.clone(),
            _ => None,
        }
    }
}

#[derive(Clone)]
enum Effect {
    Reviewed(Reviewed),
    Saved(Saved, Option<Arc<Configuration>>),
    Undone(Option<Arc<Configuration>>),
}

pub(super) struct Context<'a> {
    pub(super) config: Option<&'a Arc<Configuration>>,
    pub(super) policy: &'a AiMemoryPolicy,
    pub(super) skill_write_allowed: bool,
}

struct Pending {
    token: u64,
    writing: bool,
}

#[derive(Default)]
pub(super) struct State {
    draft: Option<Draft>,
    pending: Option<Pending>,
    receipt: Option<Receipt>,
}

impl State {
    pub(super) fn active(&self) -> bool {
        self.draft.is_some() || self.pending.is_some()
    }

    pub(super) fn write_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.writing)
    }

    pub(super) fn writing_for(&self, token: u64) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.writing && pending.token == token)
    }

    pub(super) fn will_write_skill(&self, message: &Message) -> bool {
        match message {
            Message::ConfirmRule => matches!(
                self.draft
                    .as_ref()
                    .and_then(|draft| draft.reviewed.as_ref()),
                Some(Reviewed::Skill(..))
            ),
            Message::UndoRule => matches!(
                self.receipt.as_ref().map(|receipt| &receipt.rule),
                Some(Saved::Skill(..))
            ),
            _ => false,
        }
    }

    pub(super) fn can_undo(&self) -> bool {
        self.receipt
            .as_ref()
            .is_some_and(|receipt| !receipt.rule.remaining().is_zero())
    }

    pub(super) fn can_undo_with(&self, config: &Configuration, policy: &AiMemoryPolicy) -> bool {
        if !self.can_undo() {
            return false;
        }
        self.receipt
            .as_ref()
            .is_some_and(|receipt| match receipt.rule {
                Saved::Memory(..) => config.rule_budget(policy).is_some(),
                Saved::Skill(..) => config.skill_rule_enabled(policy),
            })
    }

    pub(super) fn view(&self, permitted: bool) -> Option<Element<'_, Message>> {
        if let Some(draft) = &self.draft {
            if !permitted && self.pending.is_none() {
                return Some(
                    column![
                        text("Remember as a rule").size(19),
                        text(NOT_WRITABLE),
                        button("Cancel").on_press(Message::CancelRule),
                    ]
                    .spacing(8)
                    .height(Length::Fill)
                    .into(),
                );
            }
            return Some(learning_view::editor(
                &draft.input,
                &draft.targets,
                draft.selected.as_ref(),
                draft.reviewed.as_ref().map(Reviewed::preview),
                self.pending.is_some(),
            ));
        }
        self.write_pending()
            .then(|| text("Updating the saved rule…").height(Length::Fill).into())
    }

    fn perform(
        &mut self,
        id: window::Id,
        token: u64,
        writing: bool,
        work: impl FnOnce() -> Result<Effect, String> + Send + 'static,
    ) -> Task<Desktop> {
        self.pending = Some(Pending { token, writing });
        Task::perform(async move { work() }, move |result| {
            Desktop::TerminalAi(id, Message::RuleFinished(token, RuleOutcome(result)))
        })
    }
}

impl Chat {
    pub(super) fn can_start_rule(&self) -> bool {
        self.storage.is_some()
            && self.review.is_none()
            && self.pending.is_none()
            && self.saving.is_none()
            && self.save_title.is_none()
            && !self.rules.active()
    }

    pub(super) fn refresh_rule_targets(
        &mut self,
        config: Option<&Configuration>,
        policy: &AiMemoryPolicy,
    ) {
        if self.rules.write_pending() {
            return;
        }
        let Some(draft) = &mut self.rules.draft else {
            return;
        };
        let pending_skill = self.rules.pending.is_some()
            && draft
                .selected
                .as_ref()
                .is_some_and(RuleDestination::is_skill);
        if (self.rules.pending.is_some() && !pending_skill)
            || matches!(draft.reviewed, Some(Reviewed::Memory(..)))
        {
            return;
        }
        if pending_skill {
            self.rules.pending = None;
        }
        if draft.reviewed.take().is_some() || pending_skill {
            self.notice = "AI Skills changed. Review the rule again.".into();
        }
        if let Some(config) = config {
            draft.retarget(
                config,
                policy,
                self.memory_target.as_ref().map(|route| route.host()),
            );
        } else {
            draft.targets.clear();
            draft.selected = None;
        }
    }

    fn rule_binding(&self, root: PathBuf, connection: Option<u64>) -> Option<Binding> {
        let route = if let Some(connection) = connection {
            let target = self
                .memory_target
                .clone()
                .filter(|route| route.saved_id() == Some(connection))?;
            Some(Arc::new(ConnectionMemory {
                catalog: root.join("connections.json"),
                target,
            }))
        } else {
            None
        };
        Some(Binding { root, route })
    }

    fn start_rule(&mut self, index: usize, config: &Configuration, policy: &AiMemoryPolicy) {
        let Some(turn) = self
            .turns
            .get(index)
            .filter(|turn| !turn.question().trim().is_empty())
        else {
            return;
        };
        self.rules.draft =
            Some(self.rule_draft(turn.question(), RuleOrigin::MANUAL, config, policy));
        self.notice.clear();
    }

    pub(super) fn start_suggested_rule(
        &mut self,
        rule: &str,
        topic: Option<String>,
        config: &Configuration,
        policy: &AiMemoryPolicy,
    ) -> bool {
        if !self.can_start_rule() || !config.automatic_rule_learning(policy) {
            return false;
        }
        let draft = self.rule_draft(
            rule,
            RuleOrigin {
                source: RuleSource::Chat,
                topic,
            },
            config,
            policy,
        );
        if draft.targets.is_empty() {
            return false;
        }
        self.rules.draft = Some(draft);
        self.notice.clear();
        true
    }

    fn rule_draft(
        &self,
        input: &str,
        origin: RuleOrigin,
        config: &Configuration,
        policy: &AiMemoryPolicy,
    ) -> Draft {
        let connection = self
            .storage
            .as_ref()
            .zip(self.memory_target.as_ref())
            .and_then(|(root, route)| {
                ConnectionMemory {
                    catalog: root.join("connections.json"),
                    target: route.clone(),
                }
                .with_current(|saved| {
                    let name = metadata(&saved.name, 128);
                    let label = if name.is_empty() {
                        metadata(route.host(), 64)
                    } else {
                        name
                    };
                    RuleDestination(Target::Connection(saved.id, label))
                })
            });
        let mut draft = Draft {
            input: text_editor::Content::with_text(input),
            connection,
            origin,
            targets: Vec::new(),
            selected: None,
            explicit_target: false,
            reviewed: None,
        };
        draft.retarget(
            config,
            policy,
            self.memory_target.as_ref().map(|route| route.host()),
        );
        draft
    }

    pub(super) fn update_rule(
        &mut self,
        id: window::Id,
        message: Message,
        request: Option<u64>,
        context: Context<'_>,
    ) -> Task<Desktop> {
        match message {
            Message::RuleFinished(token, outcome) => return self.finish_rule(id, token, outcome),
            Message::RuleExpired(token) => {
                if self
                    .rules
                    .receipt
                    .as_ref()
                    .is_some_and(|receipt| receipt.token == token)
                {
                    self.rules.receipt = None;
                }
                return Task::none();
            }
            _ => {}
        }
        if self.rules.pending.is_some() {
            return Task::none();
        }
        if matches!(message, Message::CancelRule) {
            self.rules.draft = None;
            self.notice.clear();
            return Task::none();
        }
        let policy = context.policy;
        let Some(config) = context.config.filter(|config| {
            config.rule_budget(policy).is_some() || config.skill_rule_enabled(policy)
        }) else {
            self.notice = NOT_WRITABLE.into();
            return Task::none();
        };
        if !context.skill_write_allowed && self.rules.will_write_skill(&message) {
            self.notice = SKILLS_BUSY.into();
            return Task::none();
        }
        match message {
            Message::RememberRule(index) if self.can_start_rule() => {
                self.start_rule(index, config, policy)
            }
            Message::RuleEdit(action) => {
                if let Some(draft) = self
                    .rules
                    .draft
                    .as_mut()
                    .filter(|draft| draft.reviewed.is_none())
                {
                    draft.edit(action);
                    draft.retarget(
                        config,
                        policy,
                        self.memory_target.as_ref().map(|route| route.host()),
                    );
                    self.notice.clear();
                }
            }
            Message::RuleTarget(target) => {
                if let Some(draft) = self
                    .rules
                    .draft
                    .as_mut()
                    .filter(|draft| draft.reviewed.is_none() && draft.targets.contains(&target))
                {
                    draft.selected = Some(target);
                    draft.explicit_target = true;
                    self.notice.clear();
                }
            }
            Message::EditRule => {
                if let Some(draft) = &mut self.rules.draft {
                    draft.reviewed = None;
                    draft.retarget(
                        config,
                        policy,
                        self.memory_target.as_ref().map(|route| route.host()),
                    );
                    self.notice.clear();
                }
            }
            Message::ReviewRule => {
                return self.review_rule(id, request.expect("rule review token"), config, policy);
            }
            Message::ConfirmRule => {
                return self.confirm_rule(id, request.expect("rule write token"), config, policy);
            }
            Message::UndoRule if self.can_start_rule() => {
                return self.undo_rule(id, request.expect("rule undo token"), config, policy);
            }
            _ => {}
        }
        Task::none()
    }

    fn review_rule(
        &mut self,
        id: window::Id,
        token: u64,
        config: &Arc<Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Task<Desktop> {
        if let Some(draft) = self
            .rules
            .draft
            .as_mut()
            .filter(|draft| draft.reviewed.is_none())
        {
            draft.retarget(
                config,
                policy,
                self.memory_target.as_ref().map(|route| route.host()),
            );
        }
        let Some((root, draft)) = self
            .storage
            .clone()
            .zip(self.rules.draft.as_ref())
            .filter(|(_, draft)| draft.reviewed.is_none())
        else {
            return Task::none();
        };
        let Some(target) = draft.selected.as_ref() else {
            return Task::none();
        };
        let input = Zeroizing::new(draft.input.text());
        let origin = draft.origin.clone();
        let redactor = Arc::clone(&self.redactor);
        let target = target.0.clone();
        let binding = match target {
            Target::Global => self.rule_binding(root.clone(), None),
            Target::Connection(id, _) => self.rule_binding(root.clone(), Some(id)),
            Target::Skill(_) => None,
        };
        let config = Arc::clone(config);
        let policy = policy.clone();
        self.notice = "Preparing the rule review…".into();
        self.rules.perform(id, token, false, move || {
            let secret = "The rule contains a known credential.".to_owned();
            if redactor.contains_known_secret(&input) {
                return Err(secret);
            }
            let reviewed = match target {
                Target::Skill(target) => {
                    let proposal = config.review_skill_rule(&policy, &target, &input, &origin)?;
                    if redactor.contains_known_secret(proposal.rule()) {
                        return Err(secret);
                    }
                    Reviewed::Skill(root, Arc::new(proposal))
                }
                _ => {
                    let binding = binding.ok_or("Choose an available memory destination.")?;
                    let budget = config.rule_budget(&policy).ok_or(NOT_WRITABLE)?;
                    let connection = binding.connection()?;
                    let proposal = review_memory_rule(
                        &binding.root,
                        connection,
                        &input,
                        budget,
                        origin.source,
                    )
                    .map_err(|error| error.to_string())?;
                    if redactor.contains_known_secret(proposal.rule()) {
                        return Err(secret);
                    }
                    Reviewed::Memory(binding, Arc::new(proposal))
                }
            };
            Ok(Effect::Reviewed(reviewed))
        })
    }

    fn confirm_rule(
        &mut self,
        id: window::Id,
        token: u64,
        config: &Arc<Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Task<Desktop> {
        let Some(reviewed) = self
            .rules
            .draft
            .as_ref()
            .and_then(|draft| draft.reviewed.clone())
        else {
            return Task::none();
        };
        let permitted = match reviewed {
            Reviewed::Memory(..) => config.rule_budget(policy).is_some(),
            Reviewed::Skill(..) => config.skill_rule_enabled(policy),
        };
        if !permitted {
            self.notice = NOT_WRITABLE.into();
            return Task::none();
        }
        let config = Arc::clone(config);
        let policy = policy.clone();
        self.notice = "Saving the reviewed rule…".into();
        self.rules.perform(id, token, true, move || match reviewed {
            Reviewed::Memory(binding, proposal) => {
                binding.connection()?;
                let budget = config.rule_budget(&policy).ok_or(NOT_WRITABLE)?;
                let saved = proposal.save(budget).map_err(|error| error.to_string())?;
                Ok(Effect::Saved(Saved::Memory(binding, Arc::new(saved)), None))
            }
            Reviewed::Skill(root, proposal) => {
                let (config, rule) = config.save_skill_rule(&root, &policy, &proposal)?;
                Ok(Effect::Saved(
                    Saved::Skill(root, Arc::new(rule), Instant::now() + UNDO_WINDOW),
                    Some(config),
                ))
            }
        })
    }

    fn undo_rule(
        &mut self,
        id: window::Id,
        token: u64,
        config: &Arc<Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Task<Desktop> {
        if !self.rules.can_undo_with(config, policy) {
            self.notice = "Undo is unavailable for this rule.".into();
            return Task::none();
        }
        let saved = self
            .rules
            .receipt
            .as_ref()
            .expect("undo receipt")
            .rule
            .clone();
        let config = Arc::clone(config);
        let policy = policy.clone();
        self.notice = "Undoing the saved rule…".into();
        self.rules.perform(id, token, true, move || match saved {
            Saved::Memory(binding, rule) => {
                binding.connection()?;
                rule.undo().map_err(|error| error.to_string())?;
                Ok(Effect::Undone(None))
            }
            Saved::Skill(root, rule, deadline) => {
                if Instant::now() >= deadline {
                    return Err("The undo period ended.".into());
                }
                Ok(Effect::Undone(Some(
                    config.undo_skill_rule(&root, &policy, &rule)?,
                )))
            }
        })
    }

    fn finish_rule(&mut self, id: window::Id, token: u64, outcome: RuleOutcome) -> Task<Desktop> {
        let Some(pending) = self.rules.pending.take_if(|pending| pending.token == token) else {
            return Task::none();
        };
        match outcome.0 {
            Ok(Effect::Reviewed(reviewed)) => {
                if let Some(draft) = &mut self.rules.draft {
                    draft.reviewed = Some(reviewed);
                    self.notice.clear();
                }
            }
            Ok(Effect::Saved(rule, _)) => {
                self.rules.draft = None;
                self.notice = "Rule saved.".into();
                let (cancel, expiry) = expire_receipt(id, token, rule.remaining());
                self.rules.receipt = Some(Receipt {
                    token,
                    rule,
                    _cancel_expiry: cancel,
                });
                return expiry;
            }
            Ok(Effect::Undone(config)) => {
                self.rules.receipt = None;
                self.notice = if config.is_some() {
                    "Skill rule undone."
                } else {
                    "Rule moved to the archive."
                }
                .into();
            }
            Err(error) => {
                if pending.writing
                    && let Some(draft) = &mut self.rules.draft
                {
                    draft.reviewed = None;
                }
                self.notice = error;
            }
        }
        Task::none()
    }
}

fn expire_receipt(
    id: window::Id,
    token: u64,
    delay: Duration,
) -> (Option<mpsc::Sender<()>>, Task<Desktop>) {
    let (timer, cancel) = mpsc::channel();
    let (send, receive) = async_channel::bounded(1);
    if thread::Builder::new()
        .name("kortty-rule-undo".into())
        .spawn(move || {
            if cancel.recv_timeout(delay) == Err(mpsc::RecvTimeoutError::Timeout) {
                let _ = send.try_send(());
            }
        })
        .is_err()
    {
        return (None, Task::none());
    }
    let expiry = Task::run(receive, move |()| {
        Desktop::TerminalAi(id, Message::RuleExpired(token))
    });
    (Some(timer), expiry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_rule_drops_only_its_topic_on_an_actual_edit() {
        let mut draft = Draft {
            input: text_editor::Content::with_text("Keep the indentation consistent."),
            connection: None,
            origin: RuleOrigin {
                source: RuleSource::Chat,
                topic: Some("rust".into()),
            },
            targets: Vec::new(),
            selected: None,
            explicit_target: false,
            reviewed: None,
        };
        draft.edit(text_editor::Action::Move(text_editor::Motion::Home));
        draft.edit(text_editor::Action::Edit(text_editor::Edit::Paste(
            Arc::new("x".repeat(MAX_DRAFT_BYTES + 1)),
        )));
        assert_eq!(draft.origin.topic.as_deref(), Some("rust"));
        draft.edit(text_editor::Action::Edit(text_editor::Edit::Insert('X')));
        assert!(draft.origin.topic.is_none());
        assert_eq!(draft.origin.source, RuleSource::Chat);
    }
}
