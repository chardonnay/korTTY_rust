//! One-shot direct submission of an explicitly selected terminal text action.

use super::*;

impl Workspace {
    pub(super) fn continue_direct_terminal_request(&mut self, id: window::Id) -> Task<Desktop> {
        let config = self.ai_chat_configuration();
        let source_current = self
            .terminal_chats
            .get(&id)
            .is_some_and(|chat| self.terminal_chat_source(chat).is_some());
        let Some(chat) = self.terminal_chats.get_mut(&id) else {
            return Task::none();
        };
        let Some(expected) = &chat.direct_initial else {
            return Task::none();
        };
        if !source_current
            || config.as_ref().is_none_or(|current| {
                current.confirm_before_send() || !Arc::ptr_eq(current, expected)
            })
            || chat.action == Action::Ask
            || !chat.turns.is_empty()
            || chat.pending.is_some()
        {
            chat.direct_initial = None;
            return Task::none();
        }
        let Some(review) = &chat.review else {
            chat.direct_initial = None;
            return Task::none();
        };
        if review.memory.loading() || review.quota.loading() {
            return Task::none();
        }
        let extra_review = review.transport.is_cli()
            || review.attachment.is_some()
            || !matches!(review.memory, Memory::Omitted)
            || review.knowledge.is_some()
            || review.web_scope.is_some()
            || !review.web_sources.is_empty();
        chat.direct_initial = None;
        if extra_review {
            chat.notice = "Review this request before sending: it includes a file, memory, web context or a configured CLI.".into();
            return Task::none();
        }
        // Reuse the ordinary sender, including transport/policy, quota, redaction
        // and current-profile checks. This capability cannot authorize a later turn.
        self.update_terminal_ai_notified(id, Message::Send)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_ai::test_support::{
        accept, exclusive_run, fixture_response, listener, request,
    };
    use crate::session::{GridSize, Session};
    use iced::futures::{StreamExt, executor::block_on};
    use std::io::{ErrorKind, Write};

    fn fixture(endpoint: &str, confirm: bool) -> (Workspace, window::Id, SessionId) {
        let config: Configuration = serde_json::from_value(serde_json::json!({
            "general": {"confirm_before_send":confirm}, "memory":{"enabled":false},
            "profiles":{"profiles":[{"id":7,"name":"Fixture","endpoint":endpoint,"model":"test"}],"default":7,"last_id":7}
        })).unwrap();
        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(config);
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(crate::session::IbmKind::Tn3270e),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, session);
        (workspace, owner, session)
    }

    fn advance(workspace: &mut Workspace, task: Task<Desktop>) -> Task<Desktop> {
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            return Task::none();
        };
        while let Some(action) = block_on(stream.next()) {
            if let iced_runtime::Action::Output(message) = action {
                let streaming = matches!(message, Desktop::TerminalAi(_, Message::Streaming(..)));
                let next = workspace.update(message);
                if !streaming {
                    return next;
                }
            }
        }
        Task::none()
    }

    #[test]
    fn direct_terminal_actions_send_once_through_the_masked_sender() {
        let _guard = exclusive_run();
        for action in [
            Action::Summarize,
            Action::SummarizeRecent,
            Action::SolveProblem,
        ] {
            let (server, _) = listener();
            let (mut workspace, owner, session) =
                fixture(&server.local_addr().unwrap().to_string(), false);
            let open = workspace.install_terminal_chat(
                owner,
                session,
                action,
                "Terminal output\npassword=direct-fixture-secret",
                false,
            );
            let id = workspace
                .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
                .unwrap();
            assert!(workspace.terminal_chats[&id].pending.is_none());
            let send = advance(&mut workspace, open);
            assert!(workspace.terminal_chats[&id].direct_initial.is_none());
            assert!(workspace.terminal_chats[&id].pending.is_some());
            let mut stream = accept(&server);
            let payload = request(&mut stream);
            assert!(!payload.to_string().contains("direct-fixture-secret"));
            assert!(payload.to_string().contains("password=***"));
            assert!(payload.to_string().contains(action.question()));
            stream
                .get_mut()
                .write_all(fixture_response("200 OK", "", "Fixture answer").as_bytes())
                .unwrap();
            drop(stream);
            let _ = advance(&mut workspace, send);
            assert_eq!(workspace.terminal_chats[&id].turns.len(), 1);
            assert!(
                iced_runtime::task::into_stream(workspace.continue_direct_terminal_request(id))
                    .is_none()
            );
            assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn ask_and_default_confirmation_never_arm_a_direct_request() {
        for (action, confirm) in [(Action::Ask, false), (Action::Summarize, true)] {
            let (server, _) = listener();
            let (mut workspace, owner, session) =
                fixture(&server.local_addr().unwrap().to_string(), confirm);
            let _ =
                workspace.install_terminal_chat(owner, session, action, "Terminal output", false);
            let id = workspace
                .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
                .unwrap();
            let chat = &workspace.terminal_chats[&id];
            assert!(
                chat.direct_initial.is_none() && chat.review.is_none() && chat.pending.is_none()
            );
            assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn terminal_ai_menu_entries_follow_the_focused_selection_and_policy() {
        use crate::workspace::chrome::{Action as Menu, Message as MenuMessage};
        let (server, _) = listener();
        let (mut workspace, owner, session) =
            fixture(&server.local_addr().unwrap().to_string(), true);
        let mut terminal = kortty_terminal::Terminal::new(40, 2);
        terminal.advance(b"Fixture terminal selection");
        let pane = workspace.sessions.get_mut(&session).unwrap();
        pane.frame = crate::terminal_view::selection_test_frame(terminal.snapshot());
        assert!(workspace.chrome_enabled(owner, Menu::SummarizeRecentTerminalOutput));
        assert!(!workspace.chrome_enabled(owner, Menu::SummarizeTerminalSelection));
        workspace.sessions.get_mut(&session).unwrap().selection =
            Some(crate::terminal_view::Selection::from_cells((0, 0), (0, 25)));
        assert!(!workspace.chrome_enabled(owner, Menu::SummarizeRecentTerminalOutput));
        for action in [
            Menu::SummarizeTerminalSelection,
            Menu::SolveTerminalSelection,
            Menu::AskAboutTerminalSelection,
        ] {
            assert!(workspace.chrome_enabled(owner, action));
        }
        let _ =
            workspace.update_chrome(MenuMessage::Invoke(owner, Menu::AskAboutTerminalSelection));
        let id = workspace
            .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
            .unwrap();
        assert_eq!(workspace.terminal_chats[&id].action, Action::Ask);
        assert!(
            workspace.terminal_chats[&id]
                .context
                .text()
                .contains("Fixture terminal selection")
        );
        assert!(workspace.terminal_chats[&id].pending.is_none());
        workspace.windows.get_mut(&owner).unwrap().active = 0;
        workspace.clipboard.policy.ai_chat_allowed = false;
        assert!(!workspace.chrome_enabled(owner, Menu::AskAboutTerminalSelection));
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn direct_request_is_revoked_by_user_input_changed_settings_or_policy() {
        for change in 0..3 {
            let (server, _) = listener();
            let (mut workspace, owner, session) =
                fixture(&server.local_addr().unwrap().to_string(), false);
            let open = workspace.install_terminal_chat(
                owner,
                session,
                Action::Summarize,
                "Terminal output",
                false,
            );
            let id = workspace
                .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
                .unwrap();
            match change {
                0 => {
                    let _ = workspace.update_terminal_ai(id, Message::Profile(7));
                }
                1 => {
                    let mut changed =
                        serde_json::to_value(workspace.ai_chat_configuration().unwrap().as_ref())
                            .unwrap();
                    changed["general"]["confirm_before_send"] = true.into();
                    workspace.set_test_ai_configuration(serde_json::from_value(changed).unwrap());
                }
                _ => workspace.clipboard.policy.ai_allowed = false,
            }
            let _ = advance(&mut workspace, open);
            assert!(workspace.terminal_chats[&id].pending.is_none());
            assert!(workspace.terminal_chats[&id].direct_initial.is_none());
            assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
        }
    }

    #[test]
    fn attached_content_still_requires_a_visible_review() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("attachment.txt");
        std::fs::write(&path, "Additional private file").unwrap();
        let (server, _) = listener();
        let (mut workspace, owner, session) =
            fixture(&server.local_addr().unwrap().to_string(), false);
        let _ = workspace.install_terminal_chat(
            owner,
            session,
            Action::Summarize,
            "Terminal output",
            false,
        );
        let id = workspace
            .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
            .unwrap();
        let config = workspace.ai_chat_configuration().unwrap();
        let chat = workspace.terminal_chats.get_mut(&id).unwrap();
        let _ = chat
            .start_review(id, 1, config, &Default::default())
            .unwrap();
        chat.review.as_mut().unwrap().attachment = Some(Arc::new(
            attachment::Snapshot::read(&path, &chat.redactor).unwrap(),
        ));
        let _ = workspace.continue_direct_terminal_request(id);
        let chat = &workspace.terminal_chats[&id];
        assert!(chat.pending.is_none() && chat.review.is_some() && chat.direct_initial.is_none());
        assert!(chat.notice.contains("Review this request"));
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }

    #[test]
    fn direct_request_waits_for_memory_and_requires_review_of_extra_content() {
        let _guard = exclusive_run();
        for with_memory in [false, true] {
            let profile = crate::snippets::tests::Profile::new();
            if with_memory {
                let directory =
                    crate::private_files::Directory::open(&profile.0.join("memory"), true).unwrap();
                directory
                    .publish_copy("MEMORY.md", b"Prefer concise answers.\n")
                    .unwrap();
            }
            let (server, _) = listener();
            let (mut workspace, owner, session) =
                fixture(&server.local_addr().unwrap().to_string(), false);
            let mut config =
                serde_json::to_value(workspace.ai_chat_configuration().unwrap().as_ref()).unwrap();
            config["memory"]["enabled"] = true.into();
            workspace.set_test_ai_configuration(serde_json::from_value(config).unwrap());
            let open = workspace.install_terminal_chat(
                owner,
                session,
                Action::Summarize,
                "Terminal output",
                false,
            );
            let id = workspace
                .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
                .unwrap();
            workspace.terminal_chats.get_mut(&id).unwrap().storage = Some(profile.0.clone());
            let preparation = advance(&mut workspace, open);
            assert!(
                workspace.terminal_chats[&id]
                    .review
                    .as_ref()
                    .unwrap()
                    .memory
                    .loading()
            );
            assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
            let send = advance(&mut workspace, preparation);
            let chat = &workspace.terminal_chats[&id];
            assert!(chat.direct_initial.is_none());
            if with_memory {
                assert!(chat.pending.is_none());
                assert!(matches!(
                    chat.review.as_ref().unwrap().memory,
                    Memory::Included { .. }
                ));
                assert!(chat.notice.contains("Review this request"));
                assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
            } else {
                assert!(chat.pending.is_some());
                let mut stream = accept(&server);
                let _ = request(&mut stream);
                stream
                    .get_mut()
                    .write_all(fixture_response("200 OK", "", "Fixture answer").as_bytes())
                    .unwrap();
                drop(stream);
                let _ = advance(&mut workspace, send);
                assert_eq!(workspace.terminal_chats[&id].turns.len(), 1);
            }
        }
    }

    #[test]
    fn cancelling_preparation_prevents_late_direct_send() {
        let profile = crate::snippets::tests::Profile::new();
        let (server, _) = listener();
        let (mut workspace, owner, session) =
            fixture(&server.local_addr().unwrap().to_string(), false);
        let mut config =
            serde_json::to_value(workspace.ai_chat_configuration().unwrap().as_ref()).unwrap();
        config["memory"]["enabled"] = true.into();
        workspace.set_test_ai_configuration(serde_json::from_value(config).unwrap());
        let open = workspace.install_terminal_chat(
            owner,
            session,
            Action::Summarize,
            "Terminal output",
            false,
        );
        let id = workspace
            .hosted_tool_in(owner, tool_tabs::Kind::AiChat)
            .unwrap();
        workspace.terminal_chats.get_mut(&id).unwrap().storage = Some(profile.0.clone());
        let preparation = advance(&mut workspace, open);
        let _ = workspace.update_terminal_ai(id, Message::Cancel);
        let _ = advance(&mut workspace, preparation);
        let chat = &workspace.terminal_chats[&id];
        assert!(chat.direct_initial.is_none() && chat.review.is_none() && chat.pending.is_none());
        assert_eq!(server.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    }
}
