//! Vault authentication for the startup gate.

use super::*;

pub(crate) struct StartupPassword {
    vault: VaultWindow,
    input: iced::widget::Id,
    managed: bool,
    cancelled: bool,
    remembered: bool,
    recovery_key_changed: bool,
}

pub(crate) enum StartupResult {
    Wait(Task<Message>),
    Focus,
    Ready,
    Exit,
}

impl StartupPassword {
    pub(crate) fn open(path: PathBuf, managed: bool) -> Result<Self, String> {
        Ok(Self {
            vault: VaultWindow::open(path)?,
            input: iced::widget::Id::unique(),
            managed,
            cancelled: false,
            remembered: false,
            recovery_key_changed: false,
        })
    }

    pub(crate) fn input(&self) -> iced::widget::Id {
        self.input.clone()
    }

    pub(crate) fn try_remembered(&mut self) -> Task<Message> {
        self.remembered = true;
        self.vault.try_remembered()
    }

    pub(crate) fn used_remembered(&self) -> bool {
        self.remembered
    }

    pub(crate) fn update(&mut self, message: Message) -> StartupResult {
        match message {
            Message::Close => {
                self.cancelled = true;
                if self.vault.busy() {
                    StartupResult::Wait(Task::none())
                } else {
                    StartupResult::Exit
                }
            }
            Message::Completed(reply) if self.vault.busy() => {
                self.recovery_key_changed |= reply.recovery_key_changed();
                let task = self.vault.update(Message::Completed(reply));
                if self.vault.busy() {
                    StartupResult::Wait(task)
                } else if self.cancelled {
                    StartupResult::Exit
                } else if self.vault.is_unlocked() {
                    StartupResult::Ready
                } else {
                    if self.remembered {
                        self.remembered = false;
                        self.vault.status = Some(
                            "Automatic unlock was unavailable. Enter your master password.".into(),
                        );
                    }
                    StartupResult::Focus
                }
            }
            message @ (Message::Primary(_) | Message::Confirm(_) | Message::Unlock)
                if !self.cancelled =>
            {
                StartupResult::Wait(self.vault.update(message))
            }
            _ => StartupResult::Wait(Task::none()),
        }
    }

    pub(crate) fn into_unlocked(self) -> (VaultWindow, bool) {
        (self.vault, self.recovery_key_changed)
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        let vault = &self.vault;
        let submit =
            (!vault.busy() && !self.cancelled && vault.can_open()).then_some(Message::Unlock);
        let mut contents = iced::widget::column![
            text(if vault.exists {
                "Unlock the vault to start kortty"
            } else {
                "Create a vault to start kortty"
            })
            .size(21),
            text(if self.managed {
                "Your organization requires a master password at startup."
            } else {
                "kortty needs access to your credential vault."
            })
            .wrapping(text::Wrapping::WordOrGlyph),
        ]
        .spacing(10)
        .padding(16);

        if self.cancelled {
            contents = contents.push(text("Cancelling…"));
        } else if vault.busy() {
            contents = contents.push(text(if self.remembered {
                "Checking the device credential store…"
            } else if vault.exists {
                "Unlocking…"
            } else {
                "Creating vault…"
            }));
        } else {
            contents = contents.push(
                text_input("Master password", &vault.primary)
                    .id(self.input.clone())
                    .secure(true)
                    .on_input(|value| Message::Primary(SecretEdit::new(value)))
                    .on_submit_maybe(submit.clone()),
            );
            if !vault.exists {
                contents = contents
                    .push(
                        text_input("Repeat master password", &vault.confirm)
                            .secure(true)
                            .on_input(|value| Message::Confirm(SecretEdit::new(value)))
                            .on_submit_maybe(submit.clone()),
                    )
                    .push(
                        text(format!(
                            "Use at least {MIN_MASTER_PASSWORD_BYTES} UTF-8 bytes."
                        ))
                        .size(13),
                    );
            }
            if let Some(status) = &vault.status {
                contents =
                    contents.push(text(status).size(13).wrapping(text::Wrapping::WordOrGlyph));
            }
        }

        scrollable(
            contents.push(
                row![
                    space::horizontal(),
                    button(if vault.exists {
                        "Unlock"
                    } else {
                        "Create vault"
                    })
                    .on_press_maybe(submit),
                    button("Cancel").on_press_maybe((!self.cancelled).then_some(Message::Close)),
                ]
                .spacing(8)
                .wrap(),
            ),
        )
        .into()
    }
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt as _, executor::block_on};

    use super::*;

    const MASTER: &str = "native startup fixture";

    fn submit(prompt: &mut StartupPassword) -> Task<Message> {
        let StartupResult::Wait(task) = prompt.update(Message::Unlock) else {
            panic!("authentication should run before the startup decision");
        };
        task
    }

    fn complete(task: Task<Message>) -> Message {
        let mut stream =
            iced_runtime::task::into_stream(task).expect("vault operation is scheduled");
        match block_on(stream.next()) {
            Some(iced_runtime::Action::Output(message @ Message::Completed(_))) => message,
            _ => panic!("the vault operation did not complete"),
        }
    }

    fn primary(prompt: &mut StartupPassword, password: &str) {
        assert!(matches!(
            prompt.update(Message::Primary(SecretEdit::new(password.into()))),
            StartupResult::Wait(_)
        ));
    }

    #[test]
    fn required_vault_setup_and_unlock_use_the_encrypted_profile_vault() {
        let profile = crate::snippets::tests::Profile::new();
        let path = crate::profile::vault_path_at(&profile.0);
        let mut prompt = StartupPassword::open(path.clone(), true).unwrap();
        primary(&mut prompt, MASTER);
        let _ = prompt.update(Message::Confirm(SecretEdit::new(
            "different password".into(),
        )));
        assert!(iced_runtime::task::into_stream(submit(&mut prompt)).is_none());
        assert!(!path.exists());
        let _ = prompt.update(Message::Confirm(SecretEdit::new(MASTER.into())));
        let task = submit(&mut prompt);
        assert!(!prompt.vault.is_unlocked());
        assert!(matches!(
            prompt.update(complete(task)),
            StartupResult::Ready
        ));
        let (vault, recovery_key_changed) = prompt.into_unlocked();
        assert!(vault.is_unlocked() && recovery_key_changed);
        drop(vault);

        let mut prompt = StartupPassword::open(path.clone(), true).unwrap();
        primary(&mut prompt, "wrong master password");
        let task = submit(&mut prompt);
        assert!(matches!(
            prompt.update(complete(task)),
            StartupResult::Focus
        ));
        assert!(!prompt.vault.is_unlocked());
        primary(&mut prompt, MASTER);
        let task = submit(&mut prompt);
        assert!(matches!(
            prompt.update(complete(task)),
            StartupResult::Ready
        ));
        let (vault, recovery_key_changed) = prompt.into_unlocked();
        assert!(vault.is_unlocked() && !recovery_key_changed);
    }

    #[test]
    fn failed_remembered_unlock_leaves_the_typed_password_gate_active() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("test-vault.bin");
        drop(Vault::create(&path, MASTER.as_bytes()).unwrap());
        let mut prompt = StartupPassword::open(path.clone(), false).unwrap();
        let task = prompt.try_remembered();
        assert!(matches!(
            prompt.update(complete(task)),
            StartupResult::Focus
        ));
        assert!(!prompt.used_remembered());
        assert!(
            prompt
                .vault
                .status
                .as_deref()
                .is_some_and(|status| status.contains("Enter your master password"))
        );
        primary(&mut prompt, MASTER);
        let task = submit(&mut prompt);
        assert!(matches!(
            prompt.update(complete(task)),
            StartupResult::Ready
        ));
        assert!(!prompt.used_remembered());
    }

    #[test]
    fn cancelling_an_in_flight_unlock_prevents_startup_even_if_the_password_is_correct() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("test-vault.bin");
        drop(Vault::create(&path, MASTER.as_bytes()).unwrap());
        let mut prompt = StartupPassword::open(path, true).unwrap();
        primary(&mut prompt, MASTER);
        let task = submit(&mut prompt);
        assert!(matches!(
            prompt.update(Message::Close),
            StartupResult::Wait(_)
        ));
        assert!(matches!(prompt.update(complete(task)), StartupResult::Exit));
    }
}
