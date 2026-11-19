//! Local alias choices and asynchronous prompt history.

use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{self, Id, button, canvas, column, row, scrollable, text};
use iced::{Element, Length, Size, Task, window};

use crate::horizontal_divider::{self, Grow};
use crate::session::{SessionId, ShortcutPrompt};
use crate::terminal_agent::Mode;
use crate::terminal_agent::history::{self as store, Entry, Snapshot};
use crate::terminal_agent::shortcut::shortcut_character;

use super::{Capture, Desktop, HISTORY_POPUP_HEIGHT, HISTORY_POPUP_WIDTH, Workspace};
use crate::workspace::terminal_agent::history;

static NEXT_COMPLETION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    owner: window::Id,
    session: SessionId,
    stream: u64,
    prompt: ShortcutPrompt,
    nonce: u64,
    private: bool,
    planning_allowed: bool,
}

#[derive(Clone)]
pub enum Message {
    Choose(Ticket, usize),
    Loaded(Ticket, Arc<Result<Snapshot, String>>),
    ResizeHistory(Ticket, HistoryResize),
}

#[derive(Clone, Copy)]
pub enum HistoryResize {
    Width(f32),
    Height(f32),
    Finish,
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Agent shortcut completion")
    }
}

pub(super) struct Completion {
    ticket: Ticket,
    options: Options,
    selected: usize,
    scroll: Id,
    history_size: HistorySize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct HistorySize {
    width: u16,
    height: u16,
}

impl HistorySize {
    fn new(width: Option<u16>, height: Option<u16>) -> Self {
        Self {
            width: width.unwrap_or(460),
            height: height.unwrap_or(260),
        }
    }

    fn pane_limit(bounds: Size) -> Size {
        // Leave the draft and part of the terminal visible in narrow or short splits.
        Size::new((bounds.width - 12.0).max(0.0), bounds.height * 0.75)
    }

    fn apply(&mut self, resize: HistoryResize) {
        match resize {
            HistoryResize::Width(width) if width.is_finite() => {
                self.width = width.round().clamp(
                    f32::from(*HISTORY_POPUP_WIDTH.start()),
                    f32::from(*HISTORY_POPUP_WIDTH.end()),
                ) as u16;
            }
            HistoryResize::Height(height) if height.is_finite() => {
                self.height = height.round().clamp(
                    f32::from(*HISTORY_POPUP_HEIGHT.start()),
                    f32::from(*HISTORY_POPUP_HEIGHT.end()),
                ) as u16;
            }
            _ => {}
        }
    }
}

enum Options {
    Commands(Vec<String>),
    Loading,
    History(Vec<Entry>),
    Notice(&'static str),
}

impl Options {
    fn len(&self) -> usize {
        match self {
            Self::Commands(variants) => variants.len(),
            Self::History(entries) => entries.len(),
            Self::Loading | Self::Notice(_) => 0,
        }
    }
}

impl Completion {
    pub(super) fn view(
        &self,
        owner: window::Id,
        private: bool,
        bounds: Size,
    ) -> Option<Element<'_, Desktop>> {
        if self.ticket.owner != owner || self.ticket.private != private {
            return None;
        }
        let mut list = column![].spacing(4);
        let title = match &self.options {
            Options::Commands(variants) => {
                for (index, variant) in variants.iter().enumerate() {
                    list = list.push(self.button(index, variant.clone()));
                }
                "Agent shortcut variants"
            }
            Options::History(entries) => {
                for (index, entry) in entries.iter().enumerate() {
                    list = list.push(self.button(index, history::label(entry)));
                }
                return Some(self.history_view(list, bounds));
            }
            Options::Loading => return Some(text("Loading recent agent prompts…").size(13).into()),
            Options::Notice(notice) => return Some(text(*notice).size(13).into()),
        };
        Some(
            column![
                text(title).size(13),
                scrollable(list)
                    .id(self.scroll.clone())
                    .height(Length::Fixed(112.0)),
                text("Use arrow keys to choose, Enter to insert, or Esc to dismiss.").size(12),
            ]
            .spacing(4)
            .into(),
        )
    }

    fn history_view<'a>(
        &self,
        list: widget::Column<'a, Desktop>,
        bounds: Size,
    ) -> Element<'a, Desktop> {
        let limit = HistorySize::pane_limit(bounds);
        let width = f32::from(self.history_size.width).min(limit.width);
        let height = f32::from(self.history_size.height).min(limit.height);
        let ticket = self.ticket;
        let finish = Desktop::AgentShortcut(Message::ResizeHistory(ticket, HistoryResize::Finish));
        let top = (height >= f32::from(*HISTORY_POPUP_HEIGHT.start())).then(|| {
            canvas(
                horizontal_divider::program(
                    height,
                    Grow::Up,
                    move |height| {
                        Desktop::AgentShortcut(Message::ResizeHistory(
                            ticket,
                            HistoryResize::Height(height.min(limit.height.floor())),
                        ))
                    },
                    Some(finish.clone()),
                )
                .scoped(ticket.nonce),
            )
            .width(Length::Fill)
            .height(horizontal_divider::WIDTH)
        });
        let right = (width >= f32::from(*HISTORY_POPUP_WIDTH.start())).then(|| {
            canvas(
                horizontal_divider::program(
                    width,
                    Grow::Right,
                    move |width| {
                        Desktop::AgentShortcut(Message::ResizeHistory(
                            ticket,
                            HistoryResize::Width(width.min(limit.width.floor())),
                        ))
                    },
                    Some(finish),
                )
                .scoped(ticket.nonce),
            )
            .width(horizontal_divider::WIDTH)
            .height(Length::Fill)
        });
        let body = column![
            text("Recent agent prompts").size(13),
            scrollable(list)
                .id(self.scroll.clone())
                .height(Length::Fill),
            text("Use arrow keys to choose, Enter to insert, or Esc to dismiss.").size(12),
        ]
        .spacing(4)
        .width(Length::Fill)
        .height(Length::Fill);
        column![top, row![body, right].height(Length::Fill)]
            .width(width)
            .height(height)
            .into()
    }

    fn button(&self, index: usize, label: String) -> Element<'static, Desktop> {
        let selected = self.selected == index;
        button(text(label).size(13))
            .on_press(Desktop::AgentShortcut(Message::Choose(self.ticket, index)))
            .style(move |theme, status| {
                if selected {
                    button::primary(theme, status)
                } else {
                    button::secondary(theme, status)
                }
            })
            .into()
    }

    fn move_selection(&mut self, backwards: bool) -> Task<Desktop> {
        let count = self.options.len();
        if count < 2 {
            return Task::none();
        }
        self.selected = if backwards {
            (self.selected + count - 1) % count
        } else {
            (self.selected + 1) % count
        };
        if matches!(self.options, Options::History(_)) {
            widget::operation::snap_to(
                self.scroll.clone(),
                widget::operation::RelativeOffset {
                    x: None,
                    y: Some(self.selected as f32 / (count - 1) as f32),
                },
            )
        } else {
            Task::none()
        }
    }
}

impl Capture {
    pub(super) fn refresh_completion_policy(&mut self, planning_allowed: bool) {
        if self
            .completion
            .as_ref()
            .is_some_and(|completion| completion.ticket.planning_allowed != planning_allowed)
        {
            self.completion = None;
        }
    }

    pub(super) fn completion_key(
        &mut self,
        owner: window::Id,
        stream: u64,
        private: bool,
        key: &Key,
        modifiers: Modifiers,
    ) -> Option<Task<Desktop>> {
        let completion = self.completion.as_mut()?;
        if completion.ticket.owner != owner
            || completion.ticket.stream != stream
            || completion.ticket.private != private
        {
            self.completion = None;
            return None;
        }
        let backwards = modifiers == Modifiers::SHIFT && *key == Key::Named(Named::Tab)
            || modifiers.is_empty() && *key == Key::Named(Named::ArrowUp);
        let forwards =
            modifiers.is_empty() && matches!(key, Key::Named(Named::Tab | Named::ArrowDown));
        if backwards || forwards {
            return Some(completion.move_selection(backwards));
        }
        if modifiers.is_empty() && *key == Key::Named(Named::Enter) {
            let selected = completion.selected;
            self.apply_completion(selected);
            return Some(Task::none());
        }
        self.completion = None;
        (modifiers.is_empty() && *key == Key::Named(Named::Escape)).then(Task::none)
    }

    fn apply_completion(&mut self, index: usize) {
        let Some(completion) = &self.completion else {
            return;
        };
        let replacement = match &completion.options {
            Options::Commands(variants) => variants.get(index).map(|variant| format!("{variant} ")),
            Options::History(entries) => self
                .aliases
                .history_prefix(self.line.text())
                .zip(entries.get(index))
                .map(|(head, entry)| format!("{head} {}", entry.goal)),
            Options::Loading | Options::Notice(_) => None,
        };
        if let Some(replacement) = replacement {
            self.line.replace_with_completion(&replacement);
            self.completion = None;
        }
    }
}

impl Workspace {
    pub(super) fn begin_agent_completion(
        &self,
        owner: window::Id,
        session: SessionId,
        capture: &mut Capture,
    ) -> Option<Task<Desktop>> {
        if capture.line.text().is_empty() {
            return None;
        }
        let (actual_owner, pane, tab) = self.agent_pane(session)?;
        if actual_owner != owner || tab.focused_session() != Some(session) {
            return None;
        }
        let planning_allowed = self.clipboard.policy.ai_planning_allowed();
        let allowed = |command: &str| {
            capture
                .aliases
                .alias_mode(command)
                .is_some_and(|mode| mode != Mode::Plan || planning_allowed)
        };
        let variants: Vec<_> = capture
            .aliases
            .completion_variants(capture.line.text())
            .into_iter()
            .filter(|command| allowed(command))
            .collect();
        let ticket = Ticket {
            owner,
            session,
            stream: pane.stream_generation,
            prompt: capture.prompt,
            nonce: NEXT_COMPLETION.fetch_add(1, Ordering::Relaxed),
            private: tab.incognito,
            planning_allowed,
        };
        let (options, task) = if !variants.is_empty() {
            (Options::Commands(variants), Task::none())
        } else if capture
            .aliases
            .history_prefix(capture.line.text())
            .is_some_and(allowed)
        {
            if ticket.private {
                (
                    Options::Notice("Private terminals have no recent prompt history."),
                    Task::none(),
                )
            } else if let Some(root) = self.agent_history_root() {
                (
                    Options::Loading,
                    history::file_task(
                        move || store::list(&root, false),
                        move |result| {
                            Desktop::AgentShortcut(Message::Loaded(ticket, Arc::new(result)))
                        },
                    ),
                )
            } else {
                (
                    Options::Notice("Recent prompt history is unavailable."),
                    Task::none(),
                )
            }
        } else {
            return None;
        };
        capture.completion = Some(Completion {
            ticket,
            options,
            selected: 0,
            scroll: Id::unique(),
            history_size: HistorySize::new(
                self.window_settings.agent_history_popup_width,
                self.window_settings.agent_history_popup_height,
            ),
        });
        Some(task)
    }

    pub(in crate::workspace) fn update_agent_shortcut(
        &mut self,
        message: Message,
    ) -> Task<Desktop> {
        let ticket = match &message {
            Message::Choose(ticket, _)
            | Message::Loaded(ticket, _)
            | Message::ResizeHistory(ticket, _) => *ticket,
        };
        let live = self.agent_completion_live(ticket);
        let Some(capture) = self
            .sessions
            .get_mut(&ticket.session)
            .and_then(|pane| pane.agent_shortcut.as_mut())
            .filter(|capture| {
                capture
                    .completion
                    .as_ref()
                    .is_some_and(|state| state.ticket == ticket)
            })
        else {
            return Task::none();
        };
        if !live {
            capture.completion = None;
            return Task::none();
        }
        match message {
            Message::Choose(_, index) => capture.apply_completion(index),
            Message::Loaded(_, result) if !ticket.private => {
                if let Some(completion) = capture.completion.as_mut()
                    && matches!(completion.options, Options::Loading)
                {
                    completion.options = if let Ok(snapshot) = result.as_ref() {
                        let entries: Vec<_> = snapshot
                            .entries
                            .iter()
                            .filter(|entry| {
                                (ticket.planning_allowed || entry.kind != store::Kind::Plan)
                                    && entry.goal.chars().all(shortcut_character)
                            })
                            .cloned()
                            .collect();
                        if entries.is_empty() {
                            Options::Notice("No single-line recent prompts.")
                        } else {
                            Options::History(entries)
                        }
                    } else {
                        Options::Notice("Could not load recent prompts.")
                    };
                }
            }
            Message::Loaded(..) => {}
            Message::ResizeHistory(_, resize) => {
                if let Some(completion) = capture.completion.as_mut()
                    && !ticket.private
                    && matches!(completion.options, Options::History(_))
                {
                    let size = &mut completion.history_size;
                    if matches!(resize, HistoryResize::Finish) {
                        let previous = HistorySize::new(
                            self.window_settings.agent_history_popup_width,
                            self.window_settings.agent_history_popup_height,
                        );
                        if size.width != previous.width {
                            self.window_settings.agent_history_popup_width = Some(size.width);
                        }
                        if size.height != previous.height {
                            self.window_settings.agent_history_popup_height = Some(size.height);
                        }
                        if *size != previous {
                            self.save_window_settings(Some(ticket.owner));
                        }
                    } else {
                        size.apply(resize);
                    }
                }
            }
        }
        Task::none()
    }

    fn agent_completion_live(&self, ticket: Ticket) -> bool {
        let Some((owner, pane, tab)) = self.agent_pane(ticket.session) else {
            return false;
        };
        owner == ticket.owner
            && pane.stream_generation == ticket.stream
            && tab.incognito == ticket.private
            && ticket.planning_allowed == self.clipboard.policy.ai_planning_allowed()
            && self.current_targets(owner, tab.id, &[ticket.session])
            && self
                .keyboard_targets(tab)
                .is_some_and(|route| route.predict && route.panes == [ticket.session])
            && self
                .agent_shortcut_scope(ticket.session)
                .is_some_and(|(aliases, prompt)| {
                    prompt == ticket.prompt
                        && pane
                            .agent_shortcut
                            .as_ref()
                            .is_some_and(|draft| draft.aliases == aliases)
                })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::tab_titles::WindowSettings;

    #[test]
    fn history_popup_uses_java_defaults_and_bounds_each_resize() {
        let mut size = HistorySize::new(None, None);
        assert_eq!((size.width, size.height), (460, 260));
        size.apply(HistoryResize::Width(680.5));
        assert_eq!((size.width, size.height), (681, 260));
        size.apply(HistoryResize::Height(1200.0));
        size.apply(HistoryResize::Width(f32::NAN));
        assert_eq!((size.width, size.height), (681, 900));
        size.apply(HistoryResize::Width(220.0));
        assert_eq!((size.width, size.height), (280, 900));
    }

    #[test]
    fn narrow_pane_limits_display_without_losing_saved_dimensions() {
        let settings = WindowSettings::decode(
            br#"{"agent_history_popup_width":820,"agent_history_popup_height":500}"#,
        )
        .unwrap();
        let serialized = serde_json::to_vec(&settings).unwrap();
        let reopened = WindowSettings::decode(&serialized).unwrap();
        let preferred = HistorySize::new(
            reopened.agent_history_popup_width,
            reopened.agent_history_popup_height,
        );
        assert_eq!((preferred.width, preferred.height), (820, 500));
        assert_eq!(
            HistorySize::pane_limit(Size::new(240.0, 160.0)),
            Size::new(228.0, 120.0)
        );
    }
}
