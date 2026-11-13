use iced::widget::{button, column, pick_list, row, scrollable, text, text_input};
use iced::{Element, Length};

use crate::terminal_agent::design::Approval;

use super::{Message, Mode, Phase, Purpose, Run, engine, palette, request_review};

impl Run {
    pub(super) fn view(&self, palette: Option<palette::Palette>) -> Element<'_, Message> {
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
        if self.design.arrangement == crate::terminal_agent::design::Arrangement::PictureInPicture
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
        if self.binding.private() {
            page = page.push(text("Private run. Activity is kept in this window.").size(13));
        }
        let show_directory = self.phase == Phase::Setup && self.mode == Mode::Run
            || matches!(self.phase, Phase::DirectoryConfirmation | Phase::PlanReady);
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
        let reviewing_model = self.phase == Phase::ModelReview;
        if self.phase == Phase::Setup {
            page = page.push(
                column![
                    pick_list(self.profiles.as_slice(), self.profile().cloned(), Message::Profile),
                    text_input("Describe your task", &self.goal).on_input(Message::Goal),
                    text(match self.mode {
                        Mode::Run => "Each command, including the environment probe, requires a separate approval.",
                        Mode::Ask => "Review your question and any selected terminal text before sending them to this AI profile.",
                        Mode::Plan => "Review each planning request before sending it to this AI profile. Implement is a separate action.",
                    }).size(13),
                    button(match self.mode {
                        Mode::Run => "Review environment probe",
                        Mode::Ask => "Review question",
                        Mode::Plan => "Review planning request",
                    }).on_press_maybe((!self.profile_pending && !self.goal.trim().is_empty() && (self.mode != Mode::Run || directory_ready)).then_some(Message::Start)),
                ].spacing(8)
            );
        } else if !reviewing_model {
            page = page.push(text(&self.goal));
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

        if !reviewing_model && self.design.approval == Approval::BelowActivity {
            page = page.push(self.activity_view(palette));
        }
        if let Some(review) = self.model_review_view(Length::Fill) {
            page = page.push(review.map(Message::ReviewModel));
        }
        if !reviewing_model
            && let Some(controls) = self
                .planning
                .as_ref()
                .and_then(|planning| planning.view(self))
        {
            page = page.push(controls);
        }

        if let Some(command) = self.reviewed_command() {
            let password = self.reviewed_password_sudo();
            let approval: Element<'_, Message> = if let Some(review) = password {
                row![
                    review
                        .credential_label()
                        .map(|_| button("Approve with vault password").on_press_maybe(
                            (!self.paused).then_some(Message::Password(
                                super::password::Message::Approve(
                                    self.epoch,
                                    super::password::Source::Vault
                                )
                            )),
                        )),
                    super::password::MANUAL_AVAILABLE.then(|| button(
                        "Approve with manual password"
                    )
                    .on_press_maybe((!self.paused).then_some(Message::Password(
                        super::password::Message::Approve(
                            self.epoch,
                            super::password::Source::Manual
                        )
                    )),)),
                ]
                .spacing(6)
                .wrap()
                .into()
            } else {
                row![
                    button(if self.phase == Phase::ProbeApproval {
                        "Approve probe once"
                    } else if self.reviewing_sudo() {
                        "Approve sudo once"
                    } else {
                        "Approve command once"
                    })
                    .on_press_maybe((!self.paused).then_some(Message::Approve(self.epoch)))
                ]
                .into()
            };
            page = page.push(
                column![
                    text(command.0).size(15),
                    (self.reviewing_sudo() && password.is_none())
                        .then(|| text("Passwordless sudo as root over SSH").size(13)),
                    password.map(|review| column![
                        text("Password sudo as root over attached SSH").size(13),
                        text(review.destination()).size(13),
                        review.credential_label().map(|label| text(format!("Vault credential: {label}")).size(13)),
                        text("Choose the password source when approving this command. Submitting the password requires a separate action after the SSH challenge.").size(13),
                    ].spacing(3)),
                    scrollable(text(command.1).font(iced::Font::MONOSPACE))
                        .height(Length::Fixed(84.0)),
                    approval,
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
        if let Some((question, answers)) = &self.question {
            let mut choices = row![].spacing(6);
            for answer in answers {
                choices = choices.push(button(text(answer)).on_press_maybe(
                    (!self.paused).then(|| Message::Answer(self.epoch, answer.clone())),
                ));
            }
            page = page.push(column![text(question), choices.wrap()].spacing(6));
        }
        if self.phase == Phase::PlanReady {
            page = page.push(button("Implement…").on_press_maybe(
                (!self.paused && directory_ready).then_some(Message::Implement(self.epoch)),
            ));
            if let Some(handoff) = super::design::handoff::bar(self) {
                page = page.push(handoff);
            }
        }
        if !reviewing_model && self.design.approval == Approval::AboveActivity {
            page = page.push(self.activity_view(palette));
        }
        if let Some(export) = self.activity_export.view() {
            page = page.push(export);
        }
        if self.can_generate_workflow() {
            page = page.push(
                button("Generate workflow script…").on_press(Message::Workflow(
                    super::workflow::Message::Open(self.epoch),
                )),
            );
        }
        if self.phase != Phase::PlanReady
            && let Some(handoff) = super::design::handoff::bar(self)
        {
            page = page.push(handoff);
        }
        let notice_color = palette::notice_color(palette, self.phase);
        page.push(text(&self.notice).size(13).style(move |_| text::Style {
            color: notice_color,
        }))
        .push(
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
        .into()
    }

    fn activity_view(&self, palette: Option<palette::Palette>) -> Element<'_, Message> {
        super::design::history_view(self, palette)
    }

    pub(super) fn model_review_view(
        &self,
        request_height: Length,
    ) -> Option<Element<'_, request_review::Message>> {
        use request_review::{Classification, Message as Review};

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
        Some(match &review.classification {
            Classification::Closed => column![
                text(review.title()).size(15),
                review
                    .outbound_destination()
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
                self.target.as_ref()?.probe_command(),
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
