use super::*;
use iced::widget::column;

#[derive(Clone, Debug, Eq, PartialEq)]
struct PasswordSource(Option<CredentialChoice>);

impl fmt::Display for PasswordSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(credential) => credential.fmt(formatter),
            None => formatter.write_str("No stored credential"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct KeySource(Option<KeyChoice>);

impl fmt::Display for KeySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(key) => key.fmt(formatter),
            None => formatter.write_str("Use a private key file"),
        }
    }
}

impl Login {
    pub(super) fn temporary_key_feedback(
        &self,
        vault: Option<&crate::vault_window::VaultWindow>,
    ) -> &'static str {
        if self.temporary.key.is_some() {
            return "Private key pasted. Save to verify and store it.";
        }
        let Some(id) = self.temporary.credential.as_deref() else {
            return "Paste a private key for this SSH account.";
        };
        let Some(vault) = vault.filter(|vault| vault.is_unlocked()) else {
            return "Unlock the credential vault to check the saved key.";
        };
        if self
            .endpoint()
            .is_ok_and(|endpoint| vault.has_temporary_ssh_key(id, &endpoint))
        {
            "The saved key is available for this SSH account."
        } else {
            "The saved key expired, is missing, or belongs to another SSH account. Paste a replacement."
        }
    }

    pub(super) fn saved_authentication_sources(
        &self,
        choices: &ViewChoices<'_>,
        enabled: bool,
        browse_enabled: bool,
    ) -> Element<'_, Message> {
        let password_enabled = enabled && matches!(self.method, Method::Ask | Method::Vault);
        let key_enabled = enabled && matches!(self.method, Method::Key | Method::StoredKey);
        let file_enabled = enabled && self.method == Method::Key;
        let edit = |edit| Message::Edit(Edit::Login(false, edit));
        let (passwords, selected_password) =
            CredentialChoice::options(choices.passwords, &self.password, false);
        let password = PasswordSource(selected_password);
        let password_control = if password_enabled {
            pick_list(
                std::iter::once(PasswordSource(None))
                    .chain(
                        passwords
                            .into_iter()
                            .map(|choice| PasswordSource(Some(choice))),
                    )
                    .collect::<Vec<_>>(),
                Some(password),
                move |source| edit(LoginEdit::PasswordSource(source.0)),
            )
            .width(300)
            .text_size(13)
            .keyboard_navigation(self.password_source_picker.clone())
            .into()
        } else {
            read_only(password.to_string())
        };
        let selected_key = KeyChoice::selected(choices.keys, self.stored_key);
        let mut keys = choices.keys.to_vec();
        if let Some(key) = &selected_key
            && !keys.iter().any(|choice| choice.id == key.id)
        {
            keys.push(key.clone());
        }
        let key = KeySource(selected_key);
        let key_control = if key_enabled {
            pick_list(
                std::iter::once(KeySource(None))
                    .chain(keys.into_iter().map(|choice| KeySource(Some(choice))))
                    .collect::<Vec<_>>(),
                Some(key),
                move |source| edit(LoginEdit::KeySource(source.0)),
            )
            .width(300)
            .text_size(13)
            .keyboard_navigation(self.key_source_picker.clone())
            .into()
        } else {
            read_only(key.to_string())
        };
        let (passphrases, passphrase) =
            CredentialChoice::options(choices.passphrases, &self.passphrase, true);
        let passphrase_control = if file_enabled {
            pick_list(passphrases, passphrase, move |choice| {
                edit(LoginEdit::Passphrase(choice))
            })
            .width(300)
            .text_size(13)
            .keyboard_navigation(self.credential_picker.clone())
            .into()
        } else {
            read_only(passphrase.map_or_else(String::new, |choice| choice.to_string()))
        };
        column![
            login_row("Stored credentials", password_control),
            login_row(
                "Password",
                self.inline_password_field(false, password_enabled)
            ),
            (enabled && self.method != Method::TemporaryKey && !choices.vault_unlocked).then(
                || login_row(
                    "",
                    keyboard_table::action(
                        button(text("Open Credential Vault…").size(13))
                            .style(dialog_design::secondary_button)
                            .on_press(Message::OpenVault)
                            .into(),
                        Some(Message::OpenVault),
                        true,
                    ),
                )
            ),
            login_row("Stored SSH keys", key_control),
            login_row(
                "Key file",
                iced::widget::container(self.key_file_field(false, file_enabled, browse_enabled))
                    .max_width(430),
            ),
            login_row("Stored passphrase", passphrase_control),
            login_row(
                "Passphrase",
                self.inline_passphrase_field(false, file_enabled)
            ),
        ]
        .spacing(10)
        .into()
    }

    pub(super) fn inline_password_field(&self, jump: bool, enabled: bool) -> Element<'_, Message> {
        let hint = if self.password.is_empty() {
            "Enter password"
        } else {
            "Stored password (unchanged)"
        };
        text_input(hint, self.inline_password.as_str())
            .id(self.inline_password_field.clone())
            .secure(true)
            .size(13)
            .width(300)
            .on_input_maybe(enabled.then_some(move |value| {
                Message::Edit(Edit::Login(
                    jump,
                    LoginEdit::InlinePassword(Password::new(value)),
                ))
            }))
            .into()
    }

    pub(super) fn inline_passphrase_field(
        &self,
        jump: bool,
        enabled: bool,
    ) -> Element<'_, Message> {
        let hint = if self.passphrase.is_empty() {
            "Enter key passphrase"
        } else {
            "Stored passphrase (unchanged)"
        };
        text_input(hint, self.inline_passphrase.as_str())
            .id(self.inline_passphrase_field.clone())
            .secure(true)
            .size(13)
            .width(300)
            .on_input_maybe(enabled.then_some(move |value| {
                Message::Edit(Edit::Login(
                    jump,
                    LoginEdit::InlinePassphrase(Password::new(value)),
                ))
            }))
            .into()
    }
}

fn read_only(value: String) -> Element<'static, Message> {
    button(row![
        text(value).size(13).width(Length::Fill),
        text("▾").size(13)
    ])
    .width(300)
    .style(dialog_design::secondary_button)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_temporary_key_feedback_checks_the_current_vault_and_exact_account() {
        use kortty_vault::{Secret, TemporarySshKeyOptions, Vault};

        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("vault.bin");
        let master = crate::vault_window::test_vault_password();
        let mut vault = Vault::create(&path, &master[..]).unwrap();
        let id = vault
            .save_temporary_ssh_key(
                "Feedback fixture".into(),
                ("auth.example", 22, "root"),
                TemporarySshKeyOptions {
                    minutes: 1,
                    automatic: true,
                },
                Secret::new("-----BEGIN OPENSSH PRIVATE KEY-----\nZmFrZQ==\n-----END OPENSSH PRIVATE KEY-----".into()),
            )
            .unwrap();
        let window = crate::vault_window::VaultWindow::with_unlocked_fixture(path, vault);
        let mut login = login();
        login.temporary.credential = Some(id);
        assert_eq!(
            login.temporary_key_feedback(None),
            "Unlock the credential vault to check the saved key."
        );
        assert_eq!(
            login.temporary_key_feedback(Some(&window)),
            "The saved key is available for this SSH account."
        );
        for edit in [
            LoginEdit::Host("other.example".into()),
            LoginEdit::Port("2222".into()),
            LoginEdit::Username("other-user".into()),
        ] {
            login.host = "auth.example".into();
            login.port = "22".into();
            login.username = "root".into();
            login.update(edit);
            assert!(
                login
                    .temporary_key_feedback(Some(&window))
                    .contains("Paste a replacement")
            );
        }
        login.temporary.credential = Some("missing-fixture".into());
        assert!(
            login
                .temporary_key_feedback(Some(&window))
                .contains("Paste a replacement")
        );
        login.temporary.key = Some(Password::new("pending-fixture".into()));
        assert_eq!(
            login.temporary_key_feedback(Some(&window)),
            "Private key pasted. Save to verify and store it."
        );
        login.temporary.key = None;
        login.temporary.credential = None;
        assert_eq!(
            login.temporary_key_feedback(Some(&window)),
            "Paste a private key for this SSH account."
        );
    }

    fn login() -> Login {
        let mut login = Login::new();
        login.host = "auth.example".into();
        login.key_path = std::env::temp_dir()
            .join("private-key")
            .to_string_lossy()
            .into_owned();
        login
    }

    #[test]
    fn inline_secrets_are_redacted_bounded_and_replaced_by_explicit_sources() {
        let mut login = login();
        let change = LoginEdit::InlinePassword(Password::new("  private password  ".into()));
        assert!(!format!("{change:?}").contains("private password"));
        login.update(change);
        assert_eq!(login.inline_password.as_str(), "  private password  ");
        assert!(
            !serde_json::to_string(&login.saved().unwrap())
                .unwrap()
                .contains("private password")
        );
        login.update(LoginEdit::InlinePassword(Password::new("x".repeat(65_537))));
        assert_eq!(login.inline_password.as_str(), "  private password  ");
        login.update(LoginEdit::PasswordSource(Some(CredentialChoice {
            id: "selected-password".into(),
            label: "Stored".into(),
            username: String::new(),
        })));
        assert!(login.inline_password.as_str().is_empty());
        assert_eq!(login.password, "selected-password");
        login.update(LoginEdit::Method(Method::Key));
        login.update(LoginEdit::InlinePassword(Password::new("disabled".into())));
        assert!(login.inline_password.as_str().is_empty());
        login.update(LoginEdit::InlinePassphrase(Password::new(
            " key phrase ".into(),
        )));
        assert_eq!(login.inline_passphrase.as_str(), " key phrase ");
        login.update(LoginEdit::Passphrase(CredentialChoice {
            id: "selected-passphrase".into(),
            label: "Stored".into(),
            username: String::new(),
        }));
        assert!(login.inline_passphrase.as_str().is_empty());
    }

    #[test]
    fn disabled_jump_credentials_are_not_queued_and_empty_fields_keep_references() {
        let mut draft = Draft::new();
        draft.update(Edit::UseJump(true));
        draft.update(Edit::Login(
            true,
            LoginEdit::InlinePassword(Password::new("jump secret".into())),
        ));
        draft.update(Edit::UseJump(false));
        assert!(draft.pending_inline_credentials().is_empty());
        draft.update(Edit::UseJump(true));
        assert!(!draft.pending_inline_credentials().is_empty());
        let mut saved = login().saved().unwrap();
        saved.auth = Auth::VaultPassword("existing-password".into());
        let reopened = Login::from_saved(&saved, false);
        assert!(reopened.inline_password.as_str().is_empty());
        assert_eq!(reopened.saved().unwrap(), saved);
        let mut edited = reopened;
        edited.update(LoginEdit::InlinePassword(Password::new(
            "replacement".into(),
        )));
        edited.update(LoginEdit::InlinePassword(Password::default()));
        assert_eq!(edited.saved().unwrap(), saved);
        saved.auth = Auth::KeyFile {
            path: std::env::temp_dir().join("private-key"),
            passphrase: Some("existing-passphrase".into()),
        };
        saved.preferences.key_file.clear();
        let mut edited = Login::from_saved(&saved, false);
        edited.update(LoginEdit::InlinePassphrase(Password::new(
            "replacement".into(),
        )));
        edited.update(LoginEdit::InlinePassphrase(Password::default()));
        assert_eq!(edited.saved().unwrap(), saved);
    }

    #[test]
    fn inactive_file_and_password_preferences_survive_serialization_and_reselection() {
        let mut login = login();
        login.passphrase = "passphrase-reference".into();
        login.update(LoginEdit::PasswordSource(Some(CredentialChoice {
            id: "password-reference".into(),
            label: "Account".into(),
            username: "operator".into(),
        })));
        let saved = login.saved().unwrap();
        let saved = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
        let mut reopened = Login::from_saved(&saved, false);
        assert_eq!(reopened.key_path, login.key_path);
        assert_eq!(reopened.passphrase, "passphrase-reference");
        reopened.update(LoginEdit::Method(Method::Key));
        let key = reopened.saved().unwrap();
        assert!(matches!(key.auth, Auth::KeyFile { .. }));
        assert_eq!(
            key.preferences.password.as_deref(),
            Some("password-reference")
        );
        let mut reopened = Login::from_saved(&key, false);
        reopened.update(LoginEdit::Method(Method::Vault));
        assert_eq!(
            reopened.saved().unwrap().auth,
            Auth::VaultPassword("password-reference".into())
        );
    }

    #[test]
    fn switching_away_from_temporary_keys_does_not_retain_their_secret_reference() {
        let mut login = login();
        login.temporary.credential = Some("temporary-reference".into());
        login.temporary.minutes = "15".into();
        login.temporary.automatic = true;
        login.update(LoginEdit::Method(Method::Interactive));
        let saved = login.saved().unwrap();
        assert_eq!(saved.auth, Auth::KeyboardInteractive);
        assert!(
            !String::from_utf8(serde_json::to_vec(&saved).unwrap())
                .unwrap()
                .contains("temporary-reference")
        );
        let duplicate = Login::from_saved(&saved, true);
        assert!(duplicate.temporary.credential.is_none());
    }

    #[test]
    fn inactive_metadata_cannot_override_an_active_login_on_reopen() {
        let mut saved = login().saved().unwrap();
        saved.preferences.password = Some("unselected-password".into());
        saved.preferences.keyboard_interactive = true;
        let reopened = Login::from_saved(&saved, false);
        assert_eq!(reopened.method, Method::Ask);
        assert!(reopened.password.is_empty());
        assert!(!reopened.keyboard_interactive);
        assert_eq!(reopened.saved().unwrap().auth, Auth::Prompt);

        saved.auth = Auth::KeyFile {
            path: std::env::temp_dir().join("active-key"),
            passphrase: None,
        };
        let reopened = Login::from_saved(&saved, false);
        assert_eq!(reopened.method, Method::Key);
        assert_eq!(reopened.stored_key, None);
        assert!(matches!(
            reopened.saved().unwrap().auth,
            Auth::KeyFile { .. }
        ));
    }

    #[test]
    fn password_source_selects_the_saved_credential_and_can_return_to_prompting() {
        let mut login = login();
        login.update(LoginEdit::PasswordSource(Some(CredentialChoice {
            id: "saved-password".into(),
            label: "Operations".into(),
            username: "operator".into(),
        })));
        assert_eq!(login.method, Method::Vault);
        assert_eq!(login.username, "operator");
        assert_eq!(
            login.saved().unwrap().auth,
            Auth::VaultPassword("saved-password".into())
        );
        login.update(LoginEdit::PasswordSource(None));
        assert_eq!(login.method, Method::Ask);
        assert_eq!(login.saved().unwrap().auth, Auth::Prompt);
        assert_eq!(
            PathBuf::from(&login.key_path),
            std::env::temp_dir().join("private-key")
        );
    }

    #[test]
    fn key_source_selects_registered_keys_without_losing_the_file_draft() {
        let mut login = login();
        login.update(LoginEdit::Method(Method::Key));
        login.update(LoginEdit::KeySource(Some(KeyChoice {
            id: 7,
            name: "Operations key".into(),
        })));
        assert_eq!(login.method, Method::StoredKey);
        assert_eq!(login.saved().unwrap().auth, Auth::StoredKey(7));
        login.update(LoginEdit::KeySource(None));
        assert_eq!(login.method, Method::Key);
        assert!(
            matches!(login.saved().unwrap().auth, Auth::KeyFile { path, .. } if path == std::env::temp_dir().join("private-key"))
        );
    }

    #[test]
    fn disabled_source_callbacks_do_not_change_the_active_authentication() {
        let mut login = login();
        login.update(LoginEdit::KeySource(Some(KeyChoice {
            id: 7,
            name: "Key".into(),
        })));
        assert_eq!(login.method, Method::Ask);
        assert_eq!(login.stored_key, None);
        login.update(LoginEdit::Method(Method::Interactive));
        login.update(LoginEdit::PasswordSource(Some(CredentialChoice {
            id: "saved-password".into(),
            label: "Operations".into(),
            username: "operator".into(),
        })));
        assert_eq!(login.method, Method::Interactive);
        assert_eq!(login.username, "root");
        assert!(login.password.is_empty());
    }

    #[test]
    fn source_pickers_select_credentials_and_keys_with_the_keyboard() {
        use crate::settings_search::test_input;
        use iced::advanced::widget::operation::focusable;
        use iced::keyboard::{Key, key::Named};
        use iced::{Font, Renderer, Size};
        use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

        let themes = crate::terminal_theme::Themes::default();
        let passwords = [CredentialChoice {
            id: "saved-password".into(),
            label: "Operations".into(),
            username: "operator".into(),
        }];
        let keys = [KeyChoice {
            id: 7,
            name: "Operations key".into(),
        }];
        let choices = ViewChoices {
            passwords: &passwords,
            passphrases: &[],
            keys: &keys,
            temporary_keys: false,
            vault_unlocked: true,
            temporary_key_vault: None,
            ai_profiles: &[],
            java_ai_profiles: &[],
            ai_skills: &[],
            ai_agent_designs: &[],
            highlighting: Default::default(),
            themes: &themes,
            terminal_style: Default::default(),
            journal_policy: Default::default(),
            logging_policy: Default::default(),
            incognito_allowed: true,
            serial_allowed: true,
            terminal_effects: None,
            default_paste_warning: Default::default(),
            default_paste_line_delay_ms: 0,
            minimum_paste_warning: PasteWarningMode::Off,
            isolation: Default::default(),
            local_isolation_default: Default::default(),
        };
        for method in [Method::Ask, Method::Key] {
            let mut login = login();
            login.update(LoginEdit::Method(method));
            let id = if method == Method::Ask {
                login.password_source_picker.clone()
            } else {
                login.key_source_picker.clone()
            };
            let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                login.saved_authentication_sources(&choices, true, true),
                Size::new(640.0, 400.0),
                Cache::default(),
                &mut renderer,
            );
            test_input::apply(
                &mut interface,
                &renderer,
                task::effect(Action::widget(focusable::focus(id))),
            );
            for key in [Named::Space, Named::ArrowDown] {
                assert!(
                    test_input::press(&mut interface, &mut renderer, Key::Named(key), None)
                        .is_empty()
                );
            }
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Enter),
                None,
            );
            let [Message::Edit(Edit::Login(false, change))] = messages.as_slice() else {
                panic!("The source picker did not select exactly one item");
            };
            let change = change.clone();
            drop(interface);
            login.update(change);
            assert_eq!(
                login.method,
                if method == Method::Ask {
                    Method::Vault
                } else {
                    Method::StoredKey
                }
            );
        }
    }
}
