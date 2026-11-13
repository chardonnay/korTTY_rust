//! A source-bound review and a single AI request for an attachment flowchart.

use std::sync::Arc;

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Font, Length, Size, Task, window};

use crate::ai_manager::{Configuration, ProfileId};
use crate::diagram_window::{AnalysisFlowchartRequest, Chart, DiagramWindow};
use crate::local_ai::{self, Profile};
use crate::workspace::ai_quota;
use crate::workspace::dialog_placement::{Kind, escape::on_escape};

use super::super::{
    Chat, Desktop, Message as ChatMessage, Workspace, conversation, mask, reviewed_transport,
};
use super::{Snapshot, remote};

#[derive(Clone)]
pub enum Message {
    Review,
    QuotaFinished(u64, String),
    Send(u64),
    Cancel,
    Finished(u64, Result<Chart, String>),
}

#[derive(Default)]
pub(in crate::workspace::terminal_ai) struct State(Option<Phase>);

enum Phase {
    Review(Box<Review>),
    Running(Run),
}

struct Scope {
    ticket: u64,
    profile: ProfileId,
    configuration: Arc<Configuration>,
    source: Arc<Snapshot>,
    masked: Arc<str>,
}

struct Review {
    scope: Scope,
    destination: String,
    transport: Profile,
    request: AnalysisFlowchartRequest,
    quota: ai_quota::State,
}

struct Run {
    scope: Scope,
    _cancel: local_ai::Cancel,
    _source_cancel: Option<remote::Cancel>,
}

impl Scope {
    fn matches(&self, chat: &Chat, configuration: Option<&Arc<Configuration>>) -> bool {
        configuration.is_some_and(|current| Arc::ptr_eq(&self.configuration, current))
            && self.profile == chat.profile
            && chat
                .conversation
                .attachment()
                .is_some_and(|current| Arc::ptr_eq(&self.source, &current))
    }
}

impl Review {
    fn forecast(&mut self, id: window::Id) -> Task<Desktop> {
        if !self.transport.has_quota() {
            return Task::none();
        }
        let ticket = self.scope.ticket;
        self.quota.start_messages(
            ticket,
            &self.transport,
            [self.request.system(), self.request.input()],
            move |forecast| Desktop::TerminalAi(id, wrap(Message::QuotaFinished(ticket, forecast))),
        )
    }
}

impl State {
    pub(in crate::workspace::terminal_ai) fn active(&self) -> bool {
        self.0.is_some()
    }

    pub(in crate::workspace::terminal_ai) fn invalidate(
        chat: &mut Chat,
        configuration: Option<&Arc<Configuration>>,
        allowed: bool,
    ) {
        if chat
            .conversation
            .flowchart
            .scope()
            .is_some_and(|scope| !allowed || !scope.matches(chat, configuration))
        {
            chat.conversation.flowchart = Self::default();
            chat.notice =
                "The attachment or AI settings changed. Review the flowchart request again.".into();
        }
    }

    pub(in crate::workspace::terminal_ai) fn invalidate_source(
        &mut self,
        access: Option<&remote::Access>,
    ) -> bool {
        if self
            .scope()
            .is_some_and(|scope| !scope.source.matches_source(access))
        {
            self.0 = None;
            return true;
        }
        false
    }

    fn scope(&self) -> Option<&Scope> {
        match self.0.as_ref()? {
            Phase::Review(review) => Some(&review.scope),
            Phase::Running(run) => Some(&run.scope),
        }
    }

    pub(in crate::workspace::terminal_ai) fn view(
        &self,
        allowed: bool,
    ) -> Option<Element<'_, ChatMessage>> {
        let view: Element<_> = match self.0.as_ref()? {
            Phase::Review(review) => column![
                text("Review attachment flowchart").size(19),
                text(&review.destination).size(14),
                text(review.scope.source.label()).size(13),
                review.quota.label().map(|label| text(label).size(13)),
                text("Only the masked attachment shown below is sent with this request.").size(13),
                scrollable(
                    column![
                        text("System instructions").size(14),
                        text(review.request.system())
                            .font(Font::MONOSPACE)
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                        text("Request body").size(14),
                        text(review.request.input())
                            .font(Font::MONOSPACE)
                            .size(13)
                            .wrapping(text::Wrapping::WordOrGlyph),
                    ]
                    .spacing(8)
                )
                .height(Length::Fill),
                row![
                    button("Back").on_press(wrap(Message::Cancel)),
                    button("Generate reviewed flowchart").on_press_maybe(
                        (allowed && !review.quota.loading())
                            .then_some(wrap(Message::Send(review.scope.ticket)))
                    ),
                ]
                .spacing(8),
            ]
            .spacing(8)
            .padding(14)
            .height(Length::Fill)
            .into(),
            Phase::Running(_) => column![
                text("Generating attachment flowchart…").size(19),
                button("Cancel request").on_press(wrap(Message::Cancel)),
            ]
            .spacing(8)
            .padding(14)
            .into(),
        };
        Some(on_escape(view, wrap(Message::Cancel)))
    }
}

impl Workspace {
    pub(in crate::workspace::terminal_ai) fn update_chat_attachment_flowchart(
        &mut self,
        id: window::Id,
        event: Message,
    ) -> Task<Desktop> {
        let configuration = self.ai_chat_configuration();
        let allowed =
            self.ai_chat_enabled() && self.clipboard.policy.ai_memory.external_files_allowed();
        let next_ticket =
            matches!(event, Message::Review | Message::Send(_)).then(|| self.allocate());
        let access = self.terminal_chats.get(&id).and_then(|chat| {
            chat.conversation.remote_attachment()?;
            self.remote_attachment_access(chat)
        });
        let Some(chat) = self.terminal_chats.get_mut(&id) else {
            return Task::none();
        };
        State::invalidate(chat, configuration.as_ref(), allowed);
        if chat
            .conversation
            .flowchart
            .invalidate_source(access.as_ref())
        {
            chat.notice =
                "The original SSH attachment source is unavailable. Review a new snapshot.".into();
            return Task::none();
        }
        match event {
            Message::Review
                if allowed && chat.code_actions_available() && chat.code_review.is_none() =>
            {
                let Some((configuration, source)) =
                    configuration.zip(chat.conversation.attachment())
                else {
                    return Task::none();
                };
                if !source.matches_source(access.as_ref()) {
                    chat.notice =
                        "The original SSH attachment source is unavailable. Review a new snapshot."
                            .into();
                    return Task::none();
                }
                let result = prepare(
                    chat,
                    next_ticket.expect("flowchart review ticket"),
                    configuration,
                    source,
                );
                match result {
                    Ok(mut review) => {
                        chat.automatic.cancel_pending();
                        chat.notice.clear();
                        let forecast = review.forecast(id);
                        chat.conversation.flowchart.0 = Some(Phase::Review(Box::new(review)));
                        return forecast;
                    }
                    Err(error) => chat.notice = error,
                }
            }
            Message::QuotaFinished(ticket, forecast) => {
                if let Some(Phase::Review(review)) = &mut chat.conversation.flowchart.0 {
                    review.quota.complete(ticket, forecast);
                }
            }
            Message::Send(ticket) if matches!(&chat.conversation.flowchart.0, Some(Phase::Review(review)) if review.scope.ticket == ticket) =>
            {
                let Some(Phase::Review(mut review)) = chat.conversation.flowchart.0.take() else {
                    return Task::none();
                };
                if review.quota.loading() {
                    chat.conversation.flowchart.0 = Some(Phase::Review(review));
                    return Task::none();
                }
                if review.quota.stale() {
                    review.scope.ticket = next_ticket.expect("flowchart refresh ticket");
                    let forecast = review.forecast(id);
                    chat.conversation.flowchart.0 = Some(Phase::Review(review));
                    chat.notice = "Review the refreshed quota forecast before generating.".into();
                    return forecast;
                }
                let Review {
                    scope,
                    request,
                    transport,
                    ..
                } = *review;
                let (source, source_cancel) =
                    match Arc::clone(&scope.source).prepare_verification(access) {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            chat.notice = error;
                            return Task::none();
                        }
                    };
                let redactor = Arc::clone(&chat.redactor);
                let pending = local_ai::start(transport, move |client| {
                    source.require_current()?;
                    source.require_active()?;
                    let chart = request
                        .complete_reviewed(client, |mermaid| mask::prompt(&redactor, mermaid))?;
                    source.require_active()?;
                    Ok(chart)
                });
                match pending {
                    Ok(local_ai::Pending { reply, cancel }) => {
                        chat.conversation.flowchart.0 = Some(Phase::Running(Run {
                            scope,
                            _cancel: cancel,
                            _source_cancel: source_cancel,
                        }));
                        return Task::perform(
                            async move {
                                reply
                                    .recv()
                                    .await
                                    .unwrap_or_else(|_| Err("The flowchart worker stopped.".into()))
                            },
                            move |result| {
                                Desktop::TerminalAi(id, wrap(Message::Finished(ticket, result)))
                            },
                        );
                    }
                    Err(error) => chat.notice = error,
                }
            }
            Message::Cancel => {
                chat.conversation.flowchart = State::default();
                chat.notice = "Attachment flowchart canceled.".into();
            }
            Message::Finished(ticket, result) if matches!(&chat.conversation.flowchart.0, Some(Phase::Running(run)) if run.scope.ticket == ticket) =>
            {
                let Some(Phase::Running(run)) = chat.conversation.flowchart.0.take() else {
                    return Task::none();
                };
                match result {
                    Ok(chart) => {
                        chat.notice = "Attachment flowchart opened.".into();
                        return self.open_attachment_flowchart(&run.scope.masked, chart);
                    }
                    Err(error) => chat.notice = error,
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn open_attachment_flowchart(&mut self, source: &str, chart: Chart) -> Task<Desktop> {
        let size = Size::new(1050.0, 660.0);
        let (id, opened) = self.open_placed_dialog(
            Kind::Diagram,
            window::Settings {
                size,
                min_size: Some(Size::new(600.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.diagrams.insert(
            id,
            DiagramWindow::with_attachment_chart(size, source, chart),
        );
        opened.map(|_| Desktop::Noop)
    }
}

fn prepare(
    chat: &Chat,
    ticket: u64,
    configuration: Arc<Configuration>,
    source: Arc<Snapshot>,
) -> Result<Review, String> {
    let (transport, limit) = reviewed_transport(&configuration, chat.profile, chat.private_source)?;
    if transport.is_remote() || transport.is_cli() {
        return Err("Choose a local or managed AI profile for attachment flowcharts.".into());
    }
    let (_, name, model, destination) = configuration
        .terminal_chat_profiles()
        .find(|(id, ..)| *id == chat.profile)
        .ok_or("The selected AI profile is unavailable.")?;
    let destination = format!("{name} · {model} · {destination}");
    let masked: Arc<str> = Arc::from(mask::prompt(&chat.redactor, source.content())?);
    if masked.len() > super::MAX_BYTES || masked.chars().count() > limit {
        return Err("The masked attachment exceeds the profile's selection limit.".into());
    }
    let request =
        AnalysisFlowchartRequest::new(Arc::clone(&masked), false)?.with_profile(&transport);
    Ok(Review {
        scope: Scope {
            ticket,
            profile: chat.profile,
            configuration,
            source,
            masked,
        },
        destination,
        transport,
        request,
        quota: ai_quota::State::default(),
    })
}

fn wrap(event: Message) -> ChatMessage {
    ChatMessage::Conversation(conversation::Message::Flowchart(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_response, listener, request,
    };
    use crate::terminal_logging::{Redactor, Secrets};
    use iced::futures::StreamExt as _;
    use std::io::{self, Write as _};
    use std::path::Path;

    fn deliver(workspace: &mut Workspace, task: Task<Desktop>) {
        let mut stream = iced_runtime::task::into_stream(task).expect("scheduled attachment task");
        let message = iced::futures::executor::block_on(async {
            while let Some(action) = stream.next().await {
                if let iced_runtime::Action::Output(message) = action {
                    return message;
                }
            }
            panic!("missing attachment task result")
        });
        let _ = workspace.update(message);
    }

    fn load(workspace: &mut Workspace, id: window::Id, path: &Path) {
        for message in [
            conversation::Message::Attach,
            conversation::Message::AttachmentPath(path.to_string_lossy().into_owned()),
        ] {
            let _ = workspace.update_terminal_ai(id, ChatMessage::Conversation(message));
        }
        let task = workspace.update_terminal_ai(
            id,
            ChatMessage::Conversation(conversation::Message::LoadAttachment),
        );
        deliver(workspace, task);
    }

    #[test]
    fn attachment_flowchart_quota_forecasts_exact_messages_and_renews_approval() {
        let _guard = exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let path = root.join("attachment.sh");
        std::fs::write(&path, "printf 'fixture'\n").unwrap();
        let (server, _) = listener();
        let budget = local_ai::UsageBudget {
            token_limit: Some(10_000),
            ..Default::default()
        };
        let account = local_ai::UsageAccount::new(root.clone(), 7, budget.clone()).unwrap();
        let configuration: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": 7, "name": "Flowchart fixture",
                    "endpoint": server.local_addr().unwrap().to_string(), "model": "test",
                    "max_output_tokens": 512, "usage_budget": budget}],
                "default": 7, "last_id": 7
            }
        }))
        .unwrap();
        configuration
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        let configuration = Configuration::load_fixture_from(&root).unwrap();
        let chat = Chat::new(
            &configuration,
            super::super::super::Action::Ask,
            "Separate terminal context",
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default()),
            None,
        )
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(configuration);
        let id = window::Id::unique();
        workspace.terminal_chats.insert(id, chat);
        load(&mut workspace, id, &path);

        fn review(workspace: &mut Workspace, id: window::Id) -> &mut Review {
            let Some(Phase::Review(review)) = &mut workspace
                .terminal_chats
                .get_mut(&id)
                .unwrap()
                .conversation
                .flowchart
                .0
            else {
                panic!("expected a flowchart review");
            };
            review
        }

        let dispatch =
            |workspace: &mut Workspace, event| workspace.update_terminal_ai(id, wrap(event));
        let forecast = dispatch(&mut workspace, Message::Review);
        let old_ticket = review(&mut workspace, id).scope.ticket;
        assert!(review(&mut workspace, id).quota.loading());
        assert_eq!(
            dispatch(&mut workspace, Message::Send(old_ticket)).units(),
            0
        );
        server.set_nonblocking(true).unwrap();
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        deliver(&mut workspace, forecast);

        let current = review(&mut workspace, id);
        let system = current.request.system().to_owned();
        let input = current.request.input().to_owned();
        let estimate = ai_quota::estimate_input([system.as_str(), input.as_str()]);
        let label = current.quota.label().unwrap();
        assert!(label.contains("0 of 10000 tokens reported"));
        assert!(label.contains(&format!(
            "about {estimate} input tokens and up to 512 output tokens"
        )));
        current.quota.expire();
        let refreshed = dispatch(&mut workspace, Message::Send(old_ticket));
        let new_ticket = review(&mut workspace, id).scope.ticket;
        assert_ne!(old_ticket, new_ticket);
        let _ = dispatch(
            &mut workspace,
            Message::QuotaFinished(old_ticket, "Obsolete".into()),
        );
        assert!(review(&mut workspace, id).quota.loading());
        deliver(&mut workspace, refreshed);
        assert_eq!(
            dispatch(&mut workspace, Message::Send(old_ticket)).units(),
            0
        );
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let usage = account.snapshot().unwrap();
        assert_eq!((usage.counts.requests, usage.in_flight), (0, 0));

        let worker = std::thread::spawn(move || {
            let mut socket = accept(&server);
            let body = request(&mut socket);
            assert_eq!(body["max_tokens"], 512);
            assert_eq!(body["messages"][0]["content"], system);
            assert_eq!(body["messages"][1]["content"], input);
            let diagram = serde_json::json!({
                "mermaid": "flowchart TD\nA[\"Start\"] --> B[\"Done\"]"
            });
            socket
                .get_mut()
                .write_all(fixture_response("200 OK", "", &diagram.to_string()).as_bytes())
                .unwrap();
        });
        let completion = dispatch(&mut workspace, Message::Send(new_ticket));
        deliver(&mut workspace, completion);
        worker.join().unwrap();
        assert_eq!(workspace.diagrams.len(), 1);
        assert_eq!(account.snapshot().unwrap().counts.requests, 1);
    }

    #[test]
    fn attachment_flowchart_requires_review_rechecks_file_and_sends_exact_masked_request() {
        let _guard = exclusive_run();
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("attachment.sh");
        let source = "API_KEY=fixture-private-key\nprintf 'evidence'\n";
        std::fs::write(&path, source).unwrap();
        let (server, _) = listener();
        let config: Configuration = serde_json::from_value(serde_json::json!({
            "profiles": {
                "profiles": [{"id": 7, "name": "Fixture", "endpoint": server.local_addr().unwrap().to_string(), "model": "test"}],
                "default": 7, "last_id": 7
            }
        })).unwrap();
        let secrets = Secrets::default();
        secrets.add("fixture-private-key");
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Default::default());
        let chat = Chat::new(
            &config,
            super::super::super::Action::Ask,
            "Separate terminal context",
            redactor,
            None,
        )
        .unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let id = window::Id::unique();
        workspace.terminal_chats.insert(id, chat);
        load(&mut workspace, id, &path);

        let review = |workspace: &mut Workspace| {
            let task = workspace.update_terminal_ai(id, wrap(Message::Review));
            assert!(iced_runtime::task::into_stream(task).is_none());
            let Some(Phase::Review(review)) =
                &workspace.terminal_chats[&id].conversation.flowchart.0
            else {
                panic!("expected a review before any request");
            };
            assert!(review.request.input().contains("API_KEY=***"));
            assert!(!review.request.input().contains("fixture-private-key"));
            assert!(!review.request.input().contains("Separate terminal context"));
            (
                review.scope.ticket,
                review.request.system().to_owned(),
                review.request.input().to_owned(),
            )
        };
        let (ticket, _, _) = review(&mut workspace);
        server.set_nonblocking(true).unwrap();
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        std::fs::write(&path, "changed after review").unwrap();
        let task = workspace.update_terminal_ai(id, wrap(Message::Send(ticket)));
        deliver(&mut workspace, task);
        assert!(
            workspace.terminal_chats[&id]
                .notice
                .contains("changed after review")
        );
        assert_eq!(
            server.accept().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        std::fs::write(&path, source).unwrap();
        let _ = workspace.update_terminal_ai(
            id,
            ChatMessage::Conversation(conversation::Message::RemoveAttachment),
        );
        load(&mut workspace, id, &path);
        let (ticket, system, input) = review(&mut workspace);
        let worker = std::thread::spawn(move || {
            let mut stream = accept(&server);
            let body = request(&mut stream);
            assert_eq!(body["messages"][0]["content"], system);
            assert_eq!(body["messages"][1]["content"], input);
            let diagram = serde_json::json!({
                "mermaid": "flowchart TD\nA([\"Start\"]) --> B[\"fixture-private-key\"]\nB --> C([\"Done\"])"
            });
            stream
                .get_mut()
                .write_all(fixture_response("200 OK", "", &diagram.to_string()).as_bytes())
                .unwrap();
        });
        let task = workspace.update_terminal_ai(id, wrap(Message::Send(ticket)));
        deliver(&mut workspace, task);
        worker.join().unwrap();
        assert_eq!(workspace.diagrams.len(), 1);
        let diagram = workspace.diagrams.values().next().unwrap();
        assert!(diagram.source().contains("API_KEY=***"));
        let mermaid = diagram.mermaid().unwrap();
        assert!(!mermaid.contains("fixture-private-key"));
        // Export assigns its own node IDs. Check the displayed labels and topology.
        let rendered = kortty_diagram::parse_mermaid(&mermaid).unwrap();
        assert_eq!((rendered.nodes().len(), rendered.edges().len()), (3, 2));
        for label in ["Start", "***", "Done"] {
            assert!(rendered.nodes().iter().any(|node| node.label() == label));
        }
        assert!(workspace.terminal_chats[&id].turns.is_empty());
    }
}
