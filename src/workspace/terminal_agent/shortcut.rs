//! Local input capture at a marked, empty terminal prompt.

use iced::keyboard::{Key, Location, Modifiers, key::Named};
use iced::widget::{column, row, text};
use iced::{Element, Task, window};
use kortty_terminal::{TerminalProfile, TextEncoding};

use crate::clipboard_policy::ClipboardPolicy;
use crate::input::encode_stream_key;
use crate::session::{SessionId, ShortcutPrompt};
use crate::terminal_agent::shortcut::{Aliases, Input, Parsed, PendingPrefix, shortcut_character};
use crate::workspace::multi_exec_access::KeyboardRoute;

use super::{Desktop, PaneSession, Workspace};

mod completion;
use completion::Completion;
pub use completion::Message;

pub(in crate::workspace) struct Capture {
    aliases: Aliases,
    prompt: ShortcutPrompt,
    line: PendingPrefix,
    completion: Option<Completion>,
}

impl PaneSession {
    fn agent_shortcut_prompt(&self) -> Option<ShortcutPrompt> {
        let frame = self.session.frame();
        let prompt = self.session.agent_shortcut_prompt(&frame)?;
        if frame.screen.profile != TerminalProfile::Xterm
            || frame.screen.text_encoding() != TextEncoding::Utf8
            || !frame.at_live_viewport()
            || frame.password_prompt
            || frame.coding_agent_visible()
        {
            return None;
        }
        Some(prompt)
    }

    pub(in crate::workspace) fn agent_shortcut_preview(
        &self,
        owner: window::Id,
        private: bool,
    ) -> Option<Element<'_, Desktop>> {
        let capture = self.agent_shortcut.as_ref()?;
        if self.agent_shortcut_prompt()? != capture.prompt {
            return None;
        }
        let draft = capture.line.text();
        let end = draft.floor_char_boundary(draft.len().min(160));
        let preview = if end == draft.len() {
            draft.to_owned()
        } else {
            format!("{}…", &draft[..end])
        };
        let label = if capture.line.composing() {
            "Agent shortcut · Enter to submit · Esc to cancel"
        } else {
            "Input"
        };
        Some(
            column![
                capture
                    .completion
                    .as_ref()
                    .and_then(|state| state.view(owner, private)),
                row![text(label).size(12), text(preview).size(13)]
                    .spacing(8)
                    .wrap(),
            ]
            .spacing(4)
            .into(),
        )
    }
}

impl Workspace {
    pub(in crate::workspace) fn agent_shortcut_status(&self, session: SessionId) -> &'static str {
        let Some((_, pane, tab)) = self.agent_pane(session) else {
            return "This terminal is no longer available.";
        };
        if !self.clipboard.policy.ai_agent_allowed() {
            return "Managed policy blocks terminal Agent shortcuts.";
        }
        let Some(configuration) = self.ai_configuration() else {
            return "Enable AI features in AI Manager to use terminal Agent shortcuts.";
        };
        if configuration.agent_shortcut_aliases().is_none() {
            return "Enable terminal Agent shortcuts in AI Manager → General.";
        }
        if !self.window_settings.prompt_navigation_enabled {
            return "Enable shell command marks in Settings → Terminal, then reconnect or restart this terminal.";
        }
        if self.terminal_agents.owns_session(session) {
            return "An Agent run owns this terminal. Wait for it to finish or stop the run.";
        }
        let frame = pane.session.frame();
        if frame.status != crate::session::Status::Running {
            return "Connect or restart this terminal before entering an Agent shortcut.";
        }
        if pane.ssh.is_some() && pane.session.journal_redaction_secrets().is_none() {
            return "Reconnect this SSH terminal to initialize credential masking for AI.";
        }
        if frame.screen.profile != TerminalProfile::Xterm
            || frame.screen.text_encoding() != TextEncoding::Utf8
        {
            return "Agent shortcuts require an Xterm terminal with UTF-8 encoding.";
        }
        if frame.screen.alternate_screen || frame.password_prompt || frame.coding_agent_visible() {
            return "Return to the ordinary shell prompt before entering an Agent shortcut.";
        }
        if !frame.at_live_viewport() {
            return "Scroll to the live terminal prompt before entering an Agent shortcut.";
        }
        if self.tab_broadcast_active(tab) || (!tab.incognito && self.multi_exec_member(session)) {
            return "Turn off broadcast and Multi-exec for this terminal to use Agent shortcuts.";
        }
        if self.agent_shortcut_scope(session).is_some() {
            "Ready to recognize an Agent shortcut at this empty shell prompt."
        } else {
            "An empty shell prompt is not verified. Set up shell integration, then return to a fresh prompt."
        }
    }

    pub(in crate::workspace) fn agent_shortcut_key(
        &mut self,
        owner: window::Id,
        route: &KeyboardRoute,
        key: &Key,
        location: Location,
        modifiers: Modifiers,
        input: Option<&str>,
    ) -> Option<Task<Desktop>> {
        let session = route.source;
        let cancel = (key == &Key::Named(Named::Escape) && modifiers.is_empty())
            || (modifiers == Modifiers::CTRL
                && matches!(key, Key::Character(key) if key.eq_ignore_ascii_case("c")));
        let mut capture = match self.capture_agent_shortcut(
            owner,
            session,
            route.predict && route.panes == [session],
        ) {
            Ok(capture) => capture?,
            Err(stale) => {
                // Never submit a changed prompt with the held line's Enter.
                // A fresh ordinary key or interrupt can still reach the shell.
                return (matches!(key, Key::Named(Named::Enter)) || (stale.composing && !cancel))
                    .then(Task::none);
            }
        };
        capture.refresh_completion_policy(self.clipboard.policy.ai_planning_allowed());
        if let Some((actual_owner, pane, tab)) = self.agent_pane(session)
            && actual_owner == owner
            && let Some(task) =
                capture.completion_key(owner, pane.stream_generation, tab.incognito, key, modifiers)
        {
            self.hold_agent_shortcut(session, capture);
            return Some(task);
        }
        if *key == Key::Named(Named::Tab)
            && modifiers.is_empty()
            && let Some(task) = self.begin_agent_completion(owner, session, &mut capture)
        {
            self.hold_agent_shortcut(session, capture);
            return Some(task);
        }
        let composing = capture.line.composing();
        if composing && cancel {
            return Some(Task::none());
        }
        let decision = match key {
            Key::Named(Named::Enter) if modifiers.is_empty() => {
                capture.line.submit(&capture.aliases)
            }
            Key::Named(Named::Backspace) if composing && modifiers.is_empty() => {
                capture.line.backspace();
                Input::Wait
            }
            _ => {
                let frame = self.sessions.get(&session)?.session.frame();
                let bytes = encode_stream_key(
                    &frame.screen,
                    key,
                    location,
                    modifiers,
                    input,
                    frame.application_cursor_keys,
                );
                let plain = bytes.as_deref().and_then(|bytes| {
                    std::str::from_utf8(bytes)
                        .ok()
                        .filter(|text| text.chars().all(shortcut_character))
                });
                match (plain, bytes.as_deref()) {
                    (Some(plain), Some(bytes)) if !plain.is_empty() => {
                        capture.line.push(&capture.aliases, plain, bytes)
                    }
                    _ if composing => Input::Wait,
                    _ => capture.line.replay_before(),
                }
            }
        };
        self.route_agent_shortcut(owner, session, capture, decision)
    }

    pub(in crate::workspace) fn agent_shortcut_paste(
        &mut self,
        owner: window::Id,
        session: SessionId,
        text: &str,
    ) -> bool {
        let single_pane = self
            .windows
            .get(&owner)
            .and_then(|window| window.tab())
            .and_then(|tab| self.keyboard_targets(tab))
            .is_some_and(|route| route.predict && route.panes == [session]);
        let mut capture = match self.capture_agent_shortcut(owner, session, single_pane) {
            Ok(Some(capture)) => capture,
            Ok(None) => return false,
            Err(stale) => return stale.composing,
        };
        capture.completion = None;
        let decision = capture.line.paste(&capture.aliases, text);
        self.route_agent_shortcut(owner, session, capture, decision)
            .is_some()
    }

    fn capture_agent_shortcut(
        &mut self,
        owner: window::Id,
        session: SessionId,
        single_pane: bool,
    ) -> Result<Option<Capture>, StaleCapture> {
        let pending = self
            .sessions
            .get_mut(&session)
            .and_then(|pane| pane.agent_shortcut.take());
        let ready = single_pane
            .then(|| self.agent_shortcut_scope(session))
            .flatten();
        match (pending, ready) {
            (Some(draft), Some((aliases, prompt)))
                if draft.prompt == prompt && draft.aliases == aliases =>
            {
                Ok(Some(draft))
            }
            (Some(draft), _) => {
                self.notice(
                    owner,
                    "The terminal prompt changed. Enter the shortcut again.".into(),
                );
                Err(StaleCapture {
                    composing: draft.line.composing(),
                })
            }
            (None, Some((aliases, prompt))) => Ok(Some(Capture {
                aliases,
                prompt,
                line: PendingPrefix::default(),
                completion: None,
            })),
            (None, None) => Ok(None),
        }
    }

    fn agent_shortcut_scope(&self, session: SessionId) -> Option<(Aliases, ShortcutPrompt)> {
        if !self.clipboard.policy.ai_agent_allowed() || self.terminal_agents.owns_session(session) {
            return None;
        }
        let aliases = self.ai_configuration()?.agent_shortcut_aliases()?;
        Some((
            aliases,
            self.sessions.get(&session)?.agent_shortcut_prompt()?,
        ))
    }

    fn hold_agent_shortcut(&mut self, session: SessionId, capture: Capture) {
        if let Some(pane) = self.sessions.get_mut(&session) {
            pane.agent_shortcut = Some(capture);
        }
    }

    fn route_agent_shortcut(
        &mut self,
        owner: window::Id,
        session: SessionId,
        capture: Capture,
        decision: Input,
    ) -> Option<Task<Desktop>> {
        match decision {
            Input::Forward => return None,
            Input::Wait | Input::Rejected(_) => {
                if let Input::Rejected(error) = decision {
                    self.notice(owner, error.into());
                }
                self.hold_agent_shortcut(session, capture);
            }
            Input::Replay {
                bytes,
                before_current,
            } => {
                if let Some(pane) = self.sessions.get_mut(&session)
                    && pane
                        .session
                        .replay_agent_shortcut(capture.prompt, bytes.to_vec())
                        .is_ok()
                {
                    pane.clear_selection();
                    pane.session.follow_output();
                    if before_current {
                        return None;
                    }
                } else {
                    self.notice(
                        owner,
                        "The buffered terminal input could not be sent. Try again.".into(),
                    );
                }
            }
            Input::Compose(line) => {
                if self
                    .sessions
                    .get(&session)
                    .and_then(PaneSession::agent_shortcut_prompt)
                    != Some(capture.prompt)
                {
                    self.notice(
                        owner,
                        "The terminal prompt changed. Enter the shortcut again.".into(),
                    );
                } else {
                    return Some(self.open_agent_shortcut(owner, session, &capture.aliases, &line));
                }
            }
        }
        Some(Task::none())
    }

    pub(super) fn open_agent_shortcut(
        &mut self,
        owner: window::Id,
        session: SessionId,
        aliases: &Aliases,
        line: &str,
    ) -> Task<Desktop> {
        let result = (|| {
            if !self.clipboard.policy.ai_agent_allowed()
                || !ClipboardPolicy::load().ai_agent_allowed()
            {
                return Err("Managed policy blocks terminal Agent shortcuts.".into());
            }
            let invocation = match aliases.parse(line) {
                Parsed::Ready(invocation) => invocation,
                Parsed::Rejected(error) => return Err(error.to_owned()),
                Parsed::Incomplete | Parsed::Ordinary => {
                    let [run, ask, plan] = aliases.commands();
                    return Err(format!(
                        "Enter {run} <goal>, {ask} <question>, or {plan} <task>."
                    ));
                }
            };
            let config = self
                .ai_configuration()
                .ok_or("AI is unavailable under the current settings.")?;
            let profile = if let Some(name) = invocation.profile {
                Some(
                    config
                        .agent_profiles()
                        .chain(config.reviewed_agent_cli_profiles())
                        .find_map(|(id, profile, _, _)| {
                            profile.eq_ignore_ascii_case(name).then_some(id)
                        })
                        .ok_or("The shortcut's AI agent profile is unavailable.")?,
                )
            } else {
                None
            };
            let direct = invocation.mode != super::Mode::Plan && !config.show_agent_setup();
            let mut run = self.build_agent(session, invocation.mode, None, profile, !direct)?;
            run.goal = invocation.goal.to_owned();
            let direct = direct && !run.profile_pending;
            Ok((run, direct))
        })();
        self.open_agent_result(owner, result)
    }
}

struct StaleCapture {
    composing: bool,
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::session::{Frame, GridSize, Session};
    use crate::session_security::LocalSettings;
    use crate::terminal_agent::Mode;
    use crate::workspace::terminal_agent::Phase;

    struct Fixture {
        workspace: Workspace,
        owner: window::Id,
        pane: SessionId,
    }

    impl Fixture {
        fn new() -> Self {
            Self::with_setup(true)
        }

        fn with_setup(show_setup: bool) -> Self {
            #[cfg(unix)]
            const SCRIPT: &str = r#"
prompt() { printf '\033]133;A\007shell> \033]133;B\007'; }
printf 'agent-plan from terminal output\n'
prompt
while IFS= read -r line; do
    printf '\033]133;C\007\nline=%s\n\033]133;D;0\007' "$line"
    prompt
done
"#;
            #[cfg(windows)]
            const SCRIPT: &str = r#"
function Write-Prompt {
    [Console]::Write([string][char]27 + ']133;A' + [char]7 + 'shell> ' + [char]27 + ']133;B' + [char]7)
}
[Console]::WriteLine('agent-plan from terminal output')
Write-Prompt
while ($null -ne ($line = [Console]::ReadLine())) {
    [Console]::Write([string][char]27 + ']133;C' + [char]7 + "`r`nline=$line`r`n" + [char]27 + ']133;D;0' + [char]7)
    Write-Prompt
}
"#;
            #[cfg(unix)]
            let command = vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()];
            #[cfg(windows)]
            let command = vec![
                std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
                    .join(r"System32\WindowsPowerShell\v1.0\powershell.exe")
                    .to_str()
                    .unwrap()
                    .into(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                SCRIPT.into(),
            ];
            Self::with_options(
                show_setup,
                kortty_pty::LaunchOptions {
                    command: Some(command),
                    ..Default::default()
                },
                "agent-plan from terminal output",
            )
        }

        fn with_options(
            show_setup: bool,
            options: kortty_pty::LaunchOptions,
            initial_output: &str,
        ) -> Self {
            let shell = options.shell;
            let mut workspace = Workspace::default();
            workspace.set_test_ai_configuration(
                serde_json::from_value(serde_json::json!({
                    "general": {"agent_shortcut": {"enabled": true}, "show_agent_setup": show_setup},
                    "profiles": {
                        "profiles": [{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"fixture"}],
                        "default":1, "last_id":1
                    }
                })).unwrap()
            );
            let (owner, _) = workspace.open_window(false);
            let grid = GridSize::default();
            let session =
                Session::start_local(grid, TerminalProfile::Xterm, options, true, None).unwrap();
            let pane = workspace.store_session(session, grid, 14);
            workspace.add_tab(owner, pane);
            workspace.sessions.get_mut(&pane).unwrap().local_security = Some(LocalSettings {
                shell,
                ..LocalSettings::default()
            });
            let mut fixture = Self {
                workspace,
                owner,
                pane,
            };
            fixture.wait_for_prompt(initial_output);
            fixture
        }

        fn session(&self) -> &Session {
            &self.workspace.sessions[&self.pane].session
        }

        fn wait_for_prompt(&mut self, output: &str) {
            let deadline = Instant::now() + Duration::from_secs(5);
            let frame: Arc<Frame> = loop {
                let session = self.session();
                let frame = session.frame();
                let visible = (0..frame.screen.rows).any(|index| {
                    frame.screen.row(index).is_some_and(|cells| {
                        cells
                            .iter()
                            .map(|cell| cell.text.as_str())
                            .collect::<String>()
                            .trim_end()
                            == output
                    })
                });
                if visible && session.agent_shortcut_prompt(&frame).is_some() {
                    break frame;
                }
                assert!(Instant::now() < deadline, "missing prompt after {output}");
                std::thread::sleep(Duration::from_millis(10));
            };
            self.workspace.sessions.get_mut(&self.pane).unwrap().frame = frame;
        }

        fn type_text(&mut self, text: &str) {
            for character in text.chars() {
                let text = character.to_string();
                let _ = self.workspace.key(
                    self.owner,
                    Key::Character(text.as_str().into()),
                    Location::Standard,
                    Modifiers::empty(),
                    Some(&text),
                );
            }
        }

        fn enter(&mut self) {
            let _ = self.press(Named::Enter);
        }

        fn press(&mut self, key: Named) -> Task<Desktop> {
            self.workspace.key(
                self.owner,
                Key::Named(key),
                Location::Standard,
                Modifiers::empty(),
                None,
            )
        }

        fn draft(&self) -> &Capture {
            self.workspace.sessions[&self.pane]
                .agent_shortcut
                .as_ref()
                .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.session().stop().recv_blocking();
        }
    }

    #[cfg(windows)]
    #[test]
    fn agent_shortcut_native_windows_shells_recognize_fresh_and_repeated_prompts() {
        for shell in [
            kortty_pty::LocalShell::PowerShell,
            kortty_pty::LocalShell::CommandPrompt,
        ] {
            let mut fixture = Fixture::with_options(
                true,
                kortty_pty::LaunchOptions {
                    shell,
                    shell_integration: true,
                    ..Default::default()
                },
                "",
            );
            fixture.type_text("echo kortty-native-forwarded");
            assert!(
                fixture
                    .session()
                    .agent_shortcut_prompt(&fixture.session().frame())
                    .is_none()
            );
            fixture.enter();
            fixture.wait_for_prompt("kortty-native-forwarded");
            if shell == kortty_pty::LocalShell::CommandPrompt {
                assert!(
                    fixture
                        .session()
                        .frame()
                        .shell_activity()
                        .command_exit
                        .is_none()
                );
            }
            fixture.type_text("agent-plan Inspect the native prompt");
            fixture.enter();
            let run = fixture
                .workspace
                .terminal_agents
                .runs
                .values()
                .next()
                .unwrap();
            assert_eq!(run.binding.session(), Some(fixture.pane));
            assert_eq!(run.mode, Mode::Plan);
            assert_eq!(run.goal, "Inspect the native prompt");
            assert!(run.phase == Phase::Setup);
            assert!(
                fixture
                    .session()
                    .agent_shortcut_prompt(&fixture.session().frame())
                    .is_some()
            );
        }
    }

    #[test]
    #[ignore = "requires explicit KORTTY_SSH_EXEC_ADDRESS, USER, PRIVATE_KEY and HOST_KEY fixtures"]
    fn live_ssh_agent_shortcut_uses_shipped_prompt_and_preserves_disabled_input() {
        use kortty_ssh::{Authentication, Connect, Endpoint, HostKey, SshSession, TrustStore};

        let get = |suffix| {
            std::env::var(format!("KORTTY_SSH_EXEC_{suffix}"))
                .expect("provide the explicit live SSH fixture")
        };
        let address: std::net::SocketAddr = get("ADDRESS").parse().unwrap();
        let endpoint = Endpoint::new(&address.ip().to_string(), address.port()).unwrap();
        let username = get("USER");
        let trust = TrustStore::from_pins(vec![(
            endpoint.clone(),
            HostKey::from_openssh(&get("HOST_KEY")).unwrap(),
        )])
        .unwrap();
        let grid = GridSize::default();
        let snippet = kortty_pty::ShellIntegrationSnippet::Bash.text();
        const DELIMITER: &str = "KORTTY_SHORTCUT_PRIVATE_RC_END";
        assert!(!snippet.lines().any(|line| line == DELIMITER));
        // The startup file exists only as a heredoc descriptor. This private
        // shell never modifies the account's startup files or shell history.
        let command = format!(
            "HISTFILE=/dev/null; stty -echo; exec env HISTFILE=/dev/null /bin/bash --noprofile --rcfile /dev/fd/3 -i 3<<'{DELIMITER}'\n\
             stty -echo\nPS1='kortty-ssh-shortcut> '\n\
             agent() {{ printf 'kortty-forwarded:%s\\n' \"$*\"; }}\n\
             {snippet}\n{DELIMITER}\n"
        );
        let connection = SshSession::connect(
            Connect {
                address,
                endpoint,
                username: username.clone(),
                authentication: Authentication::private_key(
                    std::fs::read(get("PRIVATE_KEY")).unwrap(),
                ),
                term: TerminalProfile::Xterm.term_name().into(),
                size: crate::session::ssh_size(grid),
            },
            trust,
        )
        .unwrap();
        let capture = crate::terminal_logging::Prepared::new(
            Default::default(),
            Default::default(),
            "Private SSH shortcut fixture".into(),
            address.ip().to_string(),
            username.clone(),
        );
        assert!(!capture.enabled());
        capture.secrets().add("ssh-shortcut-synthetic-secret");
        let session = Session::ssh_with_logging(
            Some(crate::ssh_connection::Connected::Direct(Box::new(
                connection,
            ))),
            grid,
            TerminalProfile::Xterm,
            true,
            None,
            Some(Box::new(capture)),
            crate::session::StreamOptions::default(),
        )
        .unwrap();
        let mut workspace = Workspace::default();
        let configure = |workspace: &mut Workspace, enabled| {
            workspace.set_test_ai_configuration(
                serde_json::from_value(serde_json::json!({
                    "general": {"agent_shortcut": {"enabled": enabled}},
                    "profiles": {
                        "profiles": [{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"fixture"}],
                        "default":1,"last_id":1
                    }
                }))
                .unwrap(),
            );
        };
        configure(&mut workspace, false);
        let (owner, _) = workspace.open_window(false);
        let pane = workspace.store_session(session, grid, 14);
        workspace.sessions.get_mut(&pane).unwrap().ssh = Some(
            crate::workspace::ssh_access::SshTarget::from_imported_route(
                crate::ssh_dialog::LoginTarget {
                    host: address.ip().to_string(),
                    port: address.port(),
                    username,
                },
                None,
                crate::session_security::IsolationLevel::None,
            )
            .unwrap(),
        );
        workspace.add_tab_with_privacy(owner, pane, true);
        let mut fixture = Fixture {
            workspace,
            owner,
            pane,
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.session().frame().status != crate::session::Status::Running {
            assert!(
                Instant::now() < deadline,
                "native SSH terminal did not start"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture.session().send(command.into_bytes()).unwrap();
        fixture.wait_for_prompt("kortty-ssh-shortcut>");
        assert!(fixture.session().exec_opener().is_some());
        fixture.type_text("agent disabled-check");
        fixture.enter();
        fixture.wait_for_prompt("kortty-forwarded:disabled-check");
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        configure(&mut fixture.workspace, true);
        assert!(fixture.session().journal_redaction_secrets().is_some());
        assert!(
            fixture
                .workspace
                .agent_shortcut_status(pane)
                .starts_with("Ready to recognize")
        );
        fixture.type_text("agent Inspect the fixture");
        assert_eq!(fixture.draft().line.text(), "agent Inspect the fixture");
        fixture.enter();
        assert_eq!(
            fixture.workspace.terminal_agents.runs.len(),
            1,
            "SSH Agent setup failed: {:?}",
            fixture.workspace.windows[&owner].notice,
        );
        let run = fixture
            .workspace
            .terminal_agents
            .runs
            .values()
            .next()
            .unwrap();
        assert_eq!(run.binding.session(), Some(pane));
        assert_eq!(run.mode, Mode::Run);
        assert_eq!(run.goal, "Inspect the fixture");
        assert!(run.phase == Phase::Setup && run.target.is_none());
        assert_eq!(
            run.redactor
                .journal_text(
                    "ssh-shortcut-synthetic-secret",
                    Instant::now() + Duration::from_secs(1),
                )
                .unwrap()
                .as_str(),
            "***",
        );
        fixture
            .session()
            .send(b"printf '%s\\n' 'kortty-after-shortcut'\r".to_vec())
            .unwrap();
        fixture.wait_for_prompt("kortty-after-shortcut");
        let frame = fixture.session().frame();
        let visible = (0..frame.screen.rows)
            .filter_map(|index| frame.screen.row(index))
            .flat_map(|cells| cells.iter().map(|cell| cell.text.as_str()))
            .collect::<String>();
        assert!(!visible.contains("kortty-forwarded:Inspect the fixture"));
        assert_eq!(fixture.workspace.terminal_agents.runs.len(), 1);
        fixture.session().send(b"exit\r".to_vec()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while fixture.session().frame().status == crate::session::Status::Running {
            assert!(
                Instant::now() < deadline,
                "private remote shell did not exit"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn agent_shortcut_disabled_forwards_literals_and_enabled_editing_stays_local() {
        let mut fixture = Fixture::new();
        assert!(
            fixture
                .workspace
                .agent_shortcut_status(fixture.pane)
                .starts_with("Ready to recognize")
        );
        fixture.workspace.set_test_ai_configuration(
            serde_json::from_value(serde_json::json!({
                "general": {"agent_shortcut": {"enabled": false}},
                "profiles": {
                    "profiles": [{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"fixture"}],
                    "default":1, "last_id":1
                }
            }))
            .unwrap(),
        );
        assert!(
            fixture
                .workspace
                .agent_shortcut_status(fixture.pane)
                .starts_with("Enable terminal Agent shortcuts")
        );
        fixture.type_text("agent --version");
        fixture.enter();
        fixture.wait_for_prompt("line=agent --version");
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
        assert!(
            fixture.workspace.sessions[&fixture.pane]
                .agent_shortcut
                .is_none()
        );

        fixture.workspace.set_test_ai_configuration(
            serde_json::from_value(serde_json::json!({
                "general": {"agent_shortcut": {"enabled": true}},
                "profiles": {
                    "profiles": [{"id":1,"name":"Fixture","endpoint":"127.0.0.1:12345","model":"fixture"}],
                    "default":1, "last_id":1
                }
            }))
            .unwrap(),
        );
        fixture.type_text("agent-ask Inspect thisX");
        let _ = fixture.press(Named::Backspace);
        assert_eq!(fixture.draft().line.text(), "agent-ask Inspect this");
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_some()
        );
        let _ = fixture.press(Named::Escape);
        assert!(
            fixture.workspace.sessions[&fixture.pane]
                .agent_shortcut
                .is_none()
        );
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        fixture.type_text("agent-ask Inspect thisY");
        let _ = fixture.press(Named::Backspace);
        fixture.enter();
        let run = fixture
            .workspace
            .terminal_agents
            .runs
            .values()
            .next()
            .unwrap();
        assert_eq!(run.mode, Mode::Ask);
        assert_eq!(run.goal, "Inspect this");
        assert!(run.phase == Phase::Setup);
    }

    #[test]
    fn agent_shortcut_direct_run_waits_for_plain_enter_and_keeps_probe_review() {
        let mut fixture = Fixture::with_setup(false);
        fixture.type_text("agent ");
        fixture.workspace.send_paste(
            fixture.owner,
            &[fixture.pane],
            "Inspect the working directory",
            false,
        );
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
        let _opened = fixture.press(Named::Enter);
        let run = fixture
            .workspace
            .terminal_agents
            .runs
            .values()
            .next()
            .unwrap();
        assert_eq!(run.goal, "Inspect the working directory");
        assert_eq!(run.mode, Mode::Run);
        #[cfg(unix)]
        assert!(run.phase == Phase::Directory);
        #[cfg(windows)]
        assert!(run.phase == Phase::DirectoryConfirmation);
        assert!(run.target.is_none() && run.activity.is_empty());
        assert!(run.started_order > 0);
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_some()
        );
    }

    #[test]
    fn agent_shortcut_normal_key_and_paste_route_reviews_only_local_input() {
        let mut fixture = Fixture::new();
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        fixture.type_text("agrep");
        fixture.enter();
        fixture.wait_for_prompt("line=agrep");
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        fixture.type_text("agent-");
        fixture.workspace.send_paste(
            fixture.owner,
            &[fixture.pane],
            "plan: Prüfe den Dienst",
            false,
        );
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
        fixture.enter();
        let run = fixture
            .workspace
            .terminal_agents
            .runs
            .values()
            .next()
            .unwrap();
        assert_eq!(run.goal, "Prüfe den Dienst");
        assert_eq!(run.mode, Mode::Plan);
        assert!(run.phase == Phase::Setup && run.epoch == 0);
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_some()
        );

        fixture.type_text("agent-ask stale");
        fixture.session().send(b"other-input\r".to_vec()).unwrap();
        fixture.wait_for_prompt("line=other-input");
        fixture.enter();
        assert_eq!(fixture.workspace.terminal_agents.runs.len(), 1);
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_some()
        );
    }

    #[test]
    fn agent_shortcut_stale_plain_prefix_preserves_fresh_input_and_interrupt() {
        let mut fixture = Fixture::new();
        let original = fixture
            .session()
            .agent_shortcut_prompt(&fixture.session().frame())
            .unwrap();
        fixture.type_text("ag");
        fixture.session().send(b"fresh-prompt\r".to_vec()).unwrap();
        fixture.wait_for_prompt("line=fresh-prompt");
        assert!(
            fixture
                .session()
                .replay_agent_shortcut(original, b"ag".to_vec())
                .is_err()
        );
        fixture.type_text("q");
        fixture.enter();
        fixture.wait_for_prompt("line=q");

        fixture.type_text("a");
        fixture
            .session()
            .send(b"interrupt-prompt\r".to_vec())
            .unwrap();
        fixture.wait_for_prompt("line=interrupt-prompt");
        let _ = fixture.workspace.key(
            fixture.owner,
            Key::Character("c".into()),
            Location::Standard,
            Modifiers::CTRL,
            None,
        );
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_none()
        );
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
    }

    #[test]
    fn agent_shortcut_tab_preserves_shell_tab_and_inserts_a_variant_locally() {
        let mut fixture = Fixture::new();
        let _ = fixture.press(Named::Tab);
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_none()
        );
        fixture.enter();
        fixture.wait_for_prompt("line=");

        fixture.type_text("agent-p");
        let _ = fixture.press(Named::Tab);
        assert!(fixture.draft().completion.is_some());
        fixture.enter();
        assert_eq!(fixture.draft().line.text(), "agent-plan ");
        assert!(fixture.draft().completion.is_none());
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
        assert!(
            fixture
                .session()
                .agent_shortcut_prompt(&fixture.session().frame())
                .is_some()
        );

        fixture.type_text("Check the service");
        fixture.enter();
        let run = fixture
            .workspace
            .terminal_agents
            .runs
            .values()
            .next()
            .unwrap();
        assert!(run.mode == Mode::Plan && run.phase == Phase::Setup);
        assert_eq!(run.goal, "Check the service");
    }

    #[test]
    fn agent_shortcut_tab_reads_masked_history_and_discards_stale_results() {
        use crate::ai_manager::Configuration;
        use crate::snippets::tests::Profile;
        use iced::futures::{StreamExt, executor::block_on};

        fn loaded(task: Task<Desktop>) -> Desktop {
            let mut stream = iced_runtime::task::into_stream(task).expect("history read");
            let Some(iced_runtime::Action::Output(message @ Desktop::AgentShortcut(_))) =
                block_on(stream.next())
            else {
                panic!("missing shortcut history response");
            };
            message
        }

        let profile = Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut fixture = Fixture::new();
        fixture
            .workspace
            .ai_configuration()
            .unwrap()
            .save_fixture_at(&root, &Configuration::default())
            .unwrap();
        fixture
            .workspace
            .set_test_ai_configuration(Configuration::load_fixture_from(&root).unwrap());
        crate::terminal_agent::history::remember(
            &root,
            "Inspect *** safely".into(),
            Mode::Ask,
            false,
        )
        .unwrap();

        fixture.type_text("agent-ask ");
        let result = loaded(fixture.press(Named::Tab));
        let _ = fixture.workspace.update(result);
        fixture.enter();
        assert_eq!(fixture.draft().line.text(), "agent-ask Inspect *** safely");
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
        let _ = fixture.press(Named::Escape);
        fixture.type_text("agent ");
        let result = loaded(fixture.press(Named::Tab));
        fixture.session().send(b"new-prompt\r".to_vec()).unwrap();
        fixture.wait_for_prompt("line=new-prompt");
        let _ = fixture.workspace.update(result);
        assert!(fixture.draft().completion.is_none());
        fixture.enter();
        assert!(fixture.workspace.terminal_agents.runs.is_empty());

        fixture
            .workspace
            .active_tab_mut(fixture.owner)
            .unwrap()
            .incognito = true;
        fixture.type_text("agent ");
        assert!(iced_runtime::task::into_stream(fixture.press(Named::Tab)).is_none());
        fixture.enter();
        assert_eq!(fixture.draft().line.text(), "agent ");
        assert!(fixture.workspace.terminal_agents.runs.is_empty());
    }
}
