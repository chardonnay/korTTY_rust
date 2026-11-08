//! Routes the built-in effect manager and applies appearance to live panes.

use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{column, slider, text};
use iced::{Element, Size, Subscription, Task, window};

use crate::session::{Frame, SessionId};
use crate::terminal_effects::{Effect, Selection};
use crate::terminal_theme::Appearance;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::terminal_effect_access::{Action, Manager, Message};
use super::{Message as Desktop, Window, Workspace, tool_tabs::Kind};

#[derive(Clone, Copy)]
pub(super) struct Active {
    pub(super) selection: Selection,
    since: Instant,
    offset: f32,
    flash: Option<(Instant, u16)>,
}

impl Active {
    fn new(effect: Effect, speed: u8, now: Instant) -> Self {
        Self {
            selection: Selection { effect, speed },
            since: now,
            offset: 0.0,
            flash: None,
        }
    }

    pub(super) fn from_saved(selection: Selection) -> Self {
        Self::new(selection.effect, selection.speed(), Instant::now())
    }

    pub(super) fn seconds(self, now: Instant) -> f32 {
        self.offset
            + now.saturating_duration_since(self.since).as_secs_f32()
                * f32::from(self.selection.speed())
    }

    pub(super) fn update_flash(&mut self, previous: &Frame, next: &Frame) {
        if self.selection.effect != Effect::Mother
            || next.scroll_offset != 0
            || previous.scroll_offset != 0
        {
            return;
        }
        let row = next.screen.cursor.row;
        let now = Instant::now();
        if self
            .flash
            .is_none_or(|(last, _)| now.duration_since(last) >= Duration::from_millis(40))
            && (next.screen.cursor != previous.screen.cursor
                || next.screen.row(row) != previous.screen.row(row))
        {
            self.flash = Some((now, row));
        }
    }

    pub(super) fn overlay(self, now: Instant, font: u8) -> Element<'static, Desktop> {
        let flash = self.flash.and_then(|(started, row)| {
            let age = now.saturating_duration_since(started).as_secs_f32();
            (age < 0.260).then(|| {
                let (y, height) = crate::terminal_view::effect_row_bounds(font, row);
                crate::terminal_effects::Flash {
                    y,
                    height,
                    alpha: 0.24 * (1.0 - age / 0.260).powf(1.8),
                }
            })
        });
        self.selection.overlay_with_flash(self.seconds(now), flash)
    }
}

pub(super) struct Dialog {
    pub(super) native: bool,
    manager: Manager,
    preview_started: Instant,
}

impl Workspace {
    pub(super) fn terminal_effects_permitted(&self) -> bool {
        self.clipboard.policy.plugins_allowed && self.clipboard.policy.problem.is_none()
    }

    pub(super) fn terminal_effects_enabled(&self) -> bool {
        self.terminal_effects_permitted() && self.window_settings.terminal_effects.enabled
    }

    pub(super) fn allowed_terminal_effect(&self, effect: Effect) -> bool {
        self.terminal_effects_permitted() && self.window_settings.terminal_effects.allows(effect)
    }

    pub(super) fn effect_menu_target(&self, owner: window::Id) -> Option<SessionId> {
        let tab = self.windows.get(&owner)?.tab()?;
        tab.focused_session()
            .or_else(|| tab.grid.get(tab.primary).copied())
    }

    pub(super) fn effect_target_current(&self, owner: window::Id, session: SessionId) -> bool {
        self.owner_dialog(owner).is_none()
            && (self.effect_menu_target(owner) == Some(session)
                || self.window_contains_visible_pane(owner, session))
    }

    pub(super) fn effect_speed_view(
        &self,
        owner: window::Id,
        session: SessionId,
    ) -> Option<Element<'_, Desktop>> {
        let speed = self.pane_effect(session)?.selection.speed();
        Some(
            column![
                text(format!("Animation Speed: {speed}.00x")).size(13),
                slider(1..=99, speed, move |speed| Desktop::TerminalEffectSpeed(
                    owner, session, speed
                )),
            ]
            .padding([4, 8])
            .spacing(4)
            .into(),
        )
    }

    pub(super) fn pane_appearance(&self, session: SessionId) -> Appearance {
        let base = self.window_settings.terminal_appearance();
        self.pane_effect(session)
            .map_or(base, |active| active.selection.effect.appearance(base))
    }

    pub(super) fn pane_effect(&self, session: SessionId) -> Option<Active> {
        self.sessions
            .get(&session)?
            .effect
            .filter(|active| self.allowed_terminal_effect(active.selection.effect))
    }

    pub(super) fn select_terminal_effect(&mut self, session: SessionId, effect: Option<Effect>) {
        if effect.is_some_and(|effect| !self.allowed_terminal_effect(effect)) {
            return;
        }
        let Some(pane) = self.sessions.get_mut(&session) else {
            return;
        };
        if pane.effect.map(|active| active.selection.effect) == effect {
            return;
        }
        let speed = pane.effect.map_or(1, |active| active.selection.speed());
        pane.effect = effect.map(|effect| Active::new(effect, speed, Instant::now()));
        self.sync_pane_appearance(session);
        self.session_restore.changed(false);
    }

    pub(super) fn set_terminal_effect_speed(&mut self, session: SessionId, speed: u8) {
        let Some(active) = self.pane_effect(session) else {
            return;
        };
        let speed = speed.clamp(1, 99);
        if active.selection.speed() == speed {
            return;
        }
        let now = Instant::now();
        let mut next = Active::new(active.selection.effect, speed, now);
        next.offset = active.seconds(now);
        next.flash = active.flash;
        self.sessions
            .get_mut(&session)
            .expect("effect owner is live")
            .effect = Some(next);
        self.sync_pane_appearance(session);
        self.session_restore.changed(false);
    }

    pub(super) fn inherit_terminal_effect(&mut self, source: SessionId, target: SessionId) {
        let selection = self
            .sessions
            .get(&source)
            .and_then(|pane| pane.effect)
            .map(|active| active.selection);
        if let Some(selection) = selection
            && let Some(pane) = self.sessions.get_mut(&target)
        {
            pane.effect = Some(Active::from_saved(selection));
            self.sync_pane_appearance(target);
        }
    }

    pub(super) fn sync_pane_appearance(&self, session: SessionId) {
        if let Some(pane) = self.sessions.get(&session) {
            pane.session
                .set_terminal_appearance(self.pane_appearance(session));
            let speed = self
                .pane_effect(session)
                .filter(|active| active.selection.effect == Effect::TypewriterNoir)
                .map(|active| active.selection.speed());
            pane.session.set_typewriter_speed(speed);
        }
    }

    pub(super) fn terminal_effect_frame(&self) -> Instant {
        self.effect_frame.unwrap_or_else(Instant::now)
    }

    pub(super) fn effect_subscription(&self) -> Subscription<Desktop> {
        if !self.terminal_effects_enabled() {
            return Subscription::none();
        }
        let animate_preview =
            self.effect_dialogs.iter().any(|(&id, dialog)| {
                (dialog.native || self.hosted_tool_owner(id).is_some_and(|owner| {
                    self.windows.get(&owner).and_then(Window::active_entry).is_some_and(|entry| {
                    matches!(entry, super::TabEntry::Tool(tool) if tool.instance == id)
                })
                })) && dialog
                    .manager
                    .is_previewing(&self.window_settings.terminal_effects)
            });
        let animate_terminal = self.windows.values().filter_map(Window::tab).any(|tab| {
            let maximized = tab.grid.maximized();
            tab.grid.iter().any(|(pane, &session)| {
                maximized.is_none_or(|shown| shown == *pane) && self.pane_effect(session).is_some()
            })
        });
        if animate_preview || animate_terminal {
            Subscription::run(effect_clock)
        } else {
            Subscription::none()
        }
    }

    pub(super) fn terminal_effect_dialog_view(
        &self,
        id: window::Id,
    ) -> Option<Element<'_, Desktop>> {
        let dialog = self.effect_dialogs.get(&id)?;
        let clock = self
            .terminal_effect_frame()
            .saturating_duration_since(dialog.preview_started)
            .as_secs_f32();
        Some(
            dialog
                .manager
                .view(&self.window_settings.terminal_effects, clock)
                .map(move |action| Desktop::TerminalEffects(Message::Window(id, action))),
        )
    }

    pub(super) fn terminal_effect_modal(&self, owner: window::Id) -> Option<(window::Id, Desktop)> {
        let id = self.native_terminal_effects()?;
        self.windows.contains_key(&owner).then_some((
            id,
            Desktop::TerminalEffects(Message::Window(id, Action::Opened)),
        ))
    }

    pub(super) fn native_terminal_effects(&self) -> Option<window::Id> {
        self.effect_dialogs
            .iter()
            .find_map(|(&id, dialog)| dialog.native.then_some(id))
    }

    pub(super) fn close_terminal_effects(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        if !self.effect_dialogs.get(&id)?.native {
            return Some(self.request_hosted_tool_close(id));
        }
        let owner = self.effect_dialogs.remove(&id)?.manager.owner;
        if self.windows.contains_key(&owner) && !self.exiting {
            return Some(Task::batch([
                self.close_placed_dialog(id),
                window::gain_focus(owner),
            ]));
        }
        Some(self.close_placed_dialog(id))
    }

    pub(super) fn update_terminal_effects(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Open(owner) => self.open_terminal_effects(owner),
            Message::Window(id, action) => self.update_terminal_effect_dialog(id, action),
        }
    }

    fn open_terminal_effects(&mut self, owner: window::Id) -> Task<Desktop> {
        if !self.terminal_effects_permitted() || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some(id) = self.native_terminal_effects() {
            return window::gain_focus(id);
        }
        if self.owner_dialog(owner).is_some() {
            return Task::none();
        }
        self.dismiss_terminal_context(owner);
        if self.window_settings.open_tool_windows_as_tabs {
            if let Some(id) = self.hosted_tool_in(owner, Kind::TerminalEffects) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
            let id = window::Id::unique();
            self.effect_dialogs.insert(
                id,
                Dialog {
                    native: false,
                    manager: Manager::new(owner),
                    preview_started: Instant::now(),
                },
            );
            return self.host_tool(owner, Kind::TerminalEffects, id);
        }
        let (id, opened) = self.open_placed_dialog(
            DialogKind::TerminalEffects,
            window::Settings {
                size: Size::new(930.0, 480.0),
                min_size: Some(Size::new(760.0, 340.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.effect_dialogs.insert(
            id,
            Dialog {
                native: true,
                manager: Manager::new(owner),
                preview_started: Instant::now(),
            },
        );
        opened.map(move |_| Desktop::TerminalEffects(Message::Window(id, Action::Opened)))
    }

    fn update_terminal_effect_dialog(&mut self, id: window::Id, action: Action) -> Task<Desktop> {
        let Some(dialog) = self.effect_dialogs.get_mut(&id) else {
            return Task::none();
        };
        match action {
            Action::Opened => return window::gain_focus(id),
            Action::Close => return self.close_terminal_effects(id).unwrap_or_else(Task::none),
            Action::Select(effect) => {
                dialog.manager.select(effect);
                dialog.preview_started = Instant::now();
                return Task::none();
            }
            _ => {}
        }
        if !self.terminal_effects_permitted() {
            return Task::none();
        }
        let mut next = self.window_settings.clone();
        match action {
            Action::SetEnabled(enabled) => next.terminal_effects.enabled = enabled,
            Action::SetEffectEnabled(effect, enabled) if next.terminal_effects.enabled => {
                next.terminal_effects.set_effect_enabled(effect, enabled);
            }
            _ => return Task::none(),
        }
        if next.terminal_effects == self.window_settings.terminal_effects {
            return Task::none();
        }
        match crate::profile::window_settings_path().and_then(|path| next.save(&path)) {
            Ok(()) => {
                self.window_settings = next;
                if let Some(dialog) = self.effect_dialogs.get_mut(&id) {
                    dialog.manager.set_error(None);
                }
                let preferences = &self.window_settings.terminal_effects;
                let mut cleared = false;
                for pane in self.sessions.values_mut() {
                    if pane
                        .effect
                        .take_if(|active| !preferences.allows(active.selection.effect))
                        .is_some()
                    {
                        cleared = true;
                    }
                }
                if cleared {
                    self.session_restore.changed(false);
                }
                for &session in self.sessions.keys() {
                    self.sync_pane_appearance(session);
                }
            }
            Err(error) => {
                if let Some(dialog) = self.effect_dialogs.get_mut(&id) {
                    dialog
                        .manager
                        .set_error(Some(format!("Could not save terminal effects ({error}).")));
                }
            }
        }
        Task::none()
    }
}

fn effect_clock() -> BoxStream<'static, Desktop> {
    let (sender, receiver) = async_channel::bounded(1);
    let _ = thread::Builder::new()
        .name("kortty-terminal-effects".into())
        .spawn(move || {
            while !sender.is_closed() {
                let _ = sender.try_send(Instant::now());
                thread::sleep(Duration::from_millis(35));
            }
        });
    receiver.map(Desktop::TerminalEffectFrame).boxed()
}
