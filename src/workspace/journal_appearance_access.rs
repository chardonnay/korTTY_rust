//! Shared profile appearance and ownership of the viewer and Live Journal popovers.

use std::thread;
use std::time::{Duration, Instant};

use iced::widget::{Text, text};
use iced::{Element, Task, theme, window};

use crate::terminal_theme::installed_font;

use super::journal_appearance::{Content, Preferences, ThemeCycle, controls};
use super::{Message, Workspace};

const POPOVER_SAVE_DELAY: Duration = Duration::from_millis(400);
const PAGE_SAVE_DELAY: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Viewer(window::Id),
    Dock {
        owner: window::Id,
        tab: u64,
        run: u64,
    },
}

#[derive(Default)]
pub(super) struct State {
    popup: Option<Popup>,
    save: Option<Save>,
    system_theme: theme::Mode,
    theme_cycle: ThemeCycle,
}

struct Popup {
    target: Target,
    schemes_open: bool,
}

struct Save {
    owner: window::Id,
    deadline: Instant,
}

pub(super) struct Page<'a> {
    pub(super) content: Content,
    pub(super) preferences: &'a Preferences,
    pub(super) open: bool,
    pub(super) schemes_open: bool,
    pub(super) enabled: bool,
}

impl<'a> Page<'a> {
    pub(super) fn appearance_button<M: Clone + 'a>(
        &self,
        on_event: impl Fn(controls::Event) -> M + Clone + 'a,
    ) -> Element<'a, M> {
        controls::appearance_button(
            self.preferences,
            self.open,
            self.schemes_open,
            self.enabled,
            on_event,
        )
    }

    pub(super) fn page_controls<M: Clone + 'a>(
        &self,
        on_event: impl Fn(controls::Event) -> M + Clone + 'a,
    ) -> Element<'a, M> {
        controls::page_controls(self.preferences, self.enabled, on_event)
    }
}

impl Workspace {
    pub(super) fn set_journal_system_theme(&mut self, mode: theme::Mode) {
        self.journal_appearance_ui.system_theme = mode;
    }

    pub(super) fn journal_appearance_page(&self, target: Target) -> Page<'_> {
        let preferences = &self.window_settings.journal_appearance;
        let terminal = self.window_settings.terminal_appearance();
        let content = preferences.content(
            self.journal_system_dark(),
            Some((terminal.background, terminal.foreground)),
        );
        let enabled = self.journal_appearance_owner(target).is_some();
        let popup = self
            .journal_appearance_ui
            .popup
            .as_ref()
            .filter(|popup| enabled && popup.target == target);
        Page {
            content,
            preferences,
            open: popup.is_some(),
            schemes_open: popup.is_some_and(|popup| popup.schemes_open),
            enabled,
        }
    }

    fn journal_system_dark(&self) -> bool {
        self.journal_appearance_ui.system_theme != theme::Mode::Light
    }

    fn journal_appearance_owner(&self, target: Target) -> Option<window::Id> {
        match target {
            Target::Viewer(id) => self.journal_viewer_appearance_owner(id),
            Target::Dock { owner, tab, run } => self
                .journal_dock_appearance_current(owner, tab, run)
                .then_some(owner),
        }
    }

    pub(super) fn update_journal_appearance(
        &mut self,
        target: Target,
        event: controls::Event,
    ) -> Task<Message> {
        use controls::Event;

        if matches!(event, Event::Dismiss) {
            if self
                .journal_appearance_ui
                .popup
                .as_ref()
                .is_some_and(|popup| popup.target == target)
            {
                self.journal_appearance_ui.popup = None;
            }
            return Task::none();
        }
        let Some(owner) = self.journal_appearance_owner(target) else {
            return Task::none();
        };
        let open = self
            .journal_appearance_ui
            .popup
            .as_ref()
            .is_some_and(|popup| popup.target == target);
        if matches!(event, Event::Toggle) {
            self.journal_appearance_ui.popup = (!open).then_some(Popup {
                target,
                schemes_open: false,
            });
            return Task::none();
        }
        if matches!(event, Event::ToggleSchemes | Event::DismissSchemes) {
            if let Some(popup) = self
                .journal_appearance_ui
                .popup
                .as_mut()
                .filter(|popup| popup.target == target)
            {
                popup.schemes_open = matches!(event, Event::ToggleSchemes) && !popup.schemes_open;
            }
            return Task::none();
        }
        let page_action = matches!(event, Event::Zoom(_) | Event::CycleTheme);
        if !open && !page_action {
            return Task::none();
        }

        let system_dark = self.journal_system_dark();
        let preferences = &mut self.window_settings.journal_appearance;
        let previous = preferences.clone();
        match event {
            Event::Scheme(scheme) => {
                preferences.scheme = scheme;
                if let Some(popup) = &mut self.journal_appearance_ui.popup {
                    popup.schemes_open = false;
                }
            }
            Event::TextFont(family) if font_available(family.as_deref()) => {
                preferences.ui_font = family
            }
            Event::MonoFont(family) if font_available(family.as_deref()) => {
                preferences.mono_font = family
            }
            Event::Scale(scale) => preferences.set_scale(scale),
            Event::Reset => preferences.reset(),
            Event::Zoom(zoom) => preferences.zoom(zoom),
            Event::CycleTheme if matches!(target, Target::Dock { .. }) => {
                preferences.toggle_theme(system_dark);
                self.journal_appearance_ui.theme_cycle = ThemeCycle::default();
            }
            Event::CycleTheme => {
                self.journal_appearance_ui
                    .theme_cycle
                    .advance(preferences, system_dark);
            }
            _ => {}
        }
        if *preferences == previous {
            return Task::none();
        }
        let scale = preferences.scale;
        let resize = if scale != previous.scale {
            self.resize_journal_timelines(scale)
        } else {
            Task::none()
        };
        let delay = if page_action {
            PAGE_SAVE_DELAY
        } else {
            POPOVER_SAVE_DELAY
        };
        let already_scheduled = self
            .journal_appearance_ui
            .save
            .replace(Save {
                owner,
                deadline: Instant::now() + delay,
            })
            .is_some();
        let save = if already_scheduled {
            Task::none()
        } else {
            self.journal_appearance_save_after(delay)
        };
        Task::batch([resize, save])
    }

    pub(super) fn save_journal_appearance_when_ready(&mut self) -> Task<Message> {
        let Some(save) = &self.journal_appearance_ui.save else {
            return Task::none();
        };
        let remaining = save.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            self.flush_journal_appearance();
            Task::none()
        } else {
            self.journal_appearance_save_after(remaining)
        }
    }

    pub(super) fn flush_journal_appearance(&mut self) {
        if let Some(save) = self.journal_appearance_ui.save.take() {
            self.save_window_settings(Some(save.owner));
        }
    }

    fn journal_appearance_save_after(&mut self, delay: Duration) -> Task<Message> {
        let (send, receive) = async_channel::bounded(1);
        if thread::Builder::new()
            .name("kortty-journal-appearance-save".into())
            .spawn(move || {
                thread::sleep(delay);
                let _ = send.try_send(());
            })
            .is_err()
        {
            self.flush_journal_appearance();
            return Task::none();
        }
        Task::future(async move {
            let _ = receive.recv().await;
            Message::JournalAppearanceSave
        })
    }
}

fn font_available(family: Option<&str>) -> bool {
    family.is_none_or(|name| installed_font(name).is_some())
}

pub(super) fn page_text<'a>(
    content: Content,
    value: impl text::IntoFragment<'a>,
    size: f32,
) -> Text<'a> {
    text(value).font(content.ui_font).size(content.size(size))
}

#[cfg(test)]
mod tests;
