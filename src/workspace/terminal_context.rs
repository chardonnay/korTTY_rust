use iced::widget::{button, column, container, mouse_area, row, rule, space, text};
use iced::{Alignment, Element, Length, Point, Task, window};
use kortty_terminal::PromptDirection;

use crate::terminal_effects::Effect;
use crate::{
    session::{Frame, SessionId},
    terminal_links::Hit,
    terminal_view::Selection,
};

use super::{
    Message as Desktop, PaneSession, Tab, Workspace,
    chrome::{self, Navigation},
    multi_exec_access::Action as MultiExecAction,
    shell_integration,
    terminal_commands::Command,
};
use iced::widget::pane_grid;
use std::sync::{Arc, Weak};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Root,
    Extras,
    Font,
    Split,
    Effects,
    Ai,
}

impl Section {
    fn parent(self) -> Option<Self> {
        match self {
            Self::Root => None,
            Self::Extras | Self::Effects | Self::Ai => Some(Self::Root),
            Self::Font | Self::Split => Some(Self::Extras),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Root => "",
            Self::Extras => "Extras",
            Self::Font => "Font Size",
            Self::Split => "Split Terminal",
            Self::Effects => "Terminal Effect",
            Self::Ai => "AI",
        }
    }

    fn includes(self, section: Self) -> bool {
        self == section || self.parent().is_some_and(|parent| parent.includes(section))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FontAdjustment {
    Increase,
    Decrease,
    Reset,
}

impl PaneSession {
    fn adjust_font(&mut self, adjustment: FontAdjustment, step: u8) -> bool {
        let previous = self.font;
        self.font = match adjustment {
            FontAdjustment::Reset => self.font_baseline,
            FontAdjustment::Decrease => self
                .font
                .saturating_sub(step)
                .max(crate::profile::MIN_TERMINAL_FONT),
            FontAdjustment::Increase => self
                .font
                .saturating_add(step)
                .min(crate::profile::MAX_TERMINAL_FONT),
        };
        self.font != previous
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    OpenLink,
    CopyLink,
    Copy,
    Paste,
    ClearBuffer,
    Find,
    Sftp,
    SerialBreak,
    Reconnect,
    SetupShellIntegration,
    PreviousPrompt,
    NextPrompt,
    SelectLastOutput,
    CopyLastOutput,
    FontIncrease,
    FontDecrease,
    FontReset,
    SplitRight,
    SplitRightNew,
    SplitDown,
    SplitDownNew,
    CloseSplit,
    Broadcast,
    MultiExec,
    TerminalEffect(Option<Effect>),
    ShowCommandTimestamps,
    ShowMenuBar,
    Agent(chrome::Action),
    Ai(super::terminal_ai::Action),
    ContinueAgentChat,
}

impl Action {
    fn label(self) -> &'static str {
        match self {
            Self::OpenLink => "Open Link",
            Self::CopyLink => "Copy Link Address",
            Self::Copy => "Copy",
            Self::Paste => "Paste",
            Self::ClearBuffer => "Clear Buffer",
            Self::Find => "Find",
            Self::Sftp => "SFTP Here",
            Self::SerialBreak => "Send Break",
            Self::Reconnect => "Reconnect",
            Self::SetupShellIntegration => "Set Up Shell Integration…",
            Self::PreviousPrompt => "Previous Prompt",
            Self::NextPrompt => "Next Prompt",
            Self::SelectLastOutput => "Select Last Output",
            Self::CopyLastOutput => "Copy Last Output",
            Self::FontIncrease => "Increase",
            Self::FontDecrease => "Decrease",
            Self::FontReset => "Reset",
            Self::SplitRight => "Split Right (same server)",
            Self::SplitRightNew => "Split Right (new connection)",
            Self::SplitDown => "Split Down (same server)",
            Self::SplitDownNew => "Split Down (new connection)",
            Self::CloseSplit => "Close Split",
            Self::Broadcast => "Broadcast Mode",
            Self::MultiExec => "Multi-exec: Include This Pane",
            Self::TerminalEffect(effect) => effect.map_or("None", Effect::name),
            Self::ShowCommandTimestamps => "Show Command Timestamps",
            Self::ShowMenuBar => "Show Menu Bar",
            Self::Agent(action) => action.title(),
            Self::Ai(action) => action.label(),
            Self::ContinueAgentChat => "Continue Last Agent Run in Chat…",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Entry {
    Action(Action),
    Branch(Section),
    Separator,
}

use Action::*;
use Entry::{Action as Item, Branch, Separator};

#[derive(Clone, Debug)]
pub enum Message {
    Open(SessionId, Point, Option<Arc<Hit>>),
    Point(u64, Section, Entry),
    Invoke(u64, Section, Entry),
    Navigate(u64, Navigation),
    Dismiss(u64),
}

#[derive(Clone)]
pub(super) struct Context {
    owner: window::Id,
    request: u64,
    session: SessionId,
    point: Point,
    link: Option<Arc<Hit>>,
    section: Section,
    selected: Option<Entry>,
}

impl Workspace {
    pub(super) fn dismiss_terminal_context(&mut self, owner: window::Id) {
        self.terminal_context.take_if(|menu| menu.owner == owner);
    }

    pub(super) fn window_contains_visible_pane(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> bool {
        self.visible_pane(owner, session).is_some()
    }

    pub(super) fn visible_pane(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<pane_grid::Pane> {
        let window = self.windows.get(&owner)?;
        let (tab_index, pane) = window.pane_location(session)?;
        if tab_index != window.active
            || window
                .tab()?
                .grid
                .maximized()
                .is_some_and(|shown| shown != pane)
        {
            return None;
        }
        Some(pane)
    }

    fn terminal_context_pane(
        &self,
        menu: &Context,
    ) -> Option<(&Tab, &PaneSession, pane_grid::Pane)> {
        let pane = self.visible_pane(menu.owner, menu.session)?;
        Some((
            self.windows.get(&menu.owner)?.tab()?,
            self.sessions.get(&menu.session)?,
            pane,
        ))
    }

    pub(super) fn update_terminal_context(
        &mut self,
        owner: window::Id,
        message: Message,
    ) -> Task<Desktop> {
        let request = match message {
            Message::Open(session, point, link) => {
                if !self.palette_can_open(owner) || self.palette.is_open(owner) {
                    return Task::none();
                }
                let Some(pane) = self.visible_pane(owner, session) else {
                    return Task::none();
                };
                if let Some(tab) = self.active_tab_mut(owner) {
                    tab.focused = pane;
                }
                self.chrome.dismiss(owner);
                self.terminal_context = Some(Context {
                    owner,
                    request: self.allocate(),
                    session,
                    point,
                    link,
                    section: Section::Root,
                    selected: None,
                });
                return Task::none();
            }
            Message::Point(request, ..)
            | Message::Invoke(request, ..)
            | Message::Navigate(request, _)
            | Message::Dismiss(request) => request,
        };
        let Some(mut menu) = self
            .terminal_context
            .as_ref()
            .filter(|menu| menu.owner == owner && menu.request == request)
            .cloned()
        else {
            return Task::none();
        };
        if matches!(
            message,
            Message::Dismiss(_) | Message::Navigate(_, Navigation::Escape)
        ) {
            self.dismiss_terminal_context(owner);
            return Task::none();
        }
        let Some((tab, pane, target)) = self
            .terminal_context_pane(&menu)
            .filter(|_| self.palette_can_open(owner))
        else {
            self.dismiss_terminal_context(owner);
            return Task::none();
        };
        let choices = |section| {
            self.terminal_context_entries(owner, section, menu.session, pane, menu.link.is_some())
                .into_iter()
                .filter(|entry| {
                    self.terminal_context_entry_enabled(*entry, owner, menu.session, tab, pane)
                })
                .collect::<Vec<_>>()
        };
        let mut action = None;
        match message {
            Message::Point(_, section, entry) | Message::Invoke(_, section, entry)
                if menu.section.includes(section) && choices(section).contains(&entry) =>
            {
                match entry {
                    Branch(child) => {
                        menu.section = child;
                        menu.selected = choices(child).first().copied();
                    }
                    Item(value) if matches!(message, Message::Invoke(..)) => action = Some(value),
                    _ => {
                        menu.section = section;
                        menu.selected = Some(entry);
                    }
                }
            }
            Message::Navigate(_, Navigation::Previous | Navigation::Next) => {
                let entries = choices(menu.section);
                if !entries.is_empty() {
                    let previous = matches!(message, Message::Navigate(_, Navigation::Previous));
                    let current = entries
                        .iter()
                        .position(|entry| Some(*entry) == menu.selected);
                    let index = match (current, previous) {
                        (Some(index), true) => (index + entries.len() - 1) % entries.len(),
                        (Some(index), false) => (index + 1) % entries.len(),
                        (None, true) => entries.len() - 1,
                        (None, false) => 0,
                    };
                    menu.selected = Some(entries[index]);
                }
            }
            Message::Navigate(_, Navigation::Left) => {
                if let Some(parent) = menu.section.parent() {
                    menu.selected = Some(Branch(menu.section));
                    menu.section = parent;
                }
            }
            Message::Navigate(_, Navigation::Right | Navigation::Activate) => {
                let entries = choices(menu.section);
                match menu.selected.filter(|entry| entries.contains(entry)) {
                    Some(Branch(child)) => {
                        menu.section = child;
                        menu.selected = choices(child).first().copied();
                    }
                    Some(Item(value))
                        if matches!(message, Message::Navigate(_, Navigation::Activate)) =>
                    {
                        action = Some(value);
                    }
                    None => menu.selected = entries.first().copied(),
                    _ => {}
                }
            }
            _ => {}
        }
        let Some(action) = action else {
            self.terminal_context = Some(menu);
            return Task::none();
        };
        self.dismiss_terminal_context(owner);
        if let Some(tab) = self.active_tab_mut(owner) {
            tab.focused = target;
        }
        match (action, menu.link) {
            (OpenLink, Some(link)) => self.update_terminal_link(
                owner,
                super::terminal_link_dialog::Message::Open(menu.session, link),
            ),
            (CopyLink, Some(link)) => self.write_clipboard(link.address()),
            _ => self.invoke_terminal_context(owner, menu.session, target, action),
        }
    }

    fn invoke_terminal_context(
        &mut self,
        owner: window::Id,
        session: SessionId,
        pane: pane_grid::Pane,
        action: Action,
    ) -> Task<Desktop> {
        match action {
            OpenLink | CopyLink => Task::none(),
            Copy => self.copy_terminal_selection(session),
            Agent(action) => action.agent_mode().map_or_else(Task::none, |mode| {
                self.open_terminal_agent(owner, session, mode)
            }),
            Ai(action) => self.open_terminal_ai(owner, session, action),
            ContinueAgentChat => self.continue_latest_agent_chat(owner, session),
            Paste => self.update(Desktop::Paste(owner)),
            ClearBuffer => self.terminal_command(session, Command::ClearBuffer),
            Find => self.update(Desktop::OpenSearch(session)),
            Sftp => self.open_attached_sftp(owner).unwrap_or_else(Task::none),
            SerialBreak => self.update(Desktop::SerialBreak(owner, session)),
            Reconnect => {
                let Some(tab) = self.windows.get(&owner).and_then(|window| window.tab()) else {
                    return Task::none();
                };
                self.reconnect_tab(owner, tab.id)
            }
            SetupShellIntegration => self
                .update_shell_integration(owner, shell_integration::Message::Open(Some(session))),
            PreviousPrompt | NextPrompt => {
                if let Some(pane) = self.sessions.get(&session) {
                    let direction = if action == PreviousPrompt {
                        PromptDirection::Previous
                    } else {
                        PromptDirection::Next
                    };
                    pane.session.navigate_prompt(direction);
                }
                Task::none()
            }
            SelectLastOutput => self.terminal_command(session, Command::SelectLastOutput),
            CopyLastOutput => self.terminal_command(session, Command::CopyLastOutput),
            FontIncrease | FontDecrease | FontReset => {
                let adjustment = match action {
                    FontIncrease => FontAdjustment::Increase,
                    FontDecrease => FontAdjustment::Decrease,
                    _ => FontAdjustment::Reset,
                };
                if let Some(pane) = self.sessions.get_mut(&session) {
                    pane.adjust_font(adjustment, 2);
                }
                Task::none()
            }
            SplitRight => self.split_same_server(owner, session, pane_grid::Axis::Vertical),
            SplitDown => self.split_same_server(owner, session, pane_grid::Axis::Horizontal),
            SplitRightNew => self.show_split_connection(owner, session, pane_grid::Axis::Vertical),
            SplitDownNew => self.show_split_connection(owner, session, pane_grid::Axis::Horizontal),
            CloseSplit => self.update(Desktop::ClosePane(owner, pane)),
            Broadcast => self.update(Desktop::ToggleBroadcast(owner)),
            MultiExec => self.update(Desktop::MultiExec(
                Some(owner),
                MultiExecAction::PaneAt(session),
            )),
            TerminalEffect(effect) => {
                self.update(Desktop::SetTerminalEffect(owner, session, effect))
            }
            ShowCommandTimestamps => {
                if self.visible_pane(owner, session) == Some(pane) {
                    self.update(Desktop::ToggleCommandTimestamps(owner))
                } else {
                    Task::none()
                }
            }
            ShowMenuBar => self.show_menu_bar(owner),
        }
    }

    pub(super) fn copy_terminal_selection(&mut self, session: SessionId) -> Task<Desktop> {
        let Some(pane) = self.sessions.get(&session) else {
            return Task::none();
        };
        if let Some(selection) = &pane.projected_selection {
            return selection
                .text(&pane.frame, pane.stream_generation)
                .filter(|value| !value.is_empty())
                .map_or_else(Task::none, |value| self.write_clipboard(value));
        }
        if let Some(selection) = pane.selection {
            return self.write_clipboard(selection.text(&pane.frame.screen));
        }
        self.terminal_command(session, Command::CopySelection)
    }

    pub(super) fn finish_terminal_selection(
        &self,
        owner: window::Id,
        session: SessionId,
        selection: Selection,
        frame: &Weak<Frame>,
    ) -> Task<Desktop> {
        if !self.terminal_copy_on_select_allowed(owner, session) {
            return Task::none();
        }
        let Some(pane) = self.sessions.get(&session).filter(|pane| {
            pane.selection == Some(selection)
                && frame
                    .upgrade()
                    .is_some_and(|rendered| Arc::ptr_eq(&rendered, &pane.frame))
        }) else {
            return Task::none();
        };
        self.write_terminal_selection(selection.visible_text(&pane.frame.screen))
    }

    pub(super) fn finish_projected_selection(
        &self,
        owner: window::Id,
        session: SessionId,
        selection: crate::terminal_view::projected_selection::ProjectedSelection,
    ) -> Task<Desktop> {
        if !self.terminal_copy_on_select_allowed(owner, session) {
            return Task::none();
        }
        let Some(pane) = self.sessions.get(&session) else {
            return Task::none();
        };
        let Some(range) = selection.range(&pane.frame, pane.stream_generation) else {
            return Task::none();
        };
        if pane
            .projected_selection
            .as_ref()
            .and_then(|current| current.range(&pane.frame, pane.stream_generation))
            != Some(range)
        {
            return Task::none();
        }
        selection
            .text(&pane.frame, pane.stream_generation)
            .map_or_else(Task::none, |value| self.write_terminal_selection(value))
    }

    fn terminal_copy_on_select_allowed(&self, owner: window::Id, session: SessionId) -> bool {
        self.window_settings.terminal_copy_on_select_enabled
            && !self.exiting
            && !self.scheduler_draining
            && self
                .windows
                .get(&owner)
                .is_some_and(|window| window.focused)
            && self.window_contains_visible_pane(owner, session)
            && self.palette_can_open(owner)
            && !self.palette.is_open(owner)
    }

    fn write_terminal_selection(&self, value: String) -> Task<Desktop> {
        if value.is_empty() {
            return Task::none();
        }
        #[cfg(target_os = "linux")]
        if !self.clipboard.policy.internal {
            return Task::batch([
                iced::clipboard::write_primary(value.clone()),
                self.write_clipboard(value),
            ]);
        }
        self.write_clipboard(value)
    }

    fn focused_terminal_target(
        &self,
        owner: window::Id,
        action: chrome::Action,
    ) -> Option<(SessionId, pane_grid::Pane, Action)> {
        let tab = self.windows.get(&owner)?.tab()?;
        let session = tab.focused_session()?;
        let pane = self.sessions.get(&session)?;
        let action = match action {
            chrome::Action::ClearBuffer => ClearBuffer,
            chrome::Action::PreviousPrompt => PreviousPrompt,
            chrome::Action::NextPrompt => NextPrompt,
            chrome::Action::SelectLastOutput => SelectLastOutput,
            chrome::Action::CopyLastOutput => CopyLastOutput,
            chrome::Action::SftpHere => Sftp,
            chrome::Action::SendSerialBreak => SerialBreak,
            _ => return None,
        };
        self.terminal_context_entry_enabled(Item(action), owner, session, tab, pane)
            .then_some((session, tab.focused, action))
    }

    pub(super) fn focused_terminal_action_enabled(
        &self,
        owner: window::Id,
        action: chrome::Action,
    ) -> bool {
        self.focused_terminal_target(owner, action).is_some()
    }

    pub(super) fn invoke_focused_terminal_action(
        &mut self,
        owner: window::Id,
        action: chrome::Action,
    ) -> Task<Desktop> {
        let Some((session, pane, action)) = self.focused_terminal_target(owner, action) else {
            return Task::none();
        };
        self.invoke_terminal_context(owner, session, pane, action)
    }

    pub(super) fn adjust_tab_font(&mut self, owner: window::Id, adjustment: FontAdjustment) {
        let Some(tab) = self.windows.get(&owner).and_then(|window| window.tab()) else {
            return;
        };
        let mut changed = false;
        for (_, session) in tab.grid.iter() {
            if let Some(pane) = self.sessions.get_mut(session) {
                changed |= pane.adjust_font(adjustment, 1);
            }
        }
        if changed {
            self.session_restore.changed(false);
        }
    }

    pub(super) fn terminal_prompt_entries(
        &self,
        pane: &PaneSession,
    ) -> Option<shell_integration::Entries> {
        shell_integration::Entries::from_frame(
            self.window_settings.prompt_navigation_enabled,
            &pane.frame,
        )
    }

    fn terminal_context_agent_entries(
        &self,
        session: SessionId,
    ) -> impl Iterator<Item = Entry> + '_ {
        [
            chrome::Action::TerminalAgent,
            chrome::Action::TerminalAgentAsk,
            chrome::Action::TerminalAgentPlan,
        ]
        .into_iter()
        .filter(move |action| {
            action
                .agent_mode()
                .is_some_and(|mode| self.terminal_agent_available(session, mode))
        })
        .map(|action| Item(Agent(action)))
    }

    fn terminal_context_entries(
        &self,
        owner: window::Id,
        section: Section,
        session: SessionId,
        pane: &PaneSession,
        has_link: bool,
    ) -> Vec<Entry> {
        let chat_allowed =
            self.clipboard.policy.ai_allowed && self.clipboard.policy.ai_chat_allowed;
        match section {
            Section::Root => {
                let mut entries = Vec::new();
                if self.menu_bar_hidden(owner) {
                    entries.extend([Item(ShowMenuBar), Separator]);
                }
                if has_link {
                    entries.extend([Item(OpenLink), Item(CopyLink), Separator]);
                }
                entries.extend([Item(Copy), Item(Paste), Item(ClearBuffer), Item(Find)]);
                if self
                    .terminal_context_agent_entries(session)
                    .next()
                    .is_some()
                    || (chat_allowed
                        && (pane.selection.is_some()
                            || pane.frame.ibm.is_none()
                            || self.agent_chat_handoff_available(session)))
                {
                    entries.extend([Separator, Branch(Section::Ai)]);
                }
                if pane.ssh.is_some() && pane.session.sftp_opener().is_some() {
                    entries.extend([Separator, Item(Sftp)]);
                }
                if pane.frame.serial.is_some()
                    && pane.frame.status == crate::session::Status::Running
                {
                    entries.extend([Separator, Item(SerialBreak)]);
                }
                match self.terminal_prompt_entries(pane) {
                    Some(shell_integration::Entries::Setup) => {
                        entries.extend([Separator, Item(SetupShellIntegration)]);
                    }
                    Some(shell_integration::Entries::Prompts { .. }) => {
                        entries.extend([
                            Separator,
                            Item(PreviousPrompt),
                            Item(NextPrompt),
                            Item(SelectLastOutput),
                            Item(CopyLastOutput),
                            Item(SetupShellIntegration),
                        ]);
                    }
                    None => {}
                }
                if self.terminal_effects_enabled() {
                    entries.extend([Separator, Branch(Section::Effects)]);
                }
                entries.extend([
                    Separator,
                    Item(Reconnect),
                    Separator,
                    Item(ShowCommandTimestamps),
                    Separator,
                    Branch(Section::Extras),
                ]);
                entries
            }
            Section::Extras => vec![
                Branch(Section::Font),
                Branch(Section::Split),
                Separator,
                Item(Broadcast),
                Item(MultiExec),
            ],
            Section::Font => vec![Item(FontIncrease), Item(FontDecrease), Item(FontReset)],
            Section::Split => vec![
                Item(SplitRight),
                Item(SplitRightNew),
                Separator,
                Item(SplitDown),
                Item(SplitDownNew),
                Separator,
                Item(CloseSplit),
            ],
            Section::Effects => std::iter::once(Item(TerminalEffect(None)))
                .chain(
                    Effect::ALL
                        .into_iter()
                        .filter(|effect| self.allowed_terminal_effect(*effect))
                        .map(|effect| Item(TerminalEffect(Some(effect)))),
                )
                .collect(),
            Section::Ai => {
                use super::terminal_ai::Action;
                let mut entries: Vec<_> = self.terminal_context_agent_entries(session).collect();
                if chat_allowed {
                    if !entries.is_empty() {
                        entries.push(Separator);
                    }
                    if pane.selection.is_some() {
                        entries.extend([
                            Item(Ai(Action::Summarize)),
                            Item(Ai(Action::SolveProblem)),
                            Item(Ai(Action::Ask)),
                        ]);
                    } else {
                        entries.push(Item(Ai(Action::SummarizeRecent)));
                    }
                }
                if self.agent_chat_handoff_available(session) {
                    if !entries.is_empty() {
                        entries.push(Separator);
                    }
                    entries.push(Item(ContinueAgentChat));
                }
                entries
            }
        }
    }

    fn terminal_context_entry_enabled(
        &self,
        entry: Entry,
        owner: window::Id,
        session: SessionId,
        tab: &Tab,
        pane: &PaneSession,
    ) -> bool {
        match entry {
            Separator => false,
            Item(Copy) => {
                pane.selection.is_some()
                    || pane.selected_output.is_some()
                    || pane.projected_selection.as_ref().is_some_and(|selection| {
                        selection
                            .range(&pane.frame, pane.stream_generation)
                            .is_some()
                    })
            }
            Branch(Section::Ai) => {
                self.terminal_context_agent_entries(session)
                    .next()
                    .is_some()
                    || self.terminal_ai_available(pane, None)
                    || self.agent_chat_handoff_available(session)
            }
            Item(Agent(action)) => action
                .agent_mode()
                .is_some_and(|mode| self.terminal_agent_available(session, mode)),
            Item(Ai(action)) => self.terminal_ai_available(pane, Some(action)),
            Item(ContinueAgentChat) => self.agent_chat_handoff_available(session),
            Item(ClearBuffer) => pane.frame.ibm.is_none() && !pane.frame.screen.alternate_screen,
            Item(Find) => pane.session.supports_search(),
            Item(Sftp) => self.remote_pane(session).is_some(),
            Item(SerialBreak) => {
                pane.frame.serial.is_some() && pane.frame.status == crate::session::Status::Running
            }
            Item(Reconnect) => self.can_reconnect_tab(owner, tab.id),
            Item(PreviousPrompt | NextPrompt | SelectLastOutput | CopyLastOutput) => {
                self.terminal_prompt_entries(pane)
                    == Some(shell_integration::Entries::Prompts { available: true })
                    && (!matches!(entry, Item(SelectLastOutput | CopyLastOutput))
                        || pane.frame.has_finished_command)
            }
            Item(SplitRight | SplitDown) => self.same_server_split_allowed(owner, session),
            Item(SplitRightNew | SplitDownNew) => self.split_connection_allowed(owner, session),
            Item(CloseSplit) => tab.grid.len() >= 2,
            Item(Broadcast) => self.broadcast_menu_enabled(owner),
            Item(MultiExec) => {
                self.multi_exec_available(Some(owner), MultiExecAction::PaneAt(session))
            }
            _ => true,
        }
    }

    pub(super) fn terminal_context_view<'a>(
        &'a self,
        owner: window::Id,
        session: SessionId,
        trigger: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(menu) = self
            .terminal_context
            .as_ref()
            .filter(|menu| menu.owner == owner && menu.session == session)
        else {
            return trigger;
        };
        let Some((tab, pane, _)) = self
            .terminal_context_pane(menu)
            .filter(|_| self.palette_can_open(owner))
        else {
            return trigger;
        };
        let request = menu.request;
        let send = move |message| Desktop::TerminalContext(owner, message);
        chrome::popup::at_point(
            trigger,
            menu.point,
            self.terminal_context_section(menu, Section::Root, tab, pane),
            send(Message::Dismiss(request)),
            move |navigation| send(Message::Navigate(request, navigation)),
        )
    }

    fn terminal_context_section(
        &self,
        menu: &Context,
        section: Section,
        tab: &Tab,
        pane: &PaneSession,
    ) -> Element<'_, Desktop> {
        let send = move |message| Desktop::TerminalContext(menu.owner, message);
        let mut content = column![].spacing(2);
        for entry in self.terminal_context_entries(
            menu.owner,
            section,
            menu.session,
            pane,
            menu.link.is_some(),
        ) {
            if entry == Separator {
                content = content.push(rule::horizontal(1));
                continue;
            }
            let (label, child) = match entry {
                Item(action) => (action.label(), None),
                Branch(child) => (child.label(), Some(child)),
                Separator => continue,
            };
            let expanded = child.is_some_and(|child| menu.section.includes(child));
            let selected = (menu.section == section && menu.selected == Some(entry)) || expanded;
            let checked = match entry {
                Item(Broadcast) => tab.broadcast,
                Item(MultiExec) => {
                    self.multi_exec_checked(menu.owner, MultiExecAction::PaneAt(menu.session))
                }
                Item(ShowCommandTimestamps) => tab.command_timestamps,
                Item(TerminalEffect(effect)) => {
                    self.pane_effect(menu.session)
                        .map(|active| active.selection.effect)
                        == effect
                }
                _ => false,
            };
            let enabled =
                self.terminal_context_entry_enabled(entry, menu.owner, menu.session, tab, pane);
            let shortcut =
                (entry == Item(ClearBuffer) && cfg!(target_os = "macos")).then_some("⌘K");
            let item = button(
                row![
                    text(if checked { "✓" } else { "" }).width(15),
                    text(label).size(13),
                    space::horizontal(),
                    text(child.map_or(shortcut.unwrap_or(""), |_| "›")).size(13),
                ]
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([5, 6])
            .style(if selected {
                button::secondary
            } else {
                button::text
            })
            .on_press_maybe(enabled.then(|| send(Message::Invoke(menu.request, section, entry))));
            let item = if enabled {
                mouse_area(item)
                    .on_enter(send(Message::Point(menu.request, section, entry)))
                    .into()
            } else {
                item.into()
            };
            content = content.push(if let Some(child) = child.filter(|_| expanded) {
                chrome::popup::submenu(item, self.terminal_context_section(menu, child, tab, pane))
            } else {
                item
            });
        }
        if section == Section::Effects
            && let Some(speed) = self.effect_speed_view(menu.owner, menu.session)
        {
            content = content.push(rule::horizontal(1)).push(speed);
        }
        container(content)
            .width(if matches!(section, Section::Split | Section::Extras) {
                245
            } else {
                222
            })
            .padding(4)
            .style(container::rounded_box)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{GridSize, Session};
    use kortty_terminal::TerminalProfile;

    fn pane(workspace: &mut Workspace) -> SessionId {
        let grid = GridSize::default();
        let session = Session::ssh(None, grid, TerminalProfile::Xterm, true, None).unwrap();
        workspace.store_session(session, grid, 14)
    }

    #[test]
    fn terminal_copy_on_select_requires_current_visible_selection_and_keeps_internal_clipboard() {
        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        let (owner, _) = workspace.open_window(false);
        let session = pane(&mut workspace);
        workspace.add_tab(owner, session);
        let _ = workspace.update(Desktop::Window(owner, window::Event::Focused));
        let mut terminal = kortty_terminal::Terminal::new(20, 2);
        terminal.advance(b"visible \x1b[8msecret\x1b[0m");
        let frame = crate::terminal_view::selection_test_frame(terminal.snapshot());
        workspace.sessions.get_mut(&session).unwrap().frame = Arc::clone(&frame);
        let selection = Selection::from_cells((0, 0), (0, 13));
        let release =
            || Desktop::SelectionFinished(owner, session, selection, Arc::downgrade(&frame));

        workspace.clipboard.buffer.set("previous".into());
        let _ = workspace.update(Desktop::Selection(session, Some(selection)));
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );
        assert_eq!(workspace.update(release()).units(), 0);
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("visible"));

        workspace.clipboard.buffer.set("previous".into());
        workspace.window_settings.terminal_copy_on_select_enabled = false;
        let _ = workspace.update(release());
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );
        workspace.window_settings.terminal_copy_on_select_enabled = true;
        let _ = workspace.update(Desktop::Selection(session, None));
        let _ = workspace.update(release());
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );

        let _ = workspace.update(Desktop::Selection(session, Some(selection)));
        let other = pane(&mut workspace);
        workspace.add_tab(owner, other);
        let _ = workspace.update(release());
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );
        let original = workspace.windows[&owner].terminal_tabs().next().unwrap().id;
        workspace.select_tab(owner, original, false);
        workspace.sessions.get_mut(&session).unwrap().frame =
            crate::terminal_view::selection_test_frame(terminal.snapshot());
        let _ = workspace.update(release());
        assert_eq!(
            workspace.clipboard.buffer.get().as_deref(),
            Some("previous")
        );
        for pane in workspace.sessions.values() {
            let _ = pane.session.stop().recv_blocking();
        }
    }

    #[test]
    fn projected_copy_uses_the_internal_clipboard_and_rejects_a_stale_rendered_frame() {
        use crate::terminal_agent::design::inline::{Line, Role, Segment};
        use crate::terminal_view::projected_selection::ProjectedSelection;

        let mut workspace = Workspace::default();
        workspace.clipboard.policy.internal = true;
        let (owner, _) = workspace.open_window(false);
        let session = pane(&mut workspace);
        workspace.add_tab(owner, session);
        let _ = workspace.update(Desktop::Window(owner, window::Event::Focused));
        let mut terminal = kortty_terminal::Terminal::new(32, 4);
        terminal.advance(b"\x1b[8mconcealed\x1b[0m prompt");
        let mut frame = crate::terminal_view::selection_test_frame(terminal.snapshot());
        crate::session::inline_history::fixture(
            Arc::get_mut(&mut frame).unwrap(),
            0,
            vec![Line {
                segments: vec![Segment {
                    text: "Agent answer".into(),
                    role: Role::Text,
                }],
            }],
            false,
        );
        let pane = workspace.sessions.get_mut(&session).unwrap();
        pane.frame = Arc::clone(&frame);
        let selection = ProjectedSelection::new(
            &frame,
            pane.stream_generation,
            Selection::from_cells((0, 0), (1, 31)),
        );

        workspace.clipboard.buffer.set("prior".into());
        let _ = workspace.update(Desktop::Selection(
            session,
            Some(Selection::from_cells((0, 0), (0, 8))),
        ));
        assert!(workspace.sessions[&session].selection.is_none());
        let _ = workspace.update(Desktop::ProjectedSelection(
            owner,
            session,
            selection.clone(),
        ));
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("prior"));
        let _ = workspace.update(Desktop::ProjectedSelectionFinished(
            owner,
            session,
            selection.clone(),
        ));
        let copied = workspace.clipboard.buffer.get().unwrap();
        assert!(copied.starts_with("Agent answer\n"));
        assert!(copied.ends_with(" prompt"));
        assert!(!copied.contains("concealed"));
        assert!(workspace.sessions[&session].selection.is_none());
        let _ = workspace.update(Desktop::Copy(owner));
        assert_eq!(workspace.clipboard.buffer.get(), Some(copied));

        workspace.clipboard.buffer.set("prior".into());
        workspace.sessions.get_mut(&session).unwrap().frame = Arc::new(frame.as_ref().clone());
        let _ = workspace.update(Desktop::ProjectedSelectionFinished(
            owner, session, selection,
        ));
        let _ = workspace.update(Desktop::Copy(owner));
        assert_eq!(workspace.clipboard.buffer.get().as_deref(), Some("prior"));
        let _ = workspace.sessions[&session].session.stop().recv_blocking();
    }

    #[test]
    fn reconnect_is_a_root_command_and_disabled_without_a_restart_route() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = pane(&mut workspace);
        workspace.add_tab(owner, session);
        let tab = workspace.windows[&owner].tab().unwrap();
        let pane = &workspace.sessions[&session];
        assert!(
            workspace
                .terminal_context_entries(owner, Section::Root, session, pane, false)
                .ends_with(&[
                    Separator,
                    Item(Reconnect),
                    Separator,
                    Item(ShowCommandTimestamps),
                    Separator,
                    Branch(Section::Extras)
                ])
        );
        assert!(!workspace.terminal_context_entry_enabled(
            Item(Reconnect),
            owner,
            session,
            tab,
            pane
        ));
    }

    #[test]
    fn terminal_context_restores_only_its_hidden_window_menu_bar() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let (other, _) = workspace.open_window(false);
        let session = pane(&mut workspace);
        workspace.add_tab(owner, session);
        let hide = |id| Desktop::Chrome(chrome::Message::Invoke(id, chrome::Action::ShowMenuBar));
        let _ = workspace.update(hide(owner));
        let _ = workspace.update(hide(other));
        assert!(workspace.menu_bar_hidden(owner));
        assert!(workspace.menu_bar_hidden(other));

        let _ =
            workspace.update_terminal_context(owner, Message::Open(session, Point::ORIGIN, None));
        let request = workspace.terminal_context.as_ref().unwrap().request;
        let _ = workspace.update_terminal_context(
            owner,
            Message::Invoke(request, Section::Root, Item(ShowMenuBar)),
        );
        assert!(!workspace.menu_bar_hidden(owner));
        assert!(workspace.menu_bar_hidden(other));
        assert!(workspace.terminal_context.is_none());
    }

    #[test]
    fn nested_context_commands_keep_the_pane_from_the_right_click() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let source = pane(&mut workspace);
        let sibling = pane(&mut workspace);
        workspace.add_tab(owner, source);
        let tab = workspace.active_tab_mut(owner).unwrap();
        let first = tab.focused;
        let (second, _) = tab
            .grid
            .split(pane_grid::Axis::Vertical, first, sibling)
            .unwrap();
        tab.focused = second;
        let _ =
            workspace.update_terminal_context(owner, Message::Open(source, Point::ORIGIN, None));
        assert_eq!(
            workspace.windows[&owner].tab().unwrap().focused_session(),
            Some(source)
        );

        let request = workspace.terminal_context.as_ref().unwrap().request;
        let _ = workspace.update_terminal_context(
            owner,
            Message::Point(request, Section::Root, Branch(Section::Extras)),
        );
        let _ = workspace.update_terminal_context(
            owner,
            Message::Point(request, Section::Extras, Branch(Section::Font)),
        );
        workspace.active_tab_mut(owner).unwrap().focused = second;
        let _ = workspace
            .update_terminal_context(owner, Message::Navigate(request, Navigation::Activate));

        assert_eq!(workspace.sessions[&source].font, 16);
        assert_eq!(workspace.sessions[&sibling].font, 14);
        assert!(workspace.terminal_context.is_none());
    }

    #[test]
    fn terminal_context_agent_actions_follow_policy_and_the_clicked_pane() {
        use crate::session::{IbmKind, Status};

        let mut workspace = Workspace::default();
        workspace.set_test_ai_configuration(
            serde_json::from_value(serde_json::json!({
                "profiles": {
                    "profiles": [{"id": 1, "name": "Fixture", "endpoint": "127.0.0.1:12345", "model": "fixture"}],
                    "default": 1,
                    "last_id": 1
                }
            }))
            .unwrap(),
        );
        let (owner, _) = workspace.open_window(false);
        let grid = GridSize::default();
        let source = workspace.store_session(Session::disconnected_ibm(IbmKind::Tn3270e), grid, 14);
        let sibling =
            workspace.store_session(Session::disconnected_ibm(IbmKind::Tn3270e), grid, 14);
        workspace.add_tab(owner, source);
        let tab = workspace.active_tab_mut(owner).unwrap();
        let (second, _) = tab
            .grid
            .split(pane_grid::Axis::Vertical, tab.focused, sibling)
            .unwrap();
        let pane = workspace.sessions.get_mut(&source).unwrap();
        pane.local_security = Some(crate::session_security::LocalSettings::default());
        pane.frame = crate::terminal_view::selection_test_frame(pane.frame.screen.clone());
        Arc::get_mut(&mut pane.frame).unwrap().status = Status::Running;

        let policy = |features: &str| {
            crate::clipboard_policy::parse(&format!(
                "[meta]\nschema-version = 1\n[[rule]]\n[rule.features]\n{features}"
            ))
            .unwrap()
        };
        let entries = |workspace: &Workspace, section, session| {
            workspace.terminal_context_entries(
                owner,
                section,
                session,
                &workspace.sessions[&session],
                false,
            )
        };
        let run = Item(Agent(chrome::Action::TerminalAgent));
        let ask = Item(Agent(chrome::Action::TerminalAgentAsk));
        let plan = Item(Agent(chrome::Action::TerminalAgentPlan));
        let ai = Branch(Section::Ai);
        workspace.clipboard.policy = policy("ai-chat='deny'");
        assert!(entries(&workspace, Section::Root, source).contains(&ai));
        assert!(!entries(&workspace, Section::Root, sibling).contains(&ai));
        assert_eq!(entries(&workspace, Section::Ai, source), [run, ask, plan]);

        workspace.clipboard.policy = policy("ai-agent='deny'\nai-planning='deny'");
        assert_eq!(
            entries(&workspace, Section::Ai, source),
            [Item(Ai(super::super::terminal_ai::Action::SummarizeRecent))]
        );
        workspace.clipboard.policy = policy("ai-chat='deny'\nai-planning='deny'");
        assert_eq!(entries(&workspace, Section::Ai, source), [run, ask]);
        let _ =
            workspace.update_terminal_context(owner, Message::Open(source, Point::ORIGIN, None));
        let request = workspace.terminal_context.as_ref().unwrap().request;
        let _ =
            workspace.update_terminal_context(owner, Message::Point(request, Section::Root, ai));

        workspace.clipboard.policy = policy("ai-chat='deny'\nai-agent='deny'");
        assert_eq!(entries(&workspace, Section::Ai, source), [plan]);
        let denied =
            workspace.update_terminal_context(owner, Message::Invoke(request, Section::Ai, run));
        assert_eq!(denied.units(), 0);
        assert!(workspace.terminal_agents.is_empty());

        workspace.active_tab_mut(owner).unwrap().focused = second;
        let opened =
            workspace.update_terminal_context(owner, Message::Invoke(request, Section::Ai, plan));
        assert!(opened.units() > 0);
        assert!(!workspace.terminal_agents.is_empty());
        assert!(workspace.terminal_context.is_none());
        assert_eq!(
            workspace.windows[&owner].tab().unwrap().focused_session(),
            Some(source)
        );
    }

    #[test]
    fn multi_exec_targets_the_clicked_pane_and_allows_leaving_under_policy_deny() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let source = pane(&mut workspace);
        let sibling = pane(&mut workspace);
        workspace.add_tab(owner, source);
        let tab = workspace.active_tab_mut(owner).unwrap();
        let (second, _) = tab
            .grid
            .split(pane_grid::Axis::Vertical, tab.focused, sibling)
            .unwrap();
        assert!(
            workspace
                .terminal_context_entries(
                    owner,
                    Section::Extras,
                    source,
                    &workspace.sessions[&source],
                    false
                )
                .ends_with(&[Item(Broadcast), Item(MultiExec)])
        );

        let toggle = |workspace: &mut Workspace| {
            let _ = workspace
                .update_terminal_context(owner, Message::Open(source, Point::ORIGIN, None));
            let request = workspace.terminal_context.as_ref().unwrap().request;
            let _ = workspace.update_terminal_context(
                owner,
                Message::Point(request, Section::Root, Branch(Section::Extras)),
            );
            workspace.active_tab_mut(owner).unwrap().focused = second;
            let _ = workspace.update_terminal_context(
                owner,
                Message::Invoke(request, Section::Extras, Item(MultiExec)),
            );
        };
        toggle(&mut workspace);
        assert!(workspace.multi_exec_checked(owner, MultiExecAction::PaneAt(source)));
        assert!(!workspace.multi_exec_checked(owner, MultiExecAction::PaneAt(sibling)));

        workspace.clipboard.policy.multi_exec_allowed = false;
        let tab = workspace.windows[&owner].tab().unwrap();
        let pane = &workspace.sessions[&source];
        assert!(!workspace.terminal_context_entry_enabled(
            Item(Broadcast),
            owner,
            source,
            tab,
            pane
        ));
        toggle(&mut workspace);
        assert!(!workspace.multi_exec_checked(owner, MultiExecAction::PaneAt(source)));
        toggle(&mut workspace);
        assert!(!workspace.multi_exec_checked(owner, MultiExecAction::PaneAt(source)));
    }

    #[test]
    fn an_action_from_a_hidden_pane_menu_cannot_change_the_replacement() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let former = pane(&mut workspace);
        let current = pane(&mut workspace);
        workspace.add_tab(owner, former);
        let _ =
            workspace.update_terminal_context(owner, Message::Open(former, Point::ORIGIN, None));
        let request = workspace.terminal_context.as_ref().unwrap().request;
        let _ = workspace.update_terminal_context(
            owner,
            Message::Point(request, Section::Root, Branch(Section::Extras)),
        );
        let _ = workspace.update_terminal_context(
            owner,
            Message::Point(request, Section::Extras, Branch(Section::Font)),
        );
        workspace.add_tab(owner, current);
        let _ = workspace.update_terminal_context(
            owner,
            Message::Invoke(request, Section::Font, Item(FontIncrease)),
        );

        assert_eq!(workspace.sessions[&former].font, 14);
        assert_eq!(workspace.sessions[&current].font, 14);
        assert!(workspace.terminal_context.is_none());
    }
    #[test]
    fn an_earlier_menu_cannot_invoke_or_dismiss_the_replacement() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = pane(&mut workspace);
        workspace.add_tab(owner, session);
        let open = Message::Open(session, Point::ORIGIN, None);
        let _ = workspace.update_terminal_context(owner, open.clone());
        let request = workspace.terminal_context.as_ref().unwrap().request;
        let _ = workspace.update_terminal_context(owner, open);
        let _ = workspace.update_terminal_context(owner, Message::Dismiss(request));
        let _ = workspace.update_terminal_context(
            owner,
            Message::Invoke(request, Section::Root, Item(SetupShellIntegration)),
        );

        assert!(workspace.terminal_context.is_some());
        assert!(workspace.owner_dialog(owner).is_none());
    }
}
