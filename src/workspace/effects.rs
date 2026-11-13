//! Routes the built-in effect manager and applies appearance to live panes.

use std::thread;
use std::time::{Duration, Instant};

use iced::futures::{StreamExt, stream::BoxStream};
use iced::widget::{column, slider, text};
use iced::{Element, Size, Subscription, Task, window};

use crate::connection_store::{Store, TerminalOverrides};
use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT, ProjectTerminalAppearance};
use crate::session::{Frame, SessionId};
use crate::terminal_effects::{Effect, Selection, Speed};
use crate::terminal_theme::{Appearance, Id, installed_font};
use crate::terminal_view::background_image;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::terminal_effect_access::{Action, Manager, Message};
use super::{Message as Desktop, PaneSession, Window, Workspace, tool_tabs::Kind};

#[derive(Clone, Copy)]
pub(super) struct Active {
    pub(super) selection: Selection,
    since: Instant,
    offset: f32,
    flash: Option<(Instant, u16)>,
}

impl Active {
    fn new(effect: Effect, speed: Speed, now: Instant) -> Self {
        Self {
            selection: Selection { effect, speed },
            since: now,
            offset: 0.0,
            flash: None,
        }
    }

    pub(super) fn from_saved(selection: Selection) -> Self {
        Self::new(selection.effect, selection.speed, Instant::now())
    }

    pub(super) fn seconds(self, now: Instant) -> f32 {
        self.offset
            + now.saturating_duration_since(self.since).as_secs_f32()
                * self.selection.speed.multiplier() as f32
    }

    pub(super) fn update_flash(&mut self, previous: &Frame, next: &Frame) {
        if self.selection.effect != Effect::Mother
            || !next.at_live_viewport()
            || !previous.at_live_viewport()
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
        let speed = self.pane_effect(session)?.selection.speed;
        Some(
            column![
                text(format!("Animation Speed: {speed:.2}x")).size(13),
                slider(1.0..=99.0, speed.multiplier(), move |speed| {
                    Desktop::TerminalEffectSpeed(owner, session, Speed::normalized(speed))
                }),
            ]
            .padding([4, 8])
            .spacing(4)
            .into(),
        )
    }

    pub(super) fn pane_appearance(&self, session: SessionId) -> Appearance {
        let pane = self.sessions.get(&session);
        let base = if let Some(project) =
            pane.and_then(|pane| pane.project_terminal_appearance.as_deref())
        {
            self.project_base_appearance(project)
        } else {
            self.terminal_base_appearance(pane.and_then(|pane| self.terminal_overrides(pane)))
        };
        self.pane_effect(session)
            .map_or(base, |active| active.selection.effect.appearance(base))
    }

    pub(super) fn pane_theme_id(&self, session: SessionId) -> Option<Id> {
        let pane = self.sessions.get(&session)?;
        let themes = &self.window_settings.themes;
        if let Some(project) = &pane.project_terminal_appearance {
            return project.theme_id(themes);
        }
        self.terminal_overrides(pane)
            .and_then(|settings| settings.theme.as_ref())
            .and_then(|theme| theme.resolved_theme(themes))
            .map(|theme| theme.id)
            .or_else(|| themes.selected())
    }

    pub(super) fn pane_background_image(
        &self,
        session: SessionId,
    ) -> Option<&background_image::Image> {
        let asset = self
            .window_settings
            .themes
            .background_asset(self.pane_theme_id(session)?)?;
        self.terminal_backgrounds.get(asset)
    }

    pub(super) fn refresh_terminal_backgrounds(&mut self) {
        if let Ok(root) = crate::profile::root() {
            self.terminal_backgrounds
                .refresh(&root, self.window_settings.themes.background_assets());
        }
    }

    pub(super) fn project_base_appearance(
        &self,
        project: &ProjectTerminalAppearance,
    ) -> Appearance {
        project.apply(
            self.window_settings.terminal_appearance(),
            &self.window_settings.themes,
        )
    }

    pub(in crate::workspace) fn terminal_base_appearance(
        &self,
        settings: Option<&TerminalOverrides>,
    ) -> Appearance {
        let base = self.window_settings.terminal_appearance();
        settings.map_or(base, |settings| {
            settings.appearance(base, &self.window_settings.themes)
        })
    }

    pub(in crate::workspace) fn terminal_overrides(
        &self,
        pane: &PaneSession,
    ) -> Option<&TerminalOverrides> {
        if let Some(local) = &pane.saved_local {
            return self.saved_terminal_preferences.local_terminal(local.id);
        }
        self.saved_terminal_preferences.terminal(pane.ssh.as_ref()?)
    }

    pub(in crate::workspace) fn terminal_scrollback(
        &self,
        settings: Option<&TerminalOverrides>,
    ) -> kortty_terminal::ScrollbackConfig {
        let defaults = self.window_settings.scrollback_config();
        settings.map_or(defaults, |settings| settings.scrollback(defaults))
    }

    pub(in crate::workspace) fn sync_open_scrollback(&self) {
        for pane in self.sessions.values() {
            pane.session
                .set_scrollback_config(self.terminal_scrollback(self.terminal_overrides(pane)));
        }
    }

    pub(in crate::workspace) fn refresh_saved_terminal_preferences(&mut self, store: &Store) {
        self.saved_terminal_preferences.refresh(store);
        self.sync_open_scrollback();
        for &session in self.sessions.keys() {
            self.sync_pane_appearance(session);
        }
        self.refresh_highlights();
    }

    pub(in crate::workspace) fn pane_terminal_font(
        &self,
        pane: &PaneSession,
        effect: bool,
    ) -> (u8, iced::Font) {
        let themes = &self.window_settings.themes;
        let global_scheme = themes.appearance();
        if let Some(project) = &pane.project_terminal_appearance {
            let (size, font) = project.own_font(themes, effect).unwrap_or_else(|| {
                let base = self.window_settings.terminal_font;
                (
                    themes.font_size(global_scheme, base, base),
                    self.window_settings
                        .terminal_font_face(global_scheme.filter(|_| !effect)),
                )
            });
            let size = i16::from(project.font_zoom.unwrap_or(size)) + i16::from(pane.font)
                - i16::from(pane.font_baseline);
            return (
                size.clamp(i16::from(MIN_TERMINAL_FONT), i16::from(MAX_TERMINAL_FONT)) as u8,
                font,
            );
        }
        let settings = self.terminal_overrides(pane);
        let scheme = settings.and_then(|settings| settings.scheme(themes));
        let base = settings
            .and_then(|settings| settings.font_size)
            .or_else(|| scheme.map(|scheme| scheme.font_size));
        let font_size = base.map_or_else(
            || themes.font_size(global_scheme, pane.font, pane.font_baseline),
            |base| {
                (i16::from(base) + i16::from(pane.font) - i16::from(pane.font_baseline))
                    .clamp(i16::from(MIN_TERMINAL_FONT), i16::from(MAX_TERMINAL_FONT))
                    as u8
            },
        );
        let font = settings
            .and_then(|settings| settings.font_family.as_deref())
            .map(|family| installed_font(family).unwrap_or(iced::Font::MONOSPACE))
            .or_else(|| {
                scheme
                    .filter(|_| !effect)
                    .map(|scheme| scheme.family.font())
            })
            .unwrap_or_else(|| {
                self.window_settings
                    .terminal_font_face(global_scheme.filter(|_| !effect))
            });
        (font_size, font)
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
        let speed = pane
            .effect
            .map_or(Speed::DEFAULT, |active| active.selection.speed);
        pane.effect = effect.map(|effect| Active::new(effect, speed, Instant::now()));
        self.sync_pane_appearance(session);
        self.session_restore.changed(false);
    }

    pub(super) fn set_terminal_effect_speed(&mut self, session: SessionId, speed: Speed) {
        let Some(active) = self.pane_effect(session) else {
            return;
        };
        if active.selection.speed == speed {
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
                .map(|active| active.selection.speed);
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

    pub(super) fn terminal_effect_modal(
        &self,
        surface: window::Id,
    ) -> Option<(window::Id, Desktop)> {
        let id = self.native_terminal_effects().filter(|&id| id != surface)?;
        Some((
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
            return Some(self.close_placed_dialog_and_focus(id, owner));
        }
        Some(self.close_placed_dialog(id))
    }

    pub(super) fn remove_terminal_effects_for_owner(&mut self, owner: window::Id) {
        self.effect_dialogs
            .retain(|_, dialog| !dialog.native || dialog.manager.owner != owner);
    }

    pub(super) fn update_terminal_effects(&mut self, message: Message) -> Task<Desktop> {
        match message {
            Message::Open(owner) => self.open_terminal_effects(owner),
            Message::Window(id, action) => self.update_terminal_effect_dialog(id, action),
        }
    }

    fn open_terminal_effects(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting
            || self.scheduler_draining
            || !self.terminal_effects_permitted()
            || !self.windows.contains_key(&owner)
        {
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
        let (id, opened) = self.open_owned_placed_dialog(
            owner,
            DialogKind::TerminalEffects,
            window::Settings {
                size: Size::new(930.0, 480.0),
                min_size: Some(Size::new(760.0, 340.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
            None,
        );
        self.effect_dialogs.insert(
            id,
            Dialog {
                native: true,
                manager: Manager::new(owner),
                preview_started: Instant::now(),
            },
        );
        opened
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

#[cfg(test)]
mod tests {
    use iced::futures::{executor::block_on, future::ready};
    use iced::keyboard::{Key, Location, Modifiers};
    use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

    use super::*;
    use crate::workspace::{chrome, settings_window};

    fn assert_gain_focus(focus: Task<Desktop>, dialog: window::Id) {
        assert!(matches!(
            block_on(task::into_stream(focus).unwrap().next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == dialog
        ));
    }

    #[test]
    fn saved_local_terminal_overrides_follow_edits_and_inherited_values() {
        use crate::connection_store::LocalProfile;
        use crate::terminal_theme::Rgb;
        use kortty_terminal::{ScrollbackConfig, TerminalProfile, TextEncoding};

        let scratch = crate::snippets::tests::Profile::new();
        let mut store = Store::open(scratch.0.join("connections.json")).unwrap();
        let id = store
            .save_local(LocalProfile {
                id: 0,
                name: "Appearance fixture".into(),
                shell: Default::default(),
                command: None,
                working_directory: None,
                shell_integration: false,
                emulation: TerminalProfile::Xterm.term_name().into(),
                terminal: TerminalOverrides {
                    foreground: Some(Rgb::from_hex(0xC0FFEE)),
                    font_size: Some(23),
                    encoding: Some(TextEncoding::Latin9),
                    terminal_columns: Some(112),
                    scrollback_lines: Some(200),
                    scrollback_compression: Some(false),
                    confirm_close: Some(true),
                    ..TerminalOverrides::default()
                },
            })
            .unwrap();
        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_font = 16;
        workspace.window_settings.terminal_columns = Some(99);
        workspace.window_settings.terminal_rows = Some(31);
        workspace.window_settings.scrollback_lines = 150;
        workspace.window_settings.scrollback_memory_mib = 3;
        workspace.window_settings.colors.background = Some(Rgb::from_hex(0x102030));
        let session = workspace
            .store_saved_local_session(store.local(id).unwrap().clone(), None)
            .unwrap();
        let pane = &workspace.sessions[&session];
        assert_eq!(pane.frame.screen.text_encoding(), TextEncoding::Latin9);
        assert_eq!((pane.grid.columns, pane.grid.rows), (112, 31));
        assert_eq!(
            (pane.frame.screen.columns, pane.frame.screen.rows),
            (112, 31)
        );
        assert_eq!(
            pane.session.scrollback_config_requested(),
            ScrollbackConfig {
                lines: 200,
                memory_mib: 3,
                compression: false
            }
        );
        assert_eq!(workspace.pane_terminal_font(pane, false).0, 23);
        assert_eq!(
            workspace.pane_appearance(session).foreground,
            Rgb::from_hex(0xC0FFEE)
        );
        assert_eq!(
            workspace.pane_appearance(session).background,
            Rgb::from_hex(0x102030)
        );
        let (owner, _) = workspace.open_window(false);
        workspace.add_tab(owner, session);
        let tab = workspace.windows[&owner].tab().unwrap().id;
        let _ = workspace.request_close(owner, super::super::SerialClose::Tab(tab));
        let (request, _) = workspace.windows[&owner].pending_serial_close.unwrap();
        let _ = workspace.update(Desktop::ConfirmSerialClose(owner, request, false));
        assert!(workspace.sessions.contains_key(&session));

        workspace.window_settings.scrollback_lines = 180;
        workspace.window_settings.scrollback_memory_mib = 5;
        workspace.sync_open_scrollback();
        assert_eq!(
            workspace.sessions[&session]
                .session
                .scrollback_config_requested(),
            ScrollbackConfig {
                lines: 200,
                memory_mib: 5,
                compression: false
            }
        );
        let mut edited = store.local(id).unwrap().clone();
        edited.terminal = TerminalOverrides::default();
        store.save_local(edited).unwrap();
        workspace.refresh_saved_terminal_preferences(&store);
        let pane = &workspace.sessions[&session];
        assert_eq!(workspace.pane_terminal_font(pane, false).0, 16);
        assert_eq!((pane.grid.columns, pane.grid.rows), (112, 31));
        assert_eq!(
            workspace.pane_appearance(session),
            workspace.window_settings.terminal_appearance()
        );
        assert_eq!(pane.frame.screen.text_encoding(), TextEncoding::Latin9);
        assert_eq!(
            pane.session.scrollback_config_requested(),
            workspace.window_settings.scrollback_config()
        );
        assert!(!workspace.pane_needs_close_confirmation(session));
    }

    #[test]
    fn saved_local_and_ssh_panes_resolve_custom_themes_by_live_id() {
        use kortty_terminal::TerminalProfile;

        use crate::connection_store::{Connection, LocalProfile, ThemeOverride};
        use crate::session::{IbmKind, Session};
        use crate::terminal_theme::{Rgb, Scheme};

        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("connections.json");
        let mut store = Store::open(path.clone()).unwrap();
        let mut workspace = Workspace::default();
        let initial = Scheme {
            foreground: Rgb::from_hex(0x010203),
            background: Rgb::from_hex(0x112233),
            font_size: 22,
            ..Scheme::default()
        };
        let id = workspace
            .window_settings
            .themes
            .store(None, "Operations", initial)
            .unwrap();
        workspace.window_settings.themes.select(None);
        let terminal = TerminalOverrides {
            theme: Some(ThemeOverride::from(id)),
            ..TerminalOverrides::default()
        };
        let local = store
            .save_local(LocalProfile {
                id: 0,
                name: "Theme local".into(),
                shell: Default::default(),
                command: None,
                working_directory: None,
                shell_integration: false,
                emulation: TerminalProfile::Xterm.term_name().into(),
                terminal: terminal.clone(),
            })
            .unwrap();
        let mut connection: Connection = serde_json::from_value(serde_json::json!({
            "id":0,"name":"Theme SSH","tag":"","jump":null,"tunnels":[],
            "login":{"endpoint":{"host":"theme.example","port":22,"username":"operator"},"auth":"prompt"},
            "emulation":TerminalProfile::Xterm.term_name(),
        })).unwrap();
        let explicit = Rgb::from_hex(0x99AABB);
        connection.terminal = TerminalOverrides {
            foreground: Some(explicit),
            ..terminal
        };
        let remote = store.save(connection).unwrap();
        let store = Store::open(path).unwrap();
        let [local_pane, remote_pane] = std::array::from_fn(|_| {
            workspace.store_session(
                Session::disconnected_ibm(IbmKind::Tn3270e),
                Default::default(),
                14,
            )
        });
        workspace.sessions.get_mut(&local_pane).unwrap().saved_local = store.local(local).cloned();
        workspace.sessions.get_mut(&remote_pane).unwrap().ssh = Some(
            serde_json::from_value(serde_json::json!({
                "host":"theme.example","port":22,"username":"operator","saved":remote,
            }))
            .unwrap(),
        );
        workspace.refresh_saved_terminal_preferences(&store);
        assert_eq!(
            workspace.pane_appearance(local_pane).foreground,
            initial.foreground
        );
        assert_eq!(workspace.pane_appearance(remote_pane).foreground, explicit);
        for pane in [local_pane, remote_pane] {
            assert_eq!(
                workspace.pane_appearance(pane).background,
                initial.background
            );
            assert_eq!(
                workspace
                    .pane_terminal_font(&workspace.sessions[&pane], false)
                    .0,
                22
            );
        }

        let edited = Scheme {
            background: Rgb::from_hex(0x455667),
            ..initial
        };
        workspace
            .window_settings
            .themes
            .store(Some(id), "Operations revised", edited)
            .unwrap();
        workspace.window_settings.themes.select(None);
        for pane in [local_pane, remote_pane] {
            workspace.sync_pane_appearance(pane);
            assert_eq!(
                workspace.pane_appearance(pane).background,
                edited.background
            );
        }
        workspace.window_settings.themes.remove(id);
        let global = workspace.window_settings.terminal_appearance();
        assert_eq!(workspace.pane_appearance(local_pane), global);
        assert_eq!(
            workspace.pane_appearance(remote_pane).background,
            global.background
        );
        assert_eq!(workspace.pane_appearance(remote_pane).foreground, explicit);
        assert_eq!(store.local(local).unwrap().terminal.theme, Some(id.into()));
        assert_eq!(store.get(remote).unwrap().terminal.theme, Some(id.into()));
    }

    #[test]
    fn native_terminal_effects_blocks_peer_input_and_closes_before_its_owner() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = false;
        let (owner, _) = workspace.open_window(false);
        let (peer, _) = workspace.open_window(false);
        let _ = workspace.update_settings(settings_window::Message::Open(peer));
        let peer_settings = workspace.settings.for_owner(peer).unwrap();

        let _ = workspace.update_terminal_effects(Message::Open(owner));
        let dialog = workspace.native_terminal_effects().unwrap();
        assert_eq!(workspace.placed_dialog_owner(dialog), Some(owner));
        assert!(workspace.owner_dialog(dialog).is_none());
        for surface in [owner, peer, peer_settings] {
            assert_eq!(workspace.owner_dialog(surface).unwrap().0, dialog);
        }
        let command = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL | Modifiers::SHIFT
        };
        assert_gain_focus(
            workspace.update(Desktop::Key(
                peer,
                Key::Character("q".into()),
                Location::Standard,
                command,
                None,
            )),
            dialog,
        );
        assert_gain_focus(
            workspace.update(Desktop::Window(peer, window::Event::CloseRequested)),
            dialog,
        );
        let _ = workspace.update(Desktop::Chrome(chrome::Message::Invoke(
            peer,
            chrome::Action::NewWindow,
        )));
        assert_eq!(workspace.windows.len(), 2);
        assert!(!workspace.exiting);

        let _ = workspace.finish_owned_dialog_open(dialog, owner, None);
        let close = workspace.update_terminal_effects(Message::Window(dialog, Action::Close));
        let mut actions = task::into_stream(close).unwrap();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == owner
        ));
        let _ = workspace.update(Desktop::Window(dialog, window::Event::Closed));

        let _ = workspace.update_terminal_effects(Message::Open(owner));
        let dialog = workspace.native_terminal_effects().unwrap();
        let close = task::into_stream(workspace.close_window(owner)).unwrap();
        let closed: Vec<_> = block_on(
            close
                .filter_map(|action| {
                    ready(match action {
                        RuntimeAction::Window(WindowAction::Close(id)) => Some(id),
                        _ => None,
                    })
                })
                .collect(),
        );
        assert_eq!(closed, [dialog, owner]);
        assert!(workspace.native_terminal_effects().is_none());
        assert!(workspace.placed_dialog_owner(dialog).is_none());
        assert!(workspace.windows.contains_key(&peer));
        assert_eq!(workspace.owner_dialog(peer).unwrap().0, peer_settings);
    }
}
