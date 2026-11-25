use super::super::dialog_design::keyboard_table;
use super::*;
#[cfg(test)]
use iced::Font;
use iced::widget::{column, row};

fn heading(label: &'static str) -> Element<'static, Desktop> {
    text(label)
        .size(14)
        .font(super::super::dialog_design::ui_font(
            iced::font::Weight::Bold,
        ))
        .into()
}

fn help(label: &'static str) -> Element<'static, Desktop> {
    container(text(label).size(12).style(text::secondary))
        .width(Length::Fill)
        .max_width(640)
        .into()
}

fn control_id(
    dialog: &Dialog,
    setting: Setting,
    fallback: &'static str,
    enabled: bool,
) -> widget::Id {
    dialog
        .input_target(setting, enabled)
        .cloned()
        .unwrap_or_else(|| widget::Id::new(fallback))
}

fn action(
    id: widget::Id,
    label: &'static str,
    message: Option<Desktop>,
) -> Element<'static, Desktop> {
    keyboard_table::action_with_id(
        dialog_design::secondary_action(label)
            .on_press_maybe(message.clone())
            .into(),
        message,
        true,
        Some(id),
    )
}

fn toggle(
    dialog: &Dialog,
    id: &'static str,
    setting: Setting,
    checked: bool,
    message: Option<Desktop>,
) -> Element<'static, Desktop> {
    let id = control_id(dialog, setting, id, message.is_some());
    keyboard_table::action_with_id(
        checkbox(checked)
            .label(setting.label())
            .text_size(13)
            .size(16)
            .spacing(8)
            .on_toggle_maybe(message.clone().map(|message| move |_| message.clone()))
            .into(),
        message,
        false,
        Some(id),
    )
}

impl Workspace {
    pub(super) fn security_settings_page(&self, dialog: &Dialog) -> Element<'static, Desktop> {
        let id = dialog.id;
        let policy = &self.clipboard.policy;
        let require_password = policy.require_master_password
            || dialog.draft.preferences.startup.require_master_password;
        let password_editable = policy.problem.is_none() && !policy.require_master_password;
        let minimum = policy.minimum_isolation;
        let isolation = dialog
            .draft
            .preferences
            .default_session_isolation
            .unwrap_or_default()
            .max(minimum);
        let allowed = IsolationLevel::ALL
            .into_iter()
            .filter(|level| *level >= minimum)
            .collect::<Vec<_>>();
        let isolation_control: Element<'_, Desktop> =
            if policy.problem.is_some() || allowed.len() == 1 {
                text(isolation.to_string()).into()
            } else {
                pick_list(allowed, Some(isolation), move |level| {
                    Desktop::Settings(Message::DefaultIsolation(id, level))
                })
                .text_size(13)
                .keyboard_navigation(control_id(
                    dialog,
                    Setting::DefaultIsolation,
                    "security-isolation",
                    true,
                ))
                .into()
            };
        let imported = dialog.draft.preferences.java_security;
        let vault_path = profile::vault_path().ok();
        let vault_exists = vault_path
            .as_ref()
            .is_some_and(|path| Vault::exists(path).unwrap_or(false));
        let startup_unlock = vault_path
            .as_ref()
            .and_then(|path| Vault::startup_unlock_state(path).ok());
        let (automatic_status, automatic_action, enable) = match startup_unlock {
            Some(StartupUnlockState::Enabled) => (
                if require_password {
                    "Enabled on this device. The startup password requirement takes precedence."
                } else {
                    "Enabled on this device."
                },
                "Disable automatic unlock",
                false,
            ),
            Some(StartupUnlockState::RemovalPending) => (
                "Disabled. The device credential still needs to be removed.",
                "Retry credential removal",
                false,
            ),
            Some(StartupUnlockState::Disabled) => (
                if !vault_exists {
                    "Create a credential vault before enabling automatic unlock."
                } else {
                    "The device’s credential store can unlock your vault when kortty starts."
                },
                "Configure automatic unlock…",
                true,
            ),
            None => (
                "The automatic unlock setting could not be read.",
                "Configure automatic unlock…",
                true,
            ),
        };
        let can_set_automatic =
            startup_unlock.is_some() && (!enable || (vault_exists && password_editable));
        column![
            heading("Master password"),
            help("Your master password unlocks the local credential vault."),
            highlighted(
                dialog,
                Setting::ChangeMasterPassword,
                action(control_id(dialog, Setting::ChangeMasterPassword, "security-change-password", true), "Change master password…", Some(Desktop::Settings(Message::ChangeMasterPassword(id)))),
            ),
            rule::horizontal(1),
            highlighted(
                dialog,
                Setting::StartupMasterPassword,
                column![
                    toggle(dialog, "security-startup-password", Setting::StartupMasterPassword, require_password,
                        password_editable.then_some(Desktop::Settings(Message::Startup(id,
                            startup_settings::Edit::MasterPassword(!require_password))))),
                    if policy.require_master_password {
                        text("Managed by your organization.").size(12)
                    } else if policy.problem.is_some() {
                        text("The startup setting is locked until the policy can be read.").size(12)
                    } else {
                        text("Takes effect the next time korTTY starts.").size(12)
                    },
                ]
                .spacing(5),
            ),
            highlighted(
                dialog,
                Setting::AutomaticVaultUnlock,
                column![
                    text(Setting::AutomaticVaultUnlock.label()).size(13),
                    text(automatic_status).size(12),
                    action(control_id(dialog, Setting::AutomaticVaultUnlock, "security-automatic-unlock", can_set_automatic), automatic_action,
                        can_set_automatic.then_some(Desktop::Settings(Message::AutomaticVaultUnlock(id, enable)))),
                ]
                .spacing(5),
            ),
            rule::horizontal(1),
            heading("Temporary SSH keys"),
            help("Use temporary private keys in Quick Connect and saved SSH profiles. Quick Connect keeps the key in memory for its selected lifetime."),
            highlighted(
                dialog,
                Setting::TemporarySshKey,
                toggle(dialog, "security-temporary-key", Setting::TemporarySshKey,
                    dialog.draft.preferences.temporary_ssh_key_enabled,
                    policy.problem.is_none().then_some(Desktop::Settings(Message::TemporarySshKey(id,
                        !dialog.draft.preferences.temporary_ssh_key_enabled)))),
            ),
            imported.temporary_ssh_keys.map(|enabled| {
                row![
                    text(if enabled { "Java backup · Enabled" } else { "Java backup · Disabled" }).size(12),
                    action(widget::Id::new("security-adopt-temporary-key"), "Use Java setting",
                        (policy.problem.is_none() && enabled != dialog.draft.preferences.temporary_ssh_key_enabled)
                            .then_some(Desktop::Settings(Message::TemporarySshKey(id, enabled)))),
                ].spacing(8).align_y(Alignment::Center)
            }),
            rule::horizontal(1),
            heading("Session isolation"),
            help("Set the default for new local and SSH sessions. Saved profiles and folders can specify their own level in Connection Manager."),
            highlighted(
                dialog,
                Setting::DefaultIsolation,
                row![
                    text(Setting::DefaultIsolation.label()).size(13),
                    isolation_control,
                ].spacing(8).align_y(Alignment::Center),
            ),
            imported.default_isolation.map(|level| {
                row![
                    text(if level < minimum {
                        format!("Java backup · {level} · Below the managed minimum")
                    } else {
                        format!("Java backup · {level}")
                    }).size(12),
                    action(widget::Id::new("security-adopt-isolation"), "Use Java setting",
                        (policy.problem.is_none() && level >= minimum
                            && dialog.draft.preferences.default_session_isolation != Some(level))
                            .then_some(Desktop::Settings(Message::DefaultIsolation(id, level)))),
                ].spacing(8).align_y(Alignment::Center)
            }),
            (minimum != IsolationLevel::None).then(|| text(format!("Managed minimum · {minimum}")).size(12)),
            highlighted(dialog, Setting::SandboxAvailability, text(dialog.sandbox_availability.clone().unwrap_or_else(|| "Checking sandbox availability…".into())).size(12)),
            (isolation == IsolationLevel::Sandbox).then(|| text("Set an absolute sandbox workspace for local shells in Session security.").size(12)),
        ]
        .spacing(10)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::{clipboard, widget::operation::focusable};
    use iced::keyboard::{self, Key, Location, Modifiers, key};
    use iced::{Event, Renderer, Size, mouse};
    use iced_runtime::{UserInterface, user_interface::Cache};

    fn press_space(
        workspace: &Workspace,
        window: window::Id,
        target: &'static str,
    ) -> Vec<Desktop> {
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            workspace.settings_view(window).unwrap(),
            Size::new(620.0, 500.0),
            Cache::default(),
            &mut renderer,
        );
        interface.operate(&renderer, &mut focusable::focus(widget::Id::new(target)));
        let mut messages = Vec::new();
        interface.update(
            &[Event::Keyboard(keyboard::Event::KeyPressed {
                key: Key::Named(key::Named::Space),
                modified_key: Key::Named(key::Named::Space),
                physical_key: key::Physical::Unidentified(key::NativeCode::Unidentified),
                location: Location::Standard,
                modifiers: Modifiers::empty(),
                text: Some(" ".into()),
                repeat: false,
            })],
            mouse::Cursor::Unavailable,
            &mut renderer,
            &mut clipboard::Null,
            &mut messages,
        );
        messages
    }

    #[test]
    fn security_switches_support_keyboard_without_applying_the_draft() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Security));
        let id = workspace.settings.for_owner(owner).unwrap();
        let messages = press_space(&workspace, id, "security-startup-password");
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Startup(target, startup_settings::Edit::MasterPassword(true)))] if *target == id)
        );
        for message in messages {
            let _ = workspace.update(message);
        }
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .startup
                .require_master_password
        );
        assert!(!workspace.window_settings.startup.require_master_password);

        let messages = press_space(&workspace, id, "security-temporary-key");
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::TemporarySshKey(target, true))] if *target == id)
        );
        for message in messages {
            let _ = workspace.update(message);
        }
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .temporary_ssh_key_enabled
        );
        assert!(!workspace.window_settings.temporary_ssh_key_enabled);
        let messages = press_space(&workspace, id, "settings-save");
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Save(target))] if *target == id)
        );
        let messages = press_space(&workspace, id, "settings-cancel");
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Cancel(target))] if *target == id)
        );
        for message in messages {
            let _ = workspace.update(message);
        }
        assert!(workspace.settings.0.is_none());
        assert!(!workspace.window_settings.startup.require_master_password);
        assert!(!workspace.window_settings.temporary_ssh_key_enabled);
    }

    #[test]
    fn managed_security_switches_cannot_be_activated_from_keyboard() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Security));
        let id = workspace.settings.for_owner(owner).unwrap();
        workspace.clipboard.policy.require_master_password = true;
        assert!(press_space(&workspace, id, "security-startup-password").is_empty());
        workspace.clipboard.policy.problem = Some("Test policy unavailable".into());
        assert!(press_space(&workspace, id, "security-temporary-key").is_empty());
    }

    #[test]
    fn security_search_focuses_the_matching_action_or_switch() {
        use crate::settings_search::test_input;
        for (query, expected) in [
            (
                "require master password at startup",
                Setting::StartupMasterPassword,
            ),
            ("enable temporary ssh key option", Setting::TemporarySshKey),
            ("change master password", Setting::ChangeMasterPassword),
        ] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.open_settings(owner, Some(Section::Font));
            let id = workspace.settings.for_owner(owner).unwrap();
            let _ = workspace.update_settings(Message::Search(id, query.into()));
            let reveal = workspace.update_settings(Message::SearchChoose(id));
            assert_eq!(
                workspace.settings.0.as_ref().unwrap().revealed,
                Some(expected)
            );
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                Size::new(850.0, 680.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(&mut interface, &renderer, reveal);
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(key::Named::Space),
                None,
            );
            assert!(match (expected, messages.as_slice()) {
                (
                    Setting::StartupMasterPassword,
                    [
                        Desktop::Settings(Message::Startup(
                            target,
                            startup_settings::Edit::MasterPassword(true),
                        )),
                    ],
                ) => *target == id,
                (
                    Setting::TemporarySshKey,
                    [Desktop::Settings(Message::TemporarySshKey(target, true))],
                ) => *target == id,
                (
                    Setting::ChangeMasterPassword,
                    [Desktop::Settings(Message::ChangeMasterPassword(target))],
                ) => *target == id,
                _ => false,
            });
        }
    }

    #[test]
    fn security_search_retains_search_focus_for_a_managed_switch() {
        use crate::settings_search::test_input;
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        let search = workspace.settings.0.as_ref().unwrap().search_input.clone();
        workspace.clipboard.policy.require_master_password = true;
        let _ = workspace.update_settings(Message::Search(
            id,
            "require master password at startup".into(),
        ));
        let reveal = workspace.update_settings(Message::SearchChoose(id));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            workspace.settings_view(id).unwrap(),
            Size::new(850.0, 680.0),
            Cache::default(),
            &mut renderer,
        );
        interface.operate(&renderer, &mut focusable::focus(search));
        test_input::apply(&mut interface, &renderer, reveal);
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Character("x".into()),
            Some("x"),
        );
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::Search(target, value))] if *target == id && value == "x")
        );
    }

    #[test]
    fn security_isolation_search_opens_the_keyboard_menu_and_enforces_the_minimum() {
        use crate::settings_search::test_input;
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        workspace.clipboard.policy.minimum_isolation = IsolationLevel::Process;
        let _ = workspace.update_settings(Message::Search(id, "default isolation".into()));
        let reveal = workspace.update_settings(Message::SearchChoose(id));
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            workspace.settings_view(id).unwrap(),
            Size::new(850.0, 680.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        assert!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(key::Named::Space),
                None
            )
            .is_empty()
        );
        assert!(
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(key::Named::ArrowDown),
                None
            )
            .is_empty()
        );
        let messages = test_input::press(
            &mut interface,
            &mut renderer,
            Key::Named(key::Named::Enter),
            None,
        );
        assert!(
            matches!(messages.as_slice(), [Desktop::Settings(Message::DefaultIsolation(target, IsolationLevel::Sandbox))] if *target == id)
        );
        drop(interface);
        for message in messages {
            let _ = workspace.update(message);
        }
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .default_session_isolation,
            Some(IsolationLevel::Sandbox)
        );
        assert_eq!(workspace.window_settings.default_session_isolation, None);
    }
}
