use iced::widget::{
    button, checkbox, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Alignment, Element, Length};

use super::*;

impl Workspace {
    pub(in crate::workspace) fn backup_view(&self, id: window::Id) -> Option<Element<'_, Desktop>> {
        let dialog = self.backup.0.as_ref().filter(|dialog| dialog.id == id)?;
        let recipient = if matches!(dialog.phase, Phase::Form) {
            dialog
                .recipient
                .as_deref()
                .and_then(|fingerprint| self.backup_recipient_label(fingerprint))
        } else {
            None
        };
        let vault_ready = self
            .vault
            .as_ref()
            .is_some_and(VaultWindow::can_manage_environments);
        Some(
            dialog
                .view(
                    recipient,
                    vault_ready,
                    self.clipboard.policy.problem.is_none(),
                )
                .map(move |message| wrap(id, message)),
        )
    }
}

impl Dialog {
    fn view(
        &self,
        recipient: Option<String>,
        vault_ready: bool,
        host_import_allowed: bool,
    ) -> Element<'_, Message> {
        let content = if self.discard.is_some() {
            discard()
        } else if let Some(review) = &self.host_pins {
            self.host_pin_view(review, host_import_allowed)
        } else {
            match &self.phase {
                Phase::Form => self.form(recipient),
                Phase::Browse(browser) => self.browser(browser),
                Phase::Working(work) => working(*work, self.encryption()),
                Phase::Review(preview) => self.review(preview, vault_ready, host_import_allowed),
                Phase::Exported(archive) => self.exported(archive),
                Phase::Queued(result) => queued(result),
            }
        };
        container(content)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn form(&self, recipient: Option<String>) -> Element<'_, Message> {
        let exporting = self.kind == Kind::Export;
        let encryption = self.encryption();
        let header = if exporting {
            "Select Backup Destination"
        } else {
            "Select Backup File to Import"
        };
        let placeholder = if exporting {
            "Absolute path and filename for the backup"
        } else {
            "Absolute path to the encrypted backup"
        };
        let recipient_available =
            recipient.is_some() || !exporting || encryption == EncryptionMode::Password;
        let submit = (self.can_submit() && recipient_available).then_some(Message::Submit);
        let detecting = self.needs_detection();
        let path = row![
            text_input(placeholder, &self.path)
                .on_input(Message::Path)
                .on_submit_maybe(detecting.then_some(submit.clone()).flatten()),
            button("Browse…").on_press(Message::Browse),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let format = (!exporting).then(|| {
            row![
                text("Archive Format").width(142),
                pick_list(
                    ImportFormat::ALL,
                    Some(self.import_format),
                    Message::ImportFormat
                ),
                (self.import_format == ImportFormat::Detect)
                    .then_some(self.detected_format)
                    .flatten()
                    .map(|format| text(format!("Detected: {format}")).size(13)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        });
        let fields = (!detecting).then(|| match (self.kind, encryption) {
            (_, EncryptionMode::Password) => self.password_fields(submit.clone()),
            (Kind::Import, EncryptionMode::OpenPgp) => self.private_key_fields(submit.clone()),
            (Kind::Export, EncryptionMode::OpenPgp) => column![
                text("OpenPGP Encryption"),
                text(recipient.map_or_else(
                    || "Select an available OpenPGP recipient in Global Settings → Backup.".into(),
                    |recipient| format!("Recipient: {recipient}"),
                ))
                .size(14),
                text("Keep access to the recipient’s private key to restore this backup.").size(13),
            ]
            .spacing(8)
            .into(),
        });
        column![
            text(header).size(23),
            path,
            format,
            fields,
            (exporting
                && self
                    .input_path()
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == current_name(encryption)))
            .then(|| text("An existing kortty backup at this path moves to old-backups.").size(13)),
            self.error.as_ref().map(|error| text(error).size(13)),
            space::vertical(),
            row![
                space::horizontal(),
                button("Cancel").on_press(Message::Cancel),
                button(if exporting {
                    "Create Backup"
                } else if detecting {
                    "Detect Backup Format"
                } else {
                    "Review Backup"
                })
                .on_press_maybe(submit),
            ]
            .spacing(8),
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }

    fn password_fields(&self, submit: Option<Message>) -> Element<'_, Message> {
        let exporting = self.kind == Kind::Export;
        let password = text_input("Backup password", &self.password)
            .secure(true)
            .on_input(|value| Message::Password(Password::new(value)))
            .on_submit_maybe((!exporting).then_some(submit.clone()).flatten());
        let using_saved = self.selected_password().is_some();
        let repeat = (exporting && !using_saved).then(|| {
            row![
                text("Repeat Password").width(142),
                text_input("Repeat backup password", &self.repeat)
                    .secure(true)
                    .on_input(|value| Message::Repeat(Password::new(value)))
                    .on_submit_maybe(submit.clone()),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        });
        let help = if using_saved {
            "The archive password is read from the unlocked vault."
        } else if exporting && !self.repeat.is_empty() && self.password != self.repeat {
            "The backup passwords do not match."
        } else if exporting {
            "Use a backup password of 12 to 1024 UTF-8 bytes."
        } else if self.resolved_import() == Some(BackupFormat::JavaPassword) {
            "Enter the password used to create the Java backup ZIP."
        } else {
            "Enter the password to authenticate the backup and review its contents."
        };
        column![
            self.saved_password.as_ref().map(|_| row![
                checkbox(self.use_saved_password)
                    .label("Use saved archive password")
                    .on_toggle(Message::UseSavedPassword),
                using_saved.then(|| button("Vault…").on_press(Message::OpenVault)),
            ]
            .spacing(8)
            .align_y(Alignment::Center)),
            (!using_saved).then(|| row![text("Backup Password").width(142), password]
                .spacing(8)
                .align_y(Alignment::Center)),
            repeat,
            text(help).size(13),
        ]
        .spacing(12)
        .into()
    }

    fn private_key_fields(&self, submit: Option<Message>) -> Element<'_, Message> {
        column![
            row![
                text("OpenPGP Private Key").width(142),
                text_input("Absolute path to the private key", &self.private_key)
                    .on_input(Message::PrivateKeyPath),
                button("Browse…").on_press(Message::BrowsePrivateKey),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            row![
                text("Key Passphrase").width(142),
                text_input("Private key passphrase (optional)", &self.password)
                    .secure(true)
                    .on_input(|value| Message::Password(Password::new(value)))
                    .on_submit_maybe(submit),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text("Choose the recipient’s private key to decrypt and review this backup.").size(13),
        ]
        .spacing(12)
        .into()
    }

    fn browser<'a>(&'a self, browser: &'a Browser) -> Element<'a, Message> {
        let mut list = column![].spacing(2);
        let count = browser
            .directory
            .as_ref()
            .map_or(0, |directory| directory.entries.len());
        if let Some(directory) = &browser.directory {
            for (index, entry) in directory
                .entries
                .iter()
                .enumerate()
                .skip(browser.page * PAGE_SIZE)
                .take(PAGE_SIZE)
            {
                let name = if entry.kind == EntryKind::Directory {
                    format!("{}/", entry.display)
                } else {
                    entry.display.clone()
                };
                let selectable = entry.kind == EntryKind::Directory
                    || (self.kind == Kind::Import && entry.kind == EntryKind::File);
                list = list.push(
                    button(text(name))
                        .style(button::text)
                        .width(Length::Fill)
                        .on_press_maybe(
                            selectable.then_some(Message::BrowseEntry(self.revision, index)),
                        ),
                );
            }
        } else if browser.loading {
            list = list.push(text("Loading folder…"));
        }
        column![
            text(self.title()).size(23),
            row![
                button("Parent folder").on_press_maybe(
                    (!browser.loading && browser.folder.parent().is_some())
                        .then_some(Message::BrowseParent)
                ),
                text(browser.folder.to_string_lossy()).size(14),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            scrollable(list).height(Length::Fill),
            self.error.as_ref().map(|error| text(error).size(13)),
            row![
                (count > PAGE_SIZE).then(|| page_buttons(browser.page, count)),
                space::horizontal(),
                button("Back").on_press(Message::Back),
                (self.kind == Kind::Export).then(|| button("Use This Folder").on_press_maybe(
                    browser
                        .directory
                        .is_some()
                        .then_some(Message::UseFolder(self.revision))
                )),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .spacing(9)
        .height(Length::Fill)
        .into()
    }

    fn review<'a>(
        &'a self,
        preview: &'a Preview,
        vault_ready: bool,
        host_import_allowed: bool,
    ) -> Element<'a, Message> {
        if let Some((format, imported, omissions)) = preview.java_review() {
            return self.review_java(
                format,
                imported,
                omissions,
                preview,
                vault_ready,
                host_import_allowed,
            );
        }
        let count = preview.count();
        let format = if self.encryption() == EncryptionMode::OpenPgp {
            "Integrity-checked OpenPGP backup"
        } else {
            "Authenticated backup"
        };
        let includes_vault = preview.entries().any(|entry| entry == "vault/vault.bin");
        let includes_ai = preview
            .entries()
            .any(|entry| entry == crate::ai_manager::backup::SETTINGS);
        let replacing = self.restore_mode == RestoreMode::Replace;
        let description = if replacing {
            "The portable profile settings will be replaced by the contents of this backup."
        } else {
            "Existing files stay in place. Missing files are added where they can be restored with their required credentials and keys."
        };
        let mut entries = column![].spacing(3);
        for entry in preview.entries().take(MAX_VISIBLE_PREVIEW_ENTRIES) {
            entries = entries.push(text(entry).font(iced::Font::MONOSPACE).size(13));
        }
        if count > MAX_VISIBLE_PREVIEW_ENTRIES {
            entries = entries.push(
                text(format!(
                    "… and {} more",
                    count - MAX_VISIBLE_PREVIEW_ENTRIES
                ))
                .size(13),
            );
        }
        column![
            text("Review Backup").size(23),
            text(format!("{format} · {count} profile file(s)")),
            row![
                text("Import mode"),
                pick_list(
                    [RestoreMode::KeepExisting, RestoreMode::Replace],
                    Some(self.restore_mode),
                    Message::RestoreMode,
                ),
            ].spacing(8).align_y(Alignment::Center),
            text(description),
            includes_vault.then(|| text("If the credential vault is restored, unlock it with its original master password.").size(13)),
            includes_ai.then(|| text("Review restored AI endpoints, tools and knowledge sources before use. Model files are stored separately.").size(13)),
            scrollable(container(entries).padding(8)).height(Length::Fill),
            row![button("Back").on_press(Message::Back), space::horizontal(),
                button("Cancel").on_press(Message::Cancel),
                button(if replacing { "Replace on Restart" } else { "Merge on Restart" })
                    .style(if replacing { button::danger } else { button::primary })
                    .on_press(Message::Restore(self.revision)),
            ].spacing(8),
        ].spacing(12).height(Length::Fill).into()
    }

    fn review_java<'a>(
        &'a self,
        format: &'a str,
        imported: &'a [String],
        omissions: &'a [String],
        preview: &'a Preview,
        vault_ready: bool,
        host_import_allowed: bool,
    ) -> Element<'a, Message> {
        let mut entries = column![].spacing(4);
        for entry in imported
            .iter()
            .chain(omissions.iter())
            .take(MAX_VISIBLE_PREVIEW_ENTRIES)
        {
            entries = entries.push(text(entry).size(13));
        }
        let hidden = (imported.len() + omissions.len()).saturating_sub(MAX_VISIBLE_PREVIEW_ENTRIES);
        if hidden > 0 {
            entries = entries.push(text(format!("… and {hidden} more")).size(13));
        }
        let can_merge = preview.count() > 0;
        let has_host_pins = preview.java_host_pins().is_some();
        let imported_host_pins = self.imported_host_pins.as_deref();
        let review_credentials =
            self.offers_java_credential_review(JavaCredentialReview::WithVault);
        let review_keys =
            self.offers_java_credential_review(JavaCredentialReview::KeysWithoutPassphrases);
        let password_summary = preview.java_password_summary();
        let key_summary = preview.java_key_summary();
        let merge_label = match (
            review_keys,
            review_credentials && preview.has_java_passwords(),
        ) {
            (true, true) => "Merge Without SSH Credentials",
            (true, false) => "Merge Without SSH Keys",
            (false, true) => "Merge Without Passwords",
            (false, false) => "Merge on Restart",
        };
        column![
            text("Review Java Backup").size(23),
            text(format),
            text(if can_merge {
                "Add supported data on restart. Existing profiles, snippets, variables, themes and projects take precedence on conflicts."
            } else if review_credentials {
                "Review the available SSH credentials to continue."
            } else if has_host_pins {
                "SSH host keys are imported separately after you confirm their fingerprints."
            } else {
                "This backup contains no Java data that this version can import."
            }),
            review_keys.then(|| row![
                text("Keys without passphrases can be reviewed without unlocking the vault.")
                    .size(13).width(Length::Fill),
                button("Review Keys Without Passphrases").on_press(Message::ReviewJavaCredentials(
                    self.revision,
                    JavaCredentialReview::KeysWithoutPassphrases,
                )),
            ].spacing(8).align_y(Alignment::Center)),
            review_credentials.then(|| self.java_credential_fields(vault_ready, review_keys)),
            password_summary.map(|(ready, omitted)| text(format!("SSH passwords ready to add: {ready}. Passwords omitted: {omitted}."))),
            key_summary.map(|(ready, omitted)| text(format!("Managed SSH keys ready to add: {ready}. Keys omitted: {omitted}."))),
            has_host_pins.then(|| button("Review SSH Host Keys…").on_press_maybe(host_import_allowed.then_some(Message::ReviewHostPins(self.revision)))),
            imported_host_pins.map(|report| text(format!("SSH host keys added: {}. Already trusted: {}. Conflicts: {}. Need fresh review: {}.", report.added.len(), report.already_trusted, report.conflicts.len(), report.needs_review)).size(13)),
            self.error.as_ref().map(|error| text(error).size(13)),
            scrollable(container(entries).padding(8)).height(Length::Fill),
            row![button("Back").on_press(Message::Back), space::horizontal(),
                button(if !can_merge && imported_host_pins.is_some() { "Done" } else { "Cancel" }).on_press(Message::Cancel),
                can_merge.then(|| button(merge_label)
                    .style(button::primary)
                    .on_press(Message::Restore(self.revision))),
            ].spacing(8),
        ].spacing(12).height(Length::Fill).into()
    }

    fn java_credential_fields(&self, vault_ready: bool, review_keys: bool) -> Element<'_, Message> {
        if !vault_ready {
            return row![
                text(if review_keys {
                    "Unlock the credential vault to include SSH passwords and keys with passphrases."
                } else {
                    "Unlock the credential vault to review the Java SSH passwords."
                }).size(13).width(Length::Fill),
                button("Credential Vault…").on_press(Message::OpenVault),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .into();
        }
        let revision = self.revision;
        let mode = JavaCredentialReview::WithVault;
        let submit = self
            .can_review_java_credentials(revision, mode)
            .then_some(Message::ReviewJavaCredentials(revision, mode));
        column![
            text(if review_keys {
                "Use the Java korTTY master password to include SSH passwords and keys with passphrases."
            } else {
                "Use the Java korTTY master password to review which SSH passwords can be imported."
            }).size(13),
            row![
                text_input("Java Master Password", &self.java_master_password)
                    .secure(true)
                    .on_input(move |value| Message::JavaMasterPassword(revision, Password::new(value)))
                    .on_submit_maybe(submit.clone()),
                button(if review_keys { "Review SSH Credentials" } else { "Review SSH Passwords" })
                    .on_press_maybe(submit),
            ].spacing(8).align_y(Alignment::Center),
        ].spacing(5).into()
    }

    fn exported<'a>(&self, archive: &'a Exported) -> Element<'a, Message> {
        let format = if self.encryption() == EncryptionMode::OpenPgp {
            "an OpenPGP-encrypted archive"
        } else {
            "an encrypted ZIP"
        };
        column![
            text("Backup successfully created").size(23),
            text(format!("{} profile file(s) in {format}.", archive.count)),
            text(archive.path.to_string_lossy().into_owned()).size(14),
            archive
                .warning
                .as_ref()
                .map(|warning| text(warning).size(13)),
            space::vertical(),
            row![
                space::horizontal(),
                button("Close").on_press(Message::Cancel)
            ],
        ]
        .spacing(12)
        .height(Length::Fill)
        .into()
    }
}

fn queued(result: &Queued) -> Element<'static, Message> {
    let count = result.count;
    column![
        text("Restart required").size(23),
        text(format!(
            "The reviewed backup contains {count} profile file(s)."
        )),
        text(format!("Import mode: {}", result.mode)),
        column(
            result
                .scheduler_notes
                .iter()
                .map(|note| text(note.clone()).size(13).into())
        )
        .spacing(3),
        text("Quit kortty and start it again to apply the backup."),
        (!result.durable).then(|| {
            text("Backup restore was queued. Directory synchronization could not be confirmed.")
                .size(13)
        }),
        space::vertical(),
        row![
            space::horizontal(),
            button("Close").on_press(Message::Cancel),
            button("Quit kortty").on_press(Message::Quit)
        ]
        .spacing(8),
    ]
    .spacing(12)
    .height(Length::Fill)
    .into()
}

fn working(action: Work, encryption: EncryptionMode) -> Element<'static, Message> {
    let message = match action {
        Work::Detect => "Identifying backup format…",
        Work::Export => "Creating backup…",
        Work::Inspect if encryption == EncryptionMode::OpenPgp => "Decrypting OpenPGP backup…",
        Work::Inspect => "Authenticating backup…",
        Work::JavaCredentials => "Reviewing Java SSH credentials…",
        Work::Queue => "Preparing restore…",
    };
    column![text(message).size(23), space::vertical()]
        .height(Length::Fill)
        .into()
}

fn discard() -> Element<'static, Message> {
    column![
        text("Discard backup changes?").size(23),
        space::vertical(),
        row![
            space::horizontal(),
            button("Keep Editing").on_press(Message::KeepEditing),
            button("Discard")
                .style(button::danger)
                .on_press(Message::Discard),
        ]
        .spacing(8),
    ]
    .spacing(12)
    .height(Length::Fill)
    .into()
}

fn page_buttons(page: usize, count: usize) -> Element<'static, Message> {
    let pages = count.div_ceil(PAGE_SIZE);
    row![
        button("‹").on_press_maybe((page > 0).then_some(Message::BrowsePage(false))),
        text(format!("{} / {pages}", page + 1)).size(13),
        button("›").on_press_maybe((page + 1 < pages).then_some(Message::BrowsePage(true))),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .into()
}
