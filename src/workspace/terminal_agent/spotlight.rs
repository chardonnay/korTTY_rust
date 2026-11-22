//! A pane-local Agent composer using the regular setup and review path.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{
    Id, button, column, container, operation, pick_list, row, stack, text, text_input,
};
use iced::{Alignment, Element, Length, Task, window};

use crate::terminal_agent::history::{self as store, Snapshot};
use crate::terminal_agent::shortcut::shortcut_character;

use super::{
    ClipboardPolicy, Desktop, LiveBinding, MAX_GOAL, MAX_RUNS, Mode, ProfileChoice, Run, Workspace,
    history, palette,
};

mod motion;
pub(super) use motion::scale_card;

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
    opened: Instant,
}

#[derive(Clone, Copy)]
pub(super) struct Submission {
    owner: window::Id,
    tab: u64,
    source: LiveBinding,
}

pub(super) struct Departure {
    submission: Submission,
    started: Instant,
    into_pill: bool,
}

impl Departure {
    fn frame(&self, now: Instant) -> Option<motion::Frame> {
        motion::Frame::submitted(now.saturating_duration_since(self.started), self.into_pill)
    }
}

fn submitted_card<'a>(prompt: &'a str) -> Element<'a, Desktop> {
    container(column![text("Ask in Spotlight").size(15), text(prompt).size(13),].spacing(7))
        .height(104)
        .into()
}

pub(super) fn preview_frame(age: std::time::Duration) -> Option<motion::Frame> {
    let hold = std::time::Duration::from_millis(320);
    if age < hold {
        Some(motion::Frame::opening(age))
    } else {
        motion::Frame::submitted(age - hold, true)
    }
}

pub(super) fn preview<'a>(
    terminal: Element<'a, Desktop>,
    frame: motion::Frame,
    prompt: &'a str,
) -> Element<'a, Desktop> {
    let card = palette::faded_panel(
        None,
        container(submitted_card(prompt))
            .width(Length::Fill)
            .max_width(460)
            .padding(12),
        frame.opacity,
    );
    stack![terminal, motion::layer(card, frame, false)]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
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

    fn view(&self, owner: window::Id, opacity: f32) -> Element<'_, Desktop> {
        let field = self.field.clone();
        let input = text_input("Ask the terminal agent…", &self.prompt)
            .id(field.clone())
            .style(move |theme, status| {
                let mut style = text_input::default(theme, status);
                style.background = style.background.scale_alpha(opacity);
                style.border.color = style.border.color.scale_alpha(opacity);
                style.icon = style.icon.scale_alpha(opacity);
                style.placeholder = style.placeholder.scale_alpha(opacity);
                style.value = style.value.scale_alpha(opacity);
                style.selection = style.selection.scale_alpha(opacity);
                style
            })
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
        .width(Length::Fill)
        .style(move |theme, status| {
            let mut style = pick_list::default(theme, status);
            style.background = style.background.scale_alpha(opacity);
            style.border.color = style.border.color.scale_alpha(opacity);
            style.text_color = style.text_color.scale_alpha(opacity);
            style.handle_color = style.handle_color.scale_alpha(opacity);
            style.placeholder_color = style.placeholder_color.scale_alpha(opacity);
            style
        });
        let button_style = move |theme: &iced::Theme, status| {
            let mut style = button::primary(theme, status);
            style.background = style.background.map(|bg| bg.scale_alpha(opacity));
            style.border.color = style.border.color.scale_alpha(opacity);
            style.shadow.color = style.shadow.color.scale_alpha(opacity);
            style.text_color = style.text_color.scale_alpha(opacity);
            style
        };
        column![
            text("Ask in Spotlight").size(15),
            input,
            profiles,
            row![
                button("Start").style(button_style).on_press_maybe(
                    (!self.prompt.trim().is_empty()).then(|| self.message(owner, Event::Submit))
                ),
                button("Cancel")
                    .style(button_style)
                    .on_press(self.message(owner, Event::Close)),
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
    fn spotlight_binding(&self, owner: window::Id) -> Option<(u64, LiveBinding)> {
        let tab = self.windows.get(&owner)?.tab()?;
        let session = tab.focused_session()?;
        let pane = self.sessions.get(&session)?;
        if self.exiting
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

    fn agent_spotlight_source(&self, owner: window::Id) -> Option<(u64, LiveBinding)> {
        let (tab, source) = self.spotlight_binding(owner)?;
        (self.terminal_agents.runs.len() < MAX_RUNS
            && self
                .terminal_agents
                .check_live_run_slot(source, false)
                .is_ok())
        .then_some((tab, source))
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
        self.terminal_agents.spotlight_exits.remove(&owner);
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
                opened: Instant::now(),
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
        if !self.terminal_agents.spotlight_exits.is_empty() {
            let now = Instant::now();
            let mut exits = std::mem::take(&mut self.terminal_agents.spotlight_exits);
            if self.window_settings.agent_animations() {
                exits.retain(|&owner, departure| {
                    let submission = departure.submission;
                    departure.frame(now).is_some()
                        && self.spotlight_binding(owner)
                            == Some((submission.tab, submission.source))
                });
                self.terminal_agents.spotlight_exits = exits;
            }
        }
    }

    pub(super) fn show_spotlight_departure(&mut self, submission: Submission, into_pill: bool) {
        if self.window_settings.agent_animations()
            && self.spotlight_binding(submission.owner) == Some((submission.tab, submission.source))
            && !self
                .terminal_agents
                .spotlights
                .contains_key(&submission.owner)
        {
            self.terminal_agents.spotlight_exits.insert(
                submission.owner,
                Departure {
                    submission,
                    started: Instant::now(),
                    into_pill,
                },
            );
        }
    }

    pub(super) fn spotlight_animating(&self, now: Instant) -> bool {
        self.window_settings.agent_animations()
            && (self
                .terminal_agents
                .spotlights
                .values()
                .any(|composer| now.saturating_duration_since(composer.opened) < motion::OPEN)
                || !self.terminal_agents.spotlight_exits.is_empty())
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
                let submission = Submission {
                    owner,
                    tab: composer.tab,
                    source: composer.source,
                };
                let profile = composer.profile.clone();
                let prompt = composer.prompt.trim().to_owned();
                match self.build_spotlight_run(submission.source, &profile, prompt) {
                    Ok((mut run, direct)) => {
                        self.terminal_agents.spotlights.remove(&owner);
                        self.show_spotlight_departure(submission, false);
                        if direct {
                            run.spotlight_submission = Some(submission);
                        }
                        return self.open_agent_result(owner, Ok((run, direct)));
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
        self.terminal_agents.check_live_run_slot(source, false)?;
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
        let now = Instant::now();
        let animations = self.window_settings.agent_animations();
        let (content, frame, interactive) = if let Some(composer) = self
            .terminal_agents
            .spotlights
            .get(&owner)
            .filter(|composer| composer.source.session == session)
        {
            let age = if animations {
                now.saturating_duration_since(composer.opened)
            } else {
                motion::OPEN
            };
            let frame = motion::Frame::opening(age);
            (composer.view(owner, frame.opacity), frame, true)
        } else if let Some(frame) = self
            .terminal_agents
            .spotlight_exits
            .get(&owner)
            .filter(|departure| animations && departure.submission.source.session == session)
            .and_then(|departure| departure.frame(now))
        {
            (submitted_card("Prompt submitted"), frame, false)
        } else {
            return terminal;
        };
        let theme = self.agent_panel_palette(self.pane_theme_id(session));
        let card = palette::faded_panel(
            theme,
            container(content)
                .width(Length::Fill)
                .max_width(460)
                .padding(12),
            frame.opacity,
        );
        stack![terminal, motion::layer(card, frame, interactive)]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

#[cfg(test)]
mod tests;
