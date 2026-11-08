use std::collections::HashSet;
use std::time::Duration;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::container;
use iced::{Element, Length, Rectangle, Size, Task, window};
use kortty_windowing::WindowMode;

use crate::profile::WindowPlacement;

use super::{Message, TabEntry, Workspace, window_lifecycle};

#[cfg(test)]
mod tests;

pub(super) const HISTORY_LIMIT: usize = 100;

#[derive(Default)]
pub(super) struct TabCycle {
    snapshot: Vec<u64>,
    position: usize,
}

impl TabCycle {
    pub(super) fn clear(&mut self) -> bool {
        let active = !self.snapshot.is_empty();
        self.snapshot.clear();
        active
    }

    pub(super) fn advance(
        &mut self,
        open: &[u64],
        recent: &[u64],
        selected: u64,
        backwards: bool,
    ) -> Option<u64> {
        if open.is_empty() {
            self.clear();
            return None;
        }
        let mut remaining: HashSet<_> = open.iter().copied().collect();
        if let Some(next) = self.step(&remaining, backwards) {
            return Some(next);
        }
        self.clear();
        self.position = 0;
        self.snapshot.extend(
            std::iter::once(&selected)
                .chain(recent)
                .chain(open)
                .copied()
                .filter(|id| remaining.remove(id)),
        );
        let position = next_index(0, self.snapshot.len(), backwards);
        self.position = position;
        Some(self.snapshot[position])
    }

    fn step(&mut self, open: &HashSet<u64>, backwards: bool) -> Option<u64> {
        for _ in 0..self.snapshot.len() {
            self.position = next_index(self.position, self.snapshot.len(), backwards);
            let next = self.snapshot[self.position];
            if open.contains(&next) {
                return Some(next);
            }
        }
        None
    }
}

pub(super) fn next_index(index: usize, length: usize, backwards: bool) -> usize {
    if backwards {
        index.checked_sub(1).unwrap_or(length - 1)
    } else {
        (index + 1) % length
    }
}

pub(super) fn tab_step(key: &Key, modifiers: Modifiers) -> Option<bool> {
    (key == &Key::Named(Named::Tab) && modifiers.control()).then_some(modifiers.shift())
}

pub(super) fn ends_tab_cycle(key: &Key, modifiers: Modifiers) -> bool {
    tab_step(key, modifiers).is_none()
        && !matches!(
            key,
            Key::Named(
                Named::Alt
                    | Named::AltGraph
                    | Named::Control
                    | Named::Meta
                    | Named::Shift
                    | Named::Super
            )
        )
}

pub(super) fn ends_tab_cycle_on_release(key: &Key, modifiers: Modifiers) -> bool {
    key == &Key::Named(Named::Control) || !modifiers.control()
}

impl Workspace {
    pub(super) fn select_tab(&mut self, id: window::Id, tab_id: u64, preview: bool) {
        if self.hosted_close_blocks_selection(id, tab_id) {
            return;
        }
        self.dismiss_terminal_context(id);
        self.dismiss_journal_context_for_window(id);
        if let Some(window) = self.windows.get_mut(&id)
            && let Some(index) = window.tabs.iter().position(|tab| tab.id() == tab_id)
        {
            window.active = index;
            let focused = window.focused;
            if let Some(tab) = window.tab_mut() {
                tab.remote_files.release_keyboard();
                if focused {
                    tab.watch.seen();
                }
            }
            if !preview {
                window.remember_active_tab();
            }
            window.cancel_paste();
            window.pending_ibm = None;
            window.pending_serial = None;
            window.pending_serial_close = None;
            window.transfer_open = false;
        }
    }

    pub(super) fn switch_tab_once(&mut self, id: window::Id, backwards: bool) {
        self.switch_tab_from_keyboard(id, backwards);
        self.commit_tab_cycle(id);
    }

    pub(super) fn switch_tab_from_keyboard(&mut self, id: window::Id, backwards: bool) {
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        let Some(selected) = window.active_tab_id() else {
            return;
        };
        let mru = self.window_settings.tab_switch_most_recent_first;
        let next = if mru {
            let open: Vec<_> = window.tabs.iter().map(TabEntry::id).collect();
            window
                .tab_cycle
                .advance(&open, &window.tab_mru, selected, backwards)
        } else {
            Some(window.tabs[next_index(window.active, window.tabs.len(), backwards)].id())
        };
        if let Some(next) = next.filter(|next| *next != selected) {
            self.select_tab(id, next, mru);
            self.session_restore.changed(false);
        }
    }

    pub(super) fn commit_tab_cycle(&mut self, id: window::Id) {
        if let Some(window) = self.windows.get_mut(&id) {
            window.commit_tab_cycle();
        }
    }

    pub(super) fn commit_all_tab_cycles(&mut self) {
        for window in self.windows.values_mut() {
            window.commit_tab_cycle();
        }
    }
}

impl super::Window {
    pub(super) fn commit_tab_cycle(&mut self) {
        if self.tab_cycle.clear() {
            self.remember_active_tab();
        }
    }

    pub(super) fn content_bounds(&self) -> Rectangle {
        let size = self.terminal_only.size().unwrap_or(self.size);
        let width = self.size.width.min(size.width);
        let height = self.size.height.min(size.height);
        Rectangle {
            x: (self.size.width - width) / 2.0,
            y: (self.size.height - height) / 2.0,
            width,
            height,
        }
    }
}

#[derive(Default)]
pub(super) enum TerminalOnly {
    #[default]
    Off,
    Inspecting {
        request: u64,
        live_size: Size,
    },
    Entering {
        request: u64,
        size: Size,
    },
    Active {
        size: Size,
        previous_fullscreen: bool,
    },
}

impl TerminalOnly {
    pub(super) fn size(&self) -> Option<Size> {
        match self {
            Self::Entering { size, .. } | Self::Active { size, .. } => Some(*size),
            _ => None,
        }
    }

    pub(super) fn needs_native_mode(&self) -> bool {
        !matches!(self, Self::Off)
    }

    fn inspected(&mut self, request: u64, mode: WindowMode, placement: WindowPlacement) -> bool {
        let Self::Inspecting {
            request: pending,
            live_size,
        } = *self
        else {
            return false;
        };
        if request != pending {
            return false;
        }
        let normal_size = Size::new(placement.width, placement.height);
        *self = if mode.fullscreen {
            Self::Active {
                size: normal_size,
                previous_fullscreen: true,
            }
        } else if mode.windowed {
            let size = if WindowPlacement::valid_size(live_size.width, live_size.height) {
                live_size
            } else {
                normal_size
            };
            Self::Entering { request, size }
        } else {
            Self::Off
        };
        matches!(self, Self::Entering { .. })
    }

    fn observe(&mut self, mode: WindowMode) -> bool {
        match *self {
            Self::Entering { size, .. } if mode.fullscreen => {
                *self = Self::Active {
                    size,
                    previous_fullscreen: false,
                };
                false
            }
            Self::Active {
                previous_fullscreen,
                ..
            } if mode.windowed => {
                *self = Self::Off;
                previous_fullscreen
            }
            _ => false,
        }
    }

    pub(super) fn timed_out(&mut self, request: u64, mode: WindowMode) {
        if matches!(*self, Self::Entering { request: pending, .. } if pending == request) {
            self.observe(mode);
            if matches!(self, Self::Entering { .. }) {
                *self = Self::Off;
            }
        }
    }
}

impl Workspace {
    pub(super) fn terminal_only_fullscreen_active(&self, id: window::Id) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|window| matches!(window.terminal_only, TerminalOnly::Active { .. }))
    }

    pub(super) fn toggle_terminal_only_fullscreen(&mut self, id: window::Id) -> Task<Message> {
        if self.terminal_only_fullscreen_active(id) {
            return self.leave_terminal_only_fullscreen(id);
        }
        if self
            .windows
            .get(&id)
            .is_none_or(|window| window.terminal_only.needs_native_mode())
        {
            return Task::none();
        }
        let request = self.allocate();
        let window = self.windows.get_mut(&id).expect("window was checked above");
        window.terminal_only = TerminalOnly::Inspecting {
            request,
            live_size: window.size,
        };
        window_lifecycle::window_mode(id)
            .map(move |mode| Message::TerminalOnlyInspected(id, request, mode))
    }

    pub(super) fn toggle_window_fullscreen(&mut self, id: window::Id) -> Task<Message> {
        match self.windows.get(&id).map(|window| &window.terminal_only) {
            Some(TerminalOnly::Active { .. }) => self.leave_terminal_only_fullscreen(id),
            Some(TerminalOnly::Off) => window_lifecycle::toggle_fullscreen(id),
            _ => Task::none(),
        }
    }

    fn leave_terminal_only_fullscreen(&mut self, id: window::Id) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let TerminalOnly::Active {
            previous_fullscreen,
            ..
        } = window.terminal_only
        else {
            return Task::none();
        };
        window.terminal_only = TerminalOnly::Off;
        if previous_fullscreen {
            Task::none()
        } else {
            window_lifecycle::set_fullscreen(id, false)
        }
    }

    pub(super) fn terminal_only_inspected(
        &mut self,
        id: window::Id,
        request: u64,
        mode: WindowMode,
    ) -> Task<Message> {
        let Some(window) = self.windows.get_mut(&id) else {
            return Task::none();
        };
        let placement = window.placement();
        if !window.terminal_only.inspected(request, mode, placement) {
            return Task::none();
        }
        window_lifecycle::set_fullscreen(id, true).chain(
            Task::future(wait_for_fullscreen())
                .then(move |()| window_lifecycle::window_mode(id))
                .map(move |mode| Message::TerminalOnlyDeadline(id, request, mode)),
        )
    }

    pub(super) fn observe_terminal_only_mode(
        &mut self,
        id: window::Id,
        mode: WindowMode,
    ) -> Task<Message> {
        if self
            .windows
            .get_mut(&id)
            .is_some_and(|window| window.terminal_only.observe(mode))
        {
            return window_lifecycle::set_fullscreen(id, true);
        }
        Task::none()
    }
}

async fn wait_for_fullscreen() {
    let (ready, waiting) = iced::futures::channel::oneshot::channel();
    let _ = std::thread::Builder::new()
        .name("kortty-fullscreen".into())
        .spawn(move || {
            // A compositor can accept the request without changing the window's dimensions.
            std::thread::sleep(Duration::from_secs(3));
            let _ = ready.send(());
        });
    let _ = waiting.await;
}

pub(super) fn center(content: Element<'_, Message>, size: Size) -> Element<'_, Message> {
    container(
        container(content)
            .width(size.width)
            .height(size.height)
            .style(|theme| {
                container::Style::default().background(iced::theme::default(theme).background_color)
            }),
    )
    .center(Length::Fill)
    .style(|_| container::Style::default().background(iced::Color::BLACK))
    .into()
}
