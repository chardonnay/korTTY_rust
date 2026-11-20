//! A pane-local Agent composer using the regular setup and review path.

use std::fmt;
use std::sync::Arc;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    Id, button, column, container, opaque, operation, pick_list, row, stack, text, text_input,
};
use iced::{Alignment, Element, Length, Task, window};

use crate::terminal_agent::history::{self as store, Snapshot};
use crate::terminal_agent::shortcut::shortcut_character;

use super::{
    ClipboardPolicy, Desktop, LiveBinding, MAX_GOAL, MAX_RUNS, Mode, ProfileChoice, Run, Workspace,
    history, palette,
};

#[derive(Clone)]
pub struct Message {
    owner: window::Id,
    field: Id,
    event: Event,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Agent Spotlight")
    }
}

#[derive(Clone)]
enum Event {
    Edit(String),
    Profile(Choice),
    Loaded(Arc<Result<Snapshot, String>>),
    Earlier,
    Later,
    Submit,
    Close,
}

#[derive(Clone, Eq, PartialEq)]
enum Choice {
    PaneDefault,
    Profile(ProfileChoice),
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PaneDefault => formatter.write_str("Terminal default"),
            Self::Profile(profile) => fmt::Display::fmt(profile, formatter),
        }
    }
}

pub(super) struct Composer {
    tab: u64,
    source: LiveBinding,
    field: Id,
    prompt: String,
    history: Vec<String>,
    recalled: Option<usize>,
    draft: String,
    profiles: Vec<Choice>,
    profile: Choice,
    choosing_profile: bool,
    notice: Option<String>,
}

impl Composer {
    fn message(&self, owner: window::Id, event: Event) -> Desktop {
        Desktop::AgentSpotlight(Message {
            owner,
            field: self.field.clone(),
            event,
        })
    }

    fn edit(&mut self, value: String) {
        self.prompt = value;
        self.recalled = None;
        self.draft.clear();
        self.notice = None;
        self.choosing_profile = false;
    }

    fn browse(&mut self, earlier: bool) -> Task<Desktop> {
        if self.choosing_profile {
            let active = self
                .profiles
                .iter()
                .position(|choice| *choice == self.profile)
                .unwrap_or(0);
            let next = if earlier {
                active.saturating_sub(1)
            } else {
                (active + 1).min(self.profiles.len() - 1)
            };
            self.profile = self.profiles[next].clone();
            self.notice = None;
            return Task::none();
        }
        if self.history.is_empty() {
            return Task::none();
        }
        self.recalled = match self.recalled {
            Some(index) if earlier => Some((index + 1).min(self.history.len() - 1)),
            Some(index) => index.checked_sub(1),
            None if earlier => {
                self.draft.clone_from(&self.prompt);
                Some(0)
            }
            None => return Task::none(),
        };
        self.prompt.clone_from(
            self.recalled
                .map_or(&self.draft, |index| &self.history[index]),
        );
        self.notice = None;
        operation::move_cursor_to_end(self.field.clone())
    }

    fn view(&self, owner: window::Id) -> Element<'_, Desktop> {
        let field = self.field.clone();
        let input = text_input("Ask the terminal agent…", &self.prompt)
            .id(field.clone())
            .on_input(move |value| {
                Desktop::AgentSpotlight(Message {
                    owner,
                    field: field.clone(),
                    event: Event::Edit(value),
                })
            })
            .on_submit(self.message(owner, Event::Submit));
        let input = crate::settings_search::input(
            input,
            self.message(owner, Event::Earlier),
            self.message(owner, Event::Later),
            Some(self.message(owner, Event::Close)),
            None,
        );
        let field = self.field.clone();
        let profiles = pick_list(
            self.profiles.as_slice(),
            Some(&self.profile),
            move |profile| {
                Desktop::AgentSpotlight(Message {
                    owner,
                    field: field.clone(),
                    event: Event::Profile(profile),
                })
            },
        )
        .width(Length::Fill);
        column![
            text("Ask in Spotlight").size(15),
            input,
            profiles,
            row![
                button("Start").on_press_maybe(
                    (!self.prompt.trim().is_empty()).then(|| self.message(owner, Event::Submit))
                ),
                button("Cancel").on_press(self.message(owner, Event::Close)),
                text(if self.choosing_profile {
                    "↑/↓ Choose profile · Tab Prompt"
                } else if self.history.is_empty() {
                    "Tab Profile"
                } else {
                    "↑/↓ Recent prompts · Tab Profile"
                })
                .size(12),
            ]
            .align_y(Alignment::Center)
            .spacing(6)
            .wrap(),
            self.notice.as_ref().map(|notice| text(notice).size(12)),
        ]
        .spacing(7)
        .into()
    }
}

impl Workspace {
    fn agent_spotlight_source(&self, owner: window::Id) -> Option<(u64, LiveBinding)> {
        let tab = self.windows.get(&owner)?.tab()?;
        let session = tab.focused_session()?;
        let pane = self.sessions.get(&session)?;
        if self.exiting
            || self.terminal_agents.runs.len() >= MAX_RUNS
            || self.terminal_agents.owns_session(session)
            || !self.current_targets(owner, tab.id, &[session])
            || self.hosted_close_prompt(owner).is_some()
            || self.project_panel_open(owner)
            || pane.frame.screen.alternate_screen
            || pane.frame.coding_agent_visible()
            || self.agent_policy(pane, Mode::Run).is_err()
        {
            return None;
        }
        Some((
            tab.id,
            LiveBinding {
                session,
                stream: pane.stream_generation,
                private: tab.incognito,
            },
        ))
    }

    pub(in crate::workspace) fn agent_spotlight_available(&self, owner: window::Id) -> bool {
        self.agent_spotlight_source(owner).is_some()
    }

    pub(in crate::workspace) fn open_agent_spotlight(
        &mut self,
        owner: window::Id,
    ) -> Task<Desktop> {
        let Some((tab, source)) = self.agent_spotlight_source(owner) else {
            return Task::none();
        };
        if let Some(composer) = self.terminal_agents.spotlights.get(&owner)
            && (composer.tab, composer.source) == (tab, source)
        {
            return crate::settings_search::focus_if_present(composer.field.clone());
        }
        let policy = ClipboardPolicy::load();
        let Some(configuration) = self
            .ai_configuration()
            .filter(|_| super::policy::agent_mode(&policy, Mode::Run).is_ok())
        else {
            return Task::none();
        };
        let available = self.live_agent_profile_choices(
            &configuration,
            &policy,
            Mode::Run,
            source.private,
            false,
        );
        if available.is_empty() {
            self.notice(
                owner,
                "Choose an available agent profile in AI Manager.".into(),
            );
            return Task::none();
        }
        let mut profiles = vec![Choice::PaneDefault];
        profiles.extend(available.into_iter().map(Choice::Profile));
        let field = Id::unique();
        self.terminal_agents.spotlights.insert(
            owner,
            Composer {
                tab,
                source,
                field: field.clone(),
                prompt: String::new(),
                history: Vec::new(),
                recalled: None,
                draft: String::new(),
                profiles,
                profile: Choice::PaneDefault,
                choosing_profile: false,
                notice: None,
            },
        );
        let focus = crate::settings_search::focus_if_present(field.clone());
        if !source.private
            && let Some(root) = self.agent_history_root()
        {
            return Task::batch([
                focus,
                history::file_task(
                    move || store::list(&root, false),
                    move |result| {
                        Desktop::AgentSpotlight(Message {
                            owner,
                            field: field.clone(),
                            event: Event::Loaded(Arc::new(result)),
                        })
                    },
                ),
            ]);
        }
        focus
    }

    pub(in crate::workspace) fn revalidate_agent_spotlights(&mut self) {
        if !self.terminal_agents.spotlights.is_empty() {
            let mut composers = std::mem::take(&mut self.terminal_agents.spotlights);
            composers.retain(|&owner, composer| {
                self.agent_spotlight_source(owner) == Some((composer.tab, composer.source))
            });
            self.terminal_agents.spotlights = composers;
        }
    }

    pub(in crate::workspace) fn update_agent_spotlight(
        &mut self,
        message: Message,
    ) -> Task<Desktop> {
        let owner = message.owner;
        let live = self.agent_spotlight_source(owner);
        let Some(composer) = self
            .terminal_agents
            .spotlights
            .get_mut(&owner)
            .filter(|composer| composer.field == message.field)
        else {
            return Task::none();
        };
        if live != Some((composer.tab, composer.source)) || matches!(message.event, Event::Close) {
            self.terminal_agents.spotlights.remove(&owner);
            return Task::none();
        }
        match message.event {
            Event::Edit(value)
                if value.len() <= MAX_GOAL && value.chars().all(shortcut_character) =>
            {
                composer.edit(value)
            }
            Event::Profile(choice) if composer.profiles.contains(&choice) => {
                composer.profile = choice;
                composer.choosing_profile = false;
                composer.notice = None;
                return crate::settings_search::focus_if_present(composer.field.clone());
            }
            Event::Loaded(result) if !composer.source.private => match result.as_ref() {
                Ok(snapshot) => {
                    composer.history = snapshot
                        .entries
                        .iter()
                        .filter(|entry| entry.goal.chars().all(shortcut_character))
                        .map(|entry| entry.goal.clone())
                        .collect();
                }
                Err(_) => composer.notice = Some("Could not load recent prompts.".into()),
            },
            Event::Earlier | Event::Later => {
                return composer.browse(matches!(message.event, Event::Earlier));
            }
            Event::Submit if !composer.prompt.trim().is_empty() => {
                let source = composer.source;
                let profile = composer.profile.clone();
                let prompt = composer.prompt.trim().to_owned();
                match self.build_spotlight_run(source, &profile, prompt) {
                    Ok(run) => {
                        self.terminal_agents.spotlights.remove(&owner);
                        return self.open_agent_result(owner, Ok(run));
                    }
                    Err(error) => {
                        if let Some(composer) = self.terminal_agents.spotlights.get_mut(&owner) {
                            composer.notice = Some(error);
                            return crate::settings_search::focus_if_present(
                                composer.field.clone(),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn build_spotlight_run(
        &self,
        source: LiveBinding,
        choice: &Choice,
        prompt: String,
    ) -> Result<(Run, bool), String> {
        let profile = match choice {
            Choice::PaneDefault => None,
            Choice::Profile(profile) => Some(profile),
        };
        let mut run = self.build_agent(
            source.session,
            Mode::Run,
            None,
            profile.map(|profile| profile.id),
            false,
        )?;
        if let Some(profile) = profile {
            if !matches!(&run.purpose, super::Purpose::Agent(current) if current == profile) {
                return Err("The selected agent profile changed. Open Spotlight again.".into());
            }
            // The composer already made the profile choice. Model and command approvals still use
            // the ordinary Run review.
            run.profile_pending = false;
        }
        run.goal = prompt;
        let direct = !run.profile_pending
            && self
                .ai_configuration()
                .is_some_and(|config| !config.show_agent_setup());
        Ok((run, direct))
    }

    pub(in crate::workspace) fn agent_spotlight_key(
        &mut self,
        owner: window::Id,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        let composer = self.terminal_agents.spotlights.get_mut(&owner)?;
        // A key ignored by the field or queued before its first layout must stay out of the PTY.
        if modifiers.is_empty() && *key == Key::Named(Named::Escape) {
            self.terminal_agents.spotlights.remove(&owner);
        } else {
            if (modifiers.is_empty() || modifiers == Modifiers::SHIFT)
                && *key == Key::Named(Named::Tab)
            {
                composer.choosing_profile = !composer.choosing_profile;
            }
            return Some(crate::settings_search::focus_if_present(
                composer.field.clone(),
            ));
        }
        Some(Task::none())
    }

    pub(in crate::workspace) fn agent_spotlight_view<'a>(
        &'a self,
        owner: window::Id,
        session: crate::session::SessionId,
        terminal: Element<'a, Desktop>,
    ) -> Element<'a, Desktop> {
        let Some(composer) = self
            .terminal_agents
            .spotlights
            .get(&owner)
            .filter(|composer| composer.source.session == session)
        else {
            return terminal;
        };
        let theme = self.agent_panel_palette(self.pane_theme_id(session));
        let card = palette::panel(
            theme,
            container(composer.view(owner))
                .width(Length::Fill)
                .max_width(460)
                .padding(12),
        );
        stack![
            terminal,
            container(opaque(card)).center(Length::Fill).padding(8)
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

#[cfg(test)]
mod tests;
