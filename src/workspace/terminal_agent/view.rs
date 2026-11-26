use iced::widget::{
    button, checkbox, column, container, pick_list, responsive, row, scrollable, text, text_input,
};
use iced::{Element, Length, window};

use crate::terminal_agent::design::Approval;

use super::{Message, Mode, Phase, Purpose, Run, State, engine, palette, request_review};

#[cfg(test)]
mod tests;

impl Run {
    pub(super) fn view<'a>(
        &'a self,
        palette: Option<palette::Palette>,
        remaining_allowed: bool,
        reduced_motion: bool,
        owner: (&'a State, window::Id),
    ) -> Element<'a, Message> {
        if matches!(self.phase, Phase::ProbeApproval | Phase::Approval) || self.phase.finished() {
            return responsive(move |size| {
                // Leave room for the current review while keeping the history readable.
                let activity_share = if self.phase.finished() { 3.0 } else { 4.0 };
                let minimum_height =
                    if self.design.history == crate::terminal_agent::design::History::LogTable {
                        // Reserve room for the header, selected row, scrollbar and review controls.
                        96.0 + self.fonts.activity * 3.0
                    } else {
                        112.0
                    };
                let activity_height = (size.height / activity_share).max(minimum_height);
                self.view_body(
                    palette,
                    remaining_allowed,
                    reduced_motion,
                    owner,
                    Some(activity_height),
                )
            })
            .into();
        }
        self.view_body(palette, remaining_allowed, reduced_motion, owner, None)
    }

    fn view_body<'a>(
        &'a self,
        palette: Option<palette::Palette>,
        remaining_allowed: bool,
        reduced_motion: bool,
        owner: (&'a State, window::Id),
        activity_height: Option<f32>,
    ) -> Element<'a, Message> {
        let title = match self.mode {
            Mode::Run => "AI Agent",
            Mode::Ask => "Ask Agent",
            Mode::Plan => "AI Planning",
        };
        if self.phase == Phase::Setup && self.history.visible() {
            return column![
                text(title).size(22),
                text(&self.label),
                self.history.view(),
                text(&self.history.notice).size(13),
            ]
            .spacing(8)
            .padding(12)
            .into();
        }
        let mut page = column![text(title).size(22), text(&self.label)]
            .spacing(8)
            .padding(12);
        let plan_view = self.mode == Mode::Plan;
        let choosing_approach = self.phase == Phase::TriageChoice;
        let showing_plan = matches!(
            self.phase,
            Phase::PlanReady | Phase::PlanImplementationReview | Phase::PlanHandedOff
        ) && self.plan.is_some();
        let reviewing_model = self.phase == Phase::ModelReview
            || self
                .model_review
                .as_ref()
                .is_some_and(|review| review.model_tool_active());
        let body_size = if plan_view {
            self.fonts.plan
        } else {
            self.fonts.activity
        };
        if plan_view {
            page = page.push(super::font::View::Plan.controls(body_size, None));
        }
        if !reviewing_model && !showing_plan && !choosing_approach {
            page = page.push(super::font::View::Activity.controls(
                self.fonts.activity,
                (!self.binding.private()).then_some(self.fonts.remember_activity),
            ));
            page = page.push(super::design::activity_controls(self));
        }
        if self.design.arrangement == crate::terminal_agent::design::Arrangement::PictureInPicture
            && self.has_terminal_presentation()
            && !self.swarm
            && !self.closing
            && self.binding.live().is_ok()
        {
            page = page.push(
                button("Show floating history")
                    .on_press(Message::Pip(super::design::pip::Message::Show)),
            );
        }
        if let Some(target) = &self.target {
            page = page.push(text(format!("{} · {}", target.shell(), target.directory())).size(13));
        }
        if let Some(summary) = self
            .planning
            .as_ref()
            .and_then(super::planning::Flow::probe_summary)
        {
            page = page.push(text(summary).size(13));
        }
        if self.binding.private() {
            page = page.push(text("Private run. Activity is kept in memory.").size(13));
        }
        if let Some(context) =
            super::sent_context::view(self, super::sent_context::Surface::Activity)
        {
            page = page.push(context);
        }
        let show_directory = self.phase == Phase::Setup
            && matches!(self.mode, Mode::Run | Mode::Plan)
            || self.phase == Phase::DirectoryConfirmation
            || self.target.is_none()
                && matches!(
                    self.phase,
                    Phase::PlanReady | Phase::PlanImplementationReview
                );
        if show_directory && let Some(directory) = &self.manual_directory {
            page = page.push(
                column![
                    text("Confirm an absolute Windows working directory for this run.").size(13),
                    text_input("Absolute local directory", directory)
                        .on_input(Message::DirectoryChoice),
                ]
                .spacing(5),
            );
        }
        let directory_ready = self
            .manual_directory
            .as_ref()
            .is_none_or(|directory| !directory.trim().is_empty());
        if self.phase == Phase::DirectoryConfirmation {
            page = page.push(button("Confirm directory and review probe").on_press_maybe(
                (!self.paused && directory_ready).then_some(Message::ConfirmDirectory(self.epoch)),
            ));
        }
        if self.phase == Phase::Setup {
            page = page.push(
                column![
                    pick_list(self.profiles.as_slice(), self.profile().cloned(), Message::Profile),
                    text_input("Describe your task", &self.goal).on_input(Message::Goal).size(body_size),
                    checkbox(self.diagnostics.debug_enabled).label("Show Agent debug messages").on_toggle(Message::DebugMessages),
                    checkbox(self.diagnostics.runtime_enabled).label("Show Agent runtime messages")
                        .on_toggle_maybe((self.mode != Mode::Ask).then_some(Message::RuntimeMessages)),
                    (self.mode == Mode::Run).then(|| checkbox(self.auto_approve_root).label("Auto-approve commands, including sudo, for this run (root=true)").on_toggle(Message::AutoApproveRoot)),
                    (self.mode == Mode::Run).then(|| checkbox(self.confirm_every_command).label("Confirm every command in this run").on_toggle(Message::ConfirmEveryCommand)),
                    text(match self.mode {
                        Mode::Run if self.auto_approve_root => "Review the environment probe first. This run requests automatic command approval, including sudo. Policy and global confirmation settings override it; password authentication remains separate. If permitted, root=true overrides ask=true.",
                        Mode::Run if self.confirm_every_command => "Each command, including the environment probe, requires a separate approval.",
                        Mode::Run => "Review the environment probe first. Read-only commands may continue automatically; mutating commands require approval.",
                        Mode::Ask => "Review your question and any selected terminal text before sending them to this AI profile.",
                        Mode::Plan => "Review the environment probe and each model request separately. Implement is a separate action.",
                    }).size(13),
                    button(match self.mode {
                        Mode::Run => "Review environment probe",
                        Mode::Ask => "Review question",
                        Mode::Plan => "Start planning",
                    }).on_press_maybe((!self.profile_pending && !self.goal.trim().is_empty() && (self.mode != Mode::Run || directory_ready)).then_some(Message::Start)),
                    (self.mode == Mode::Plan).then(|| button("Plan without environment details").on_press_maybe(
                        (!self.profile_pending && !self.goal.trim().is_empty()).then_some(Message::StartPlanWithoutProbe),
                    )),
                ].spacing(8)
            );
        } else if !reviewing_model {
            page = page.push(text(&self.goal).size(body_size));
        }
        if self.mode == Mode::Plan
            && matches!(
                self.phase,
                Phase::DirectoryConfirmation | Phase::ProbeApproval
            )
        {
            page = page.push(
                button("Skip environment probe")
                    .on_press_maybe((!self.paused).then_some(Message::SkipPlanProbe(self.epoch))),
            );
        }
        if self.phase == Phase::Setup && self.mode == Mode::Plan && !self.binding.private() {
            page = page.push(
                button("Saved plans…")
                    .on_press(Message::Planning(super::planning::Message::Browse)),
            );
        }
        if self.phase == Phase::Setup && !self.binding.private() {
            page = page.push(self.history.view());
        }
        if !self.history.notice.is_empty() {
            page = page.push(text(&self.history.notice).size(13));
        }

        let show_activity = self.phase != Phase::Setup
            && !reviewing_model
            && !showing_plan
            && !choosing_approach
            && !self.presentation.activity.collapsed;
        let activity = || {
            let history = super::design::history_view(self, palette, reduced_motion, Some(owner));
            if let Some(height) = activity_height {
                // Resolve the nested table and its detail pane against a finite viewport.
                container(history).height(Length::Fixed(height)).into()
            } else {
                history
            }
        };
        if show_activity && self.design.approval == Approval::BelowActivity {
            page = page.push(activity());
        }
        if let Some(review) = self.model_review_view(Length::Fill) {
            page = page.push(review.map(Message::ReviewModel));
        }
        if choosing_approach && let Some(choice) = &self.triage.choice {
            page = page.push(choice.view(self));
        }
        if !reviewing_model
            && let Some(controls) = self
                .planning
                .as_ref()
                .and_then(|planning| planning.view(self))
        {
            page = page.push(controls);
        } else if showing_plan && let Some(report) = &self.plan {
            page = page.push(
                scrollable(text(report).size(body_size).width(Length::Fill)).height(Length::Fill),
            );
        }

        if let Some(command) = self.reviewed_command() {
            let password = self.reviewed_password_sudo();
            let root_approved = self
                .run_approval
                .as_ref()
                .is_some_and(super::policy::RunApproval::root_active);
            let approval: Element<'_, Message> = if let Some(review) = password {
                if self.sudo_terminal.is_some() {
                    button("Approve with this run's sudo ticket")
                        .on_press_maybe((!self.paused).then_some(Message::Password(
                            super::password::Message::Approve(
                                self.epoch,
                                super::password::Source::Ticket,
                            ),
                        )))
                        .into()
                } else {
                    row![
                        review.credential_label().map(|_| button(if root_approved {
                            "Authenticate sudo with vault password"
                        } else {
                            "Approve with vault password"
                        })
                        .on_press_maybe((!self.paused).then_some(Message::Password(
                            super::password::Message::Approve(
                                self.epoch,
                                super::password::Source::Vault,
                            ),
                        )))),
                        button(if root_approved {
                            "Authenticate sudo with manual password"
                        } else {
                            "Approve with manual password"
                        })
                        .on_press_maybe((!self.paused).then_some(
                            Message::Password(super::password::Message::Approve(
                                self.epoch,
                                super::password::Source::Manual,
                            ),)
                        ),),
                    ]
                    .spacing(6)
                    .wrap()
                    .into()
                }
            } else {
                row![
                    button(if self.phase == Phase::ProbeApproval {
                        "Approve probe once"
                    } else if self.reviewing_sudo() {
                        "Approve sudo once"
                    } else {
                        "Approve command once"
                    })
                    .on_press_maybe((!self.paused).then_some(Message::Approve(self.epoch))),
                    (remaining_allowed && self.offers_remaining_approval()).then(|| button(
                        "Approve future ordinary commands for this run"
                    )
                    .on_press_maybe(
                        (!self.paused).then_some(Message::ApproveRemaining(self.epoch))
                    )),
                ]
                .into()
            };
            page = page.push(
                column![
                    text(command.0).size(15),
                    (remaining_allowed && self.offers_remaining_approval()).then(|| text("Run-wide approval ends on pause, failure, elevation or changed settings. Password and elevation requests require separate review.").size(13)),
                    (self.reviewing_sudo() && password.is_none())
                        .then(|| text("Passwordless sudo as root over SSH").size(13)),
                    password.map(|review| column![
                        text("Password sudo as root").size(13),
                        text(review.destination()).size(13),
                        (self.sudo_terminal.is_none())
                            .then(|| review.credential_label())
                            .flatten()
                            .map(|label| text(format!("Vault credential: {label}")).size(13)),
                        text(if self.sudo_terminal.is_some() {
                            "This command will use the sudo ticket from this run."
                        } else {
                            "Choose the password source. The run requests the password after sudo prompts in the private terminal."
                        }).size(13),
                    ].spacing(3)),
                    scrollable(text(command.1).font(iced::Font::MONOSPACE))
                        .height(Length::Fixed(84.0)),
                    approval,
                ]
                .spacing(6),
            );
        }
        if self.phase == Phase::SudoPreflight
            && let Some(review) = &self.sudo_preflight
        {
            use super::password::{Message as Sudo, Source};
            let enabled = !self.paused && !self.cancel.requested();
            page = page.push(
                column![
                    text("Authenticate sudo for this implementation run").size(15),
                    text(review.destination()).size(13),
                    review
                        .credential_label()
                        .map(|label| text(format!("Vault credential: {label}")).size(13)),
                    row![
                        review.credential_label().map(|_| {
                            button("Authenticate with vault password").on_press_maybe(
                                enabled.then_some(Message::Password(Sudo::AuthenticatePlan(
                                    self.epoch,
                                    Source::Vault,
                                ))),
                            )
                        }),
                        button("Authenticate with manual password").on_press_maybe(
                            enabled.then_some(Message::Password(Sudo::AuthenticatePlan(
                                self.epoch,
                                Source::Manual,
                            ))),
                        ),
                        button("Continue to model review").on_press_maybe(enabled.then_some(
                            Message::Password(Sudo::SkipPlanAuthentication(self.epoch,))
                        ),),
                    ]
                    .spacing(6)
                    .wrap(),
                ]
                .spacing(6),
            );
        }
        if self.phase == Phase::SudoPassword
            && let Some(pending) = &self.password
        {
            page = page.push(
                column![
                    text("Authorize one sudo password submission").size(15),
                    text(pending.destination()).size(13),
                    pending
                        .credential_label()
                        .map(|label| text(format!("Vault credential: {label}")).size(13)),
                    button(if pending.credential_label().is_some() {
                        "Use this vault credential once"
                    } else {
                        "Enter password once…"
                    })
                    .on_press_maybe(
                        (!self.paused && !self.cancel.requested() && !pending.manual_open())
                            .then_some(Message::Password(super::password::Message::Use(
                                self.epoch
                            ))),
                    ),
                ]
                .spacing(6),
            );
        }
        #[cfg(not(target_os = "windows"))]
        if let Some(prompt) = self.manual_password_view() {
            page = page.push(prompt);
        }
        if let Some(question) = &self.question {
            page = page.push(question.view(self).map(Message::Clarification));
        }
        if self.phase == Phase::PlanImplementationReview {
            let enabled = !self.paused && !self.cancel.requested();
            page = page.push(
                row![
                    button("Accept plan and open Agent").on_press_maybe(
                        (enabled && directory_ready).then_some(Message::Implement(self.epoch)),
                    ),
                    button("Keep plan for later").on_press_maybe(enabled.then_some(
                        Message::Planning(super::planning::Message::KeepReport(self.epoch),)
                    )),
                ]
                .spacing(6)
                .wrap(),
            );
        } else if self.phase == Phase::PlanReady {
            page = page.push(button("Implement…").on_press_maybe(
                (!self.paused && directory_ready).then_some(Message::Implement(self.epoch)),
            ));
            if let Some(handoff) = super::design::handoff::bar(self) {
                page = page.push(handoff);
            }
        }
        if show_activity && self.design.approval == Approval::AboveActivity {
            page = page.push(activity());
        }
        use super::activity_export::Scope;
        let has_archive = owner.0.has_archived_activity(self);
        let export_scope = [Scope::This, Scope::All]
            .into_iter()
            .find(|&scope| self.can_export_activity(scope, has_archive));
        if let Some(export) = self.activity_export.view(export_scope) {
            page = page.push(export);
        }
        if self.can_generate_workflow() {
            page = page.push(
                button("Generate workflow script…").on_press(Message::Workflow(
                    super::workflow::Message::Open(self.epoch),
                )),
            );
        }
        if !matches!(
            self.phase,
            Phase::PlanReady | Phase::PlanImplementationReview
        ) && let Some(handoff) = super::design::handoff::bar(self)
        {
            page = page.push(handoff);
        }
        let notice_color = palette::notice_color(palette, self.phase);
        let notice = text(&self.notice).size(13).style(move |_| text::Style {
            color: notice_color,
        });
        let actions = container(
            row![
                button(if self.paused {
                    "Resume"
                } else {
                    "Pause after current step"
                })
                .on_press_maybe(
                    (self.phase != Phase::Setup
                        && !self.phase.finished()
                        && !self.cancel.requested())
                    .then_some(Message::Pause)
                ),
                button("Cancel run").on_press_maybe(
                    (self.phase != Phase::Setup
                        && !self.phase.finished()
                        && !self.cancel.requested())
                    .then_some(Message::Cancel)
                ),
                button("Copy transcript")
                    .on_press_maybe((!self.activity.is_empty()).then_some(Message::Copy)),
                (self.chat_handoff_dismissed && !self.chat_handoff_pending)
                    .then(|| super::design::handoff::compact(self))
                    .flatten(),
                button("Close").on_press(Message::Close),
            ]
            .spacing(8)
            .wrap(),
        )
        .id("terminal-agent-actions");
        if self.phase == Phase::Setup || choosing_approach || activity_height.is_some() {
            column![
                scrollable(page)
                    .id("terminal-agent-body")
                    .height(Length::Fill),
                column![notice, actions].spacing(8).padding(12),
            ]
            .height(Length::Fill)
            .into()
        } else {
            page.push(notice).push(actions).into()
        }
    }

    pub(super) fn model_review_view(
        &self,
        request_height: Length,
    ) -> Option<Element<'_, request_review::Message>> {
        use request_review::{Classification, Message as Review};

        if self.phase.deciding()
            && let Some(review) = &self.model_review
        {
            let ready = !self.paused && !self.closing && !self.cancel.requested();
            return review.model_tool_view(self.epoch, ready).map(|view| {
                container(view)
                    .height(match request_height {
                        Length::Fixed(_) => Length::Fixed(320.0),
                        height => height,
                    })
                    .into()
            });
        }

        if self.phase != Phase::ModelReview {
            return None;
        }
        let review = self.model_review.as_ref()?;
        let panel_height = match request_height {
            Length::Fixed(_) => Length::Shrink,
            height => height,
        };
        let active = !self.closing && !self.cancel.requested();
        let ready = active && !self.paused;
        let back = active.then_some(Review::CancelClassification(self.epoch));
        if review.web_active() {
            if ready {
                return review.web_view(self.epoch).map(|view| {
                    column![view]
                        .height(match request_height {
                            Length::Fixed(_) => Length::Fixed(320.0),
                            height => height,
                        })
                        .into()
                });
            }
            return Some(
                row![
                    text("Web search paused"),
                    button("Close web review").on_press_maybe(active.then_some(Review::Web(
                        self.epoch,
                        super::request_review::WebMessage::Close,
                    ))),
                ]
                .spacing(6)
                .into(),
            );
        }
        Some(match &review.classification {
            Classification::Closed => column![
                text(review.title()).size(15),
                review
                    .reviewed_destination()
                    .map(|(destination, model)| text(format!("{model} · {destination}")).size(13)),
                review.quota.label().map(|forecast| text(forecast).size(13)),
                review.knowledge_view(self.epoch, ready),
                scrollable(if let Some(cli) = &review.cli {
                    cli.view()
                } else {
                    column![
                        text("System instructions").size(13),
                        text(review.system.as_str()).wrapping(text::Wrapping::WordOrGlyph),
                        text("Masked request").size(13),
                        text(review.input.as_str())
                            .font(iced::Font::MONOSPACE)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(5)
                    .into()
                })
                .height(request_height),
                row![
                    self.ask_file.snapshot.as_ref().map(|_| {
                        button("Remove selected file").on_press_maybe(
                            ready.then_some(Review::RemoveAskAttachment(self.epoch)),
                        )
                    }),
                    review.triage_mode().map(|_| {
                        button("Skip complexity check")
                            .on_press_maybe(ready.then_some(Review::SkipTriage(self.epoch)))
                    }),
                    review.web_available().then(|| {
                        let (label, message) = if review.web_included() {
                            ("Remove web excerpts", request_review::WebMessage::Close)
                        } else {
                            ("Search web…", request_review::WebMessage::Open)
                        };
                        button(label).on_press_maybe(
                            (ready && !review.knowledge_busy())
                                .then_some(Review::Web(self.epoch, message)),
                        )
                    }),
                    review.classifier.as_ref().map(|_| {
                        button("Review skill classification…").on_press_maybe(
                            (ready && !review.knowledge_busy())
                                .then_some(Review::ReviewSkills(self.epoch)),
                        )
                    }),
                    button(review.send_label()).on_press_maybe(
                        (ready && review.knowledge_ready() && !review.quota.loading())
                            .then_some(Review::Send(self.epoch)),
                    ),
                ]
                .spacing(6)
                .wrap(),
            ]
            .spacing(6)
            .height(panel_height)
            .into(),
            Classification::Preview {
                text: preview,
                quota,
            } => column![
                text("Review masked skill classification request").size(15),
                quota.label().map(|forecast| text(forecast).size(13)),
                scrollable(
                    text(preview.as_str())
                        .font(iced::Font::MONOSPACE)
                        .wrapping(text::Wrapping::WordOrGlyph),
                )
                .height(request_height),
                row![
                    button("Back to request review").on_press_maybe(back),
                    button("Classify skills").on_press_maybe(
                        (ready && !quota.loading()).then_some(Review::Classify(self.epoch)),
                    ),
                ]
                .spacing(6)
                .wrap(),
            ]
            .spacing(6)
            .height(panel_height)
            .into(),
            Classification::Pending { .. } => column![
                text("Waiting for skill classification…").size(15),
                button("Cancel classification").on_press_maybe(back),
            ]
            .spacing(6)
            .into(),
        })
    }

    pub(super) fn reviewed_command(&self) -> Option<(&str, &str)> {
        match self.phase {
            Phase::ProbeApproval => Some((
                "Fixed environment probe",
                self.environment_probe(self.target.as_ref()?),
            )),
            Phase::Approval if let Purpose::Script(review) = &self.purpose => {
                Some(("Saved script", review.prepared.preview()))
            }
            Phase::Approval => self.commands.front().map(|command| {
                let purpose = match command.proposal.risk {
                    engine::Risk::ReadOnly | engine::Risk::RequiresConfirmation => {
                        command.proposal.purpose.as_str()
                    }
                };
                (purpose, command.text())
            }),
            _ => None,
        }
    }
}
