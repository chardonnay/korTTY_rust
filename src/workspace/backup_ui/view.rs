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
        Some(dialog.view(recipient).map(move |message| wrap(id, message)))
    }
}

impl Dialog {
    fn view(&self, recipient: Option<String>) -> Element<'_, Message> {
        let content = if self.discard.is_some() {
            discard()
        } else {
            match &self.phase {
                Phase::Form => self.form(recipient),
                Phase::Browse(browser) => self.browser(browser),
                Phase::Working(work) => working(*work, self.encryption()),
                Phase::Review(preview) => self.review(preview),
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

    fn review<'a>(&'a self, preview: &'a Preview) -> Element<'a, Message> {
        let count = preview.count();
        let format = if self.encryption() == EncryptionMode::OpenPgp {
            "Integrity-checked OpenPGP backup"
        } else {
            "Authenticated backup"
        };
        let includes_vault = preview.entries().any(|entry| entry == "vault/vault.bin");
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
            scrollable(container(entries).padding(8)).height(Length::Fill),
            row![button("Back").on_press(Message::Back), space::horizontal(),
                button("Cancel").on_press(Message::Cancel),
                button(if replacing { "Replace on Restart" } else { "Merge on Restart" })
                    .style(if replacing { button::danger } else { button::primary })
                    .on_press(Message::Restore(self.revision)),
            ].spacing(8),
        ].spacing(12).height(Length::Fill).into()
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
