//! Reviewed rule detection and proposal cards for new terminal chat turns.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use iced::{
    Element, Length, Task,
    widget::{button, checkbox, column, container, row, text, text_editor},
    window,
};
use zeroize::Zeroizing;

use crate::{
    ai_manager::{Configuration, RuleOrigin, RuleSource},
    clipboard_policy::AiMemoryPolicy,
    local_ai,
};

use super::{
    Chat, Desktop, Message, Turn, automatic_rule_classifier as classifier,
    automatic_rule_ledger as ledger, mask,
};

const TIMEOUT: Duration = Duration::from_secs(90);
const OUTPUT_TOKENS: u32 = 512;

#[derive(Clone)]
struct Settings {
    config: Arc<Configuration>,
    policy: AiMemoryPolicy,
}

impl Settings {
    fn matches(&self, config: Option<&Arc<Configuration>>, policy: &AiMemoryPolicy) -> bool {
        self.policy == *policy
            && config.is_some_and(|config| {
                Arc::ptr_eq(config, &self.config) && config.automatic_rule_learning(policy)
            })
    }
}

pub(super) struct Review {
    request: classifier::ClassifierRequest,
    settings: Settings,
    pub(super) preview: text_editor::Content,
    pub(super) included: bool,
    pub(super) showing: bool,
}

impl Review {
    pub(super) fn new(
        original: &str,
        question: &str,
        config: &Arc<Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Option<Self> {
        if !config.automatic_rule_learning(policy)
            || !has_destination(config, policy, question, &RuleOrigin::MANUAL)
        {
            return None;
        }
        let request = classifier::prepare(original, question)?;
        let preview = local_ai::preview_chat(classifier::SYSTEM_PROMPT, &[], &request.user).ok()?;
        Some(Self {
            request,
            settings: Settings {
                config: Arc::clone(config),
                policy: policy.clone(),
            },
            preview: text_editor::Content::with_text(&Zeroizing::new(preview)),
            included: false,
            showing: false,
        })
    }

    pub(super) fn matches(
        &self,
        config: Option<&Arc<Configuration>>,
        policy: &AiMemoryPolicy,
    ) -> bool {
        self.settings.matches(config, policy)
    }

    pub(super) fn controls(&self, editable: bool) -> Element<'_, Message> {
        column![
            checkbox(self.included)
                .label("Check for a reusable rule after the answer")
                .on_toggle_maybe(editable.then_some(Message::IncludeRuleSuggestion)),
            self.included.then(|| row![
                text("A separate request sends only the reviewed question to this profile, within the daily limits.")
                    .size(13).width(Length::Fill),
                button(if self.showing { "View chat request" } else { "View rule request" })
                    .style(button::secondary)
                    .on_press_maybe(editable.then_some(Message::ShowRuleSuggestion)),
            ].spacing(8)),
        ]
        .spacing(5)
        .into()
    }
}

fn has_destination(
    config: &Configuration,
    policy: &AiMemoryPolicy,
    text: &str,
    origin: &RuleOrigin,
) -> bool {
    config.rule_budget(policy).is_some()
        || config
            .skill_rule_targets(policy, text, origin)
            .is_some_and(|options| !options.targets.is_empty())
}

struct Proposal {
    candidate: classifier::Candidate,
    message: ledger::Fingerprint,
    rule: ledger::Fingerprint,
}

struct Card {
    token: u64,
    turn: Weak<Turn>,
    root: PathBuf,
    settings: Settings,
    proposal: Arc<Proposal>,
}

struct Pending {
    token: u64,
    turn: Weak<Turn>,
    root: PathBuf,
    settings: Settings,
    active: Arc<AtomicBool>,
    cancel: Option<local_ai::Cancel>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Outcome(ResultState);

#[derive(Clone)]
enum ResultState {
    Detected(Option<Arc<Proposal>>),
    Claimed(Option<Arc<Proposal>>),
    Feedback(bool),
}

#[derive(Default)]
pub(super) struct State {
    pending: Option<Pending>,
    cards: Vec<Card>,
}

impl State {
    pub(super) fn invalidate(
        &mut self,
        config: Option<&Arc<Configuration>>,
        policy: &AiMemoryPolicy,
    ) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| !pending.settings.matches(config, policy))
        {
            self.cancel_pending();
        }
        self.cards
            .retain(|card| card.settings.matches(config, policy));
    }

    pub(super) fn cancel_pending(&mut self) {
        self.pending = None;
    }

    pub(super) fn prune_cards(&mut self, turns: &[Arc<Turn>]) {
        self.cards.retain(|card| {
            card.turn
                .upgrade()
                .is_some_and(|turn| turns.iter().any(|current| Arc::ptr_eq(current, &turn)))
        });
    }

    pub(super) fn view(&self, turn: &Arc<Turn>, editable: bool) -> Option<Element<'_, Message>> {
        let card = self
            .cards
            .iter()
            .find(|card| Weak::ptr_eq(&card.turn, &Arc::downgrade(turn)))?;
        Some(
            container(
                column![
                    text("Suggested rule").size(14),
                    text(card.proposal.candidate.rule.as_str()),
                    row![
                        button("Review rule…").on_press_maybe(
                            editable.then_some(Message::UseRuleSuggestion(card.token))
                        ),
                        button("Not now").style(button::secondary).on_press_maybe(
                            editable.then_some(Message::DismissRuleSuggestion(card.token, false))
                        ),
                        button("Never suggest this rule")
                            .style(button::secondary)
                            .on_press_maybe(
                                editable
                                    .then_some(Message::DismissRuleSuggestion(card.token, true))
                            ),
                    ]
                    .spacing(8)
                    .wrap(),
                ]
                .spacing(6),
            )
            .width(Length::Fill)
            .padding(8)
            .style(container::bordered_box)
            .into(),
        )
    }
}

impl Chat {
    pub(super) fn start_rule_detection(
        &mut self,
        id: window::Id,
        token: u64,
        transport: local_ai::Profile,
        review: Review,
        turn: &Arc<Turn>,
    ) -> Task<Desktop> {
        let Some(root) = self.storage.clone() else {
            return Task::none();
        };
        let worker_root = root.clone();
        let question = Zeroizing::new(turn.question().to_owned());
        let redactor = Arc::clone(&self.redactor);
        let active = Arc::new(AtomicBool::new(true));
        let worker_active = Arc::clone(&active);
        let worker = local_ai::start(transport.with_output_limit(OUTPUT_TOKENS), move |client| {
            Ok(detect(
                client,
                &worker_root,
                &question,
                &review.request.user,
                &redactor,
                &worker_active,
            )
            .map(Arc::new))
        });
        let Ok(worker) = worker else {
            return Task::none();
        };
        self.automatic.pending = Some(Pending {
            token,
            turn: Arc::downgrade(turn),
            root,
            settings: review.settings,
            active,
            cancel: Some(worker.cancel),
        });
        Task::perform(
            async move {
                worker
                    .reply
                    .recv()
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
            },
            move |result| {
                Desktop::TerminalAi(
                    id,
                    Message::RuleSuggestionFinished(token, Outcome(ResultState::Detected(result))),
                )
            },
        )
    }

    pub(super) fn update_rule_suggestion(
        &mut self,
        id: window::Id,
        message: Message,
        config: Option<&Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Task<Desktop> {
        match message {
            Message::RuleSuggestionFinished(_, Outcome(ResultState::Feedback(success))) => {
                if !success && self.notice.is_empty() {
                    self.notice = "Could not save the rule suggestion preference.".into();
                }
            }
            Message::RuleSuggestionFinished(token, Outcome(result)) => {
                return self.finish_rule_detection(id, token, result, config, policy);
            }
            Message::UseRuleSuggestion(token) if self.can_start_rule() => {
                if let Some(index) = self
                    .automatic
                    .cards
                    .iter()
                    .position(|card| card.token == token)
                    && let Some(config) = config
                    && config.automatic_rule_learning(policy)
                {
                    let proposal = Arc::clone(&self.automatic.cards[index].proposal);
                    if self.start_suggested_rule(
                        &proposal.candidate.rule,
                        proposal.candidate.topic.clone(),
                        config,
                        policy,
                    ) {
                        self.automatic.cards.remove(index);
                        self.automatic.cancel_pending();
                    }
                }
            }
            Message::DismissRuleSuggestion(token, permanently) if self.can_start_rule() => {
                if let Some(index) = self
                    .automatic
                    .cards
                    .iter()
                    .position(|card| card.token == token)
                {
                    let card = self.automatic.cards.remove(index);
                    let feedback = if permanently {
                        ledger::Feedback::Reject
                    } else {
                        ledger::Feedback::Snooze
                    };
                    return Task::perform(
                        async move {
                            ledger::record_feedback(
                                &card.root,
                                &card.proposal.message,
                                &card.proposal.rule,
                                feedback,
                            )
                            .is_ok()
                        },
                        move |success| {
                            Desktop::TerminalAi(
                                id,
                                Message::RuleSuggestionFinished(
                                    token,
                                    Outcome(ResultState::Feedback(success)),
                                ),
                            )
                        },
                    );
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn finish_rule_detection(
        &mut self,
        id: window::Id,
        token: u64,
        result: ResultState,
        config: Option<&Configuration>,
        policy: &AiMemoryPolicy,
    ) -> Task<Desktop> {
        let can_show = self.storage.is_some()
            && self.pending.is_none()
            && self.review.is_none()
            && self.save_title.is_none()
            && !self.rules.active();
        let Some(pending) = self
            .automatic
            .pending
            .as_mut()
            .filter(|pending| pending.token == token)
        else {
            return Task::none();
        };
        let turn_is_visible = pending
            .turn
            .upgrade()
            .is_some_and(|turn| self.turns.iter().any(|current| Arc::ptr_eq(current, &turn)));
        if !can_show || !turn_is_visible {
            self.automatic.cancel_pending();
            return Task::none();
        }
        match result {
            ResultState::Detected(Some(proposal)) if pending.cancel.take().is_some() => {
                let origin = RuleOrigin {
                    source: RuleSource::Chat,
                    topic: proposal.candidate.topic.clone(),
                };
                if !config.is_some_and(|config| {
                    has_destination(config, policy, &proposal.candidate.rule, &origin)
                }) {
                    self.automatic.cancel_pending();
                    return Task::none();
                }
                let root = pending.root.clone();
                let active = Arc::clone(&pending.active);
                Task::perform(
                    async move {
                        ledger::claim_card(&root, &proposal.message, &proposal.rule, || {
                            active.load(Ordering::Acquire)
                        })
                        .unwrap_or(false)
                        .then_some(proposal)
                    },
                    move |result| {
                        Desktop::TerminalAi(
                            id,
                            Message::RuleSuggestionFinished(
                                token,
                                Outcome(ResultState::Claimed(result)),
                            ),
                        )
                    },
                )
            }
            ResultState::Claimed(Some(proposal)) if pending.cancel.is_none() => {
                self.automatic.cards.push(Card {
                    token,
                    turn: pending.turn.clone(),
                    root: pending.root.clone(),
                    settings: pending.settings.clone(),
                    proposal,
                });
                self.automatic.cancel_pending();
                Task::none()
            }
            _ => {
                self.automatic.cancel_pending();
                Task::none()
            }
        }
    }
}

fn detect(
    client: &local_ai::Client,
    root: &Path,
    question: &str,
    user: &str,
    redactor: &crate::terminal_logging::Redactor,
    active: &AtomicBool,
) -> Option<Proposal> {
    let message = ledger::fingerprint(question).ok()?;
    if !ledger::reserve_detection(root, &message, || active.load(Ordering::Acquire)).ok()? {
        return None;
    }
    if !active.load(Ordering::Acquire) {
        return None;
    }
    let reply = Zeroizing::new(
        client
            .complete_text_before(
                classifier::SYSTEM_PROMPT,
                user,
                false,
                Instant::now() + TIMEOUT,
            )
            .ok()?,
    );
    let candidate = classifier::parse_reply(&reply)?;
    let safe = |value: &str| {
        !redactor.contains_known_secret(value)
            && mask::prompt(redactor, value).is_ok_and(|masked| masked == value)
    };
    if !safe(&candidate.rule) || !candidate.topic.as_deref().is_none_or(safe) {
        return None;
    }
    let rule = ledger::fingerprint(&candidate.rule).ok()?;
    Some(Proposal {
        candidate,
        message,
        rule,
    })
}
