use std::path::{Path, PathBuf};

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Alignment, Element, Length};

use crate::snippets::MAX_NAME_BYTES;
use crate::snippets::recipients::{Action, CertificateInfo, Change, Choice};

const MAX_FINGERPRINT_BYTES: usize = 128;
const MAX_CERTIFICATE_PATH_BYTES: usize = 8192;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub enum Message {
    Select(Choice),
    New,
    Edit,
    Name(String),
    Fingerprint(String),
    Certificate(String),
    Browse,
    PreviewCertificate,
    PreviewFinished(u64, Result<CertificateInfo, String>),
    Save,
    Discard,
    AskDelete,
    Delete,
    CancelDelete,
    Reload,
    Done,
    KeepEditing,
    DiscardAndClose,
    Finished(Result<Vec<Choice>, String>),
}

#[derive(Default)]
struct Form {
    previous: Option<Choice>,
    name: String,
    fingerprint: String,
    certificate: String,
    invalid_certificate_path: bool,
    preview: Option<CertificateInfo>,
}

impl Form {
    fn for_choice(choice: Choice) -> Self {
        Self {
            name: choice.name().to_owned(),
            fingerprint: choice.fingerprint().to_owned(),
            previous: Some(choice),
            ..Self::default()
        }
    }

    fn dirty(&self) -> bool {
        self.name != self.previous.as_ref().map_or("", Choice::name)
            || self.fingerprint != self.previous.as_ref().map_or("", Choice::fingerprint)
            || !self.certificate.is_empty()
    }

    fn can_save(&self) -> bool {
        if !self.dirty() || self.name.trim().is_empty() || self.invalid_certificate_path {
            return false;
        }
        if self.certificate.is_empty() {
            return self.previous.is_some();
        }
        let Some(preview) = self.preview.as_ref() else {
            return false;
        };
        if let Some(previous) = &self.previous {
            !previous.revoked() && previous.fingerprint() == preview.fingerprint
        } else {
            !preview.revoked
                && self
                    .fingerprint
                    .bytes()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .map(|byte| byte.to_ascii_uppercase())
                    .eq(preview.fingerprint.bytes())
        }
    }

    fn certificate_path(&self) -> Option<&Path> {
        let path = Path::new(&self.certificate);
        (path.is_absolute()
            && path.file_name().is_some()
            && !self.certificate.chars().any(char::is_control))
        .then_some(path)
    }

    fn clear_preview(&mut self) {
        self.preview = None;
        if self.previous.is_none() {
            self.fingerprint.clear();
        }
    }

    fn change(&self) -> Change {
        Change {
            previous: self.previous.clone(),
            name: self.name.clone(),
            fingerprint: self.fingerprint.clone(),
            certificate: self.certificate_path().map(Path::to_path_buf),
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Operation {
    Load,
    Save,
    Delete,
    Preview(u64),
}

pub(crate) struct Page {
    choices: Vec<Choice>,
    selected: Option<String>,
    form: Option<Form>,
    loaded: bool,
    pending: Option<Operation>,
    preview_revision: u64,
    confirm_delete: Option<Choice>,
    confirm_close: bool,
    error: Option<String>,
}

impl Page {
    pub(crate) fn loading() -> Self {
        Self {
            choices: Vec::new(),
            selected: None,
            form: None,
            loaded: false,
            pending: Some(Operation::Load),
            preview_revision: 0,
            confirm_delete: None,
            confirm_close: false,
            error: None,
        }
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn dirty(&self) -> bool {
        self.form.as_ref().is_some_and(Form::dirty)
    }

    pub(crate) fn request_close(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        self.confirm_close = self.dirty();
        !self.confirm_close
    }

    pub(crate) fn discard_and_close(&mut self) -> bool {
        if !self.confirm_close || self.busy() {
            return false;
        }
        self.confirm_close = false;
        self.form = None;
        true
    }

    pub(crate) fn cancel_inner(&mut self) -> bool {
        if self.busy() || self.confirm_close {
            return false;
        }
        if self.confirm_delete.take().is_some() {
            return true;
        }
        if self.form.is_some() && !self.dirty() {
            self.form = None;
            self.error = None;
            return true;
        }
        false
    }

    pub(crate) fn can_browse_certificate(&self) -> bool {
        !self.busy()
            && !self.confirm_close
            && self.form.as_ref().is_some_and(|form| {
                form.previous
                    .as_ref()
                    .is_none_or(|choice| !choice.revoked())
            })
    }

    pub(crate) fn certificate_path(&self) -> Option<&Path> {
        self.form.as_ref()?.certificate_path()
    }

    pub(crate) fn begin_preview(&mut self) -> Option<(u64, PathBuf)> {
        if !self.can_browse_certificate() {
            return None;
        }
        let path = self.certificate_path()?.to_path_buf();
        self.form.as_mut()?.clear_preview();
        self.preview_revision = self.preview_revision.wrapping_add(1);
        let revision = self.preview_revision;
        self.pending = Some(Operation::Preview(revision));
        self.error = None;
        Some((revision, path))
    }

    fn selected_choice(&self) -> Option<&Choice> {
        let fingerprint = self.selected.as_deref()?;
        self.choices
            .iter()
            .find(|choice| choice.fingerprint() == fingerprint)
    }

    fn finish_catalog(&mut self, result: Result<Vec<Choice>, String>) {
        let Some(operation @ (Operation::Load | Operation::Save | Operation::Delete)) =
            self.pending
        else {
            return;
        };
        self.pending = None;
        match result {
            Ok(choices) => {
                let selected = if operation == Operation::Save {
                    self.form.as_ref().and_then(|form| {
                        choices
                            .iter()
                            .find(|choice| choice.name() == form.name.trim())
                    })
                } else {
                    self.selected.as_deref().and_then(|fingerprint| {
                        choices
                            .iter()
                            .find(|choice| choice.fingerprint() == fingerprint)
                    })
                };
                self.selected = selected.map(|choice| choice.fingerprint().to_owned());
                self.choices = choices;
                self.form = None;
                self.loaded = true;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn finish_preview(&mut self, revision: u64, result: Result<CertificateInfo, String>) {
        if self.pending != Some(Operation::Preview(revision)) {
            return;
        }
        self.pending = None;
        match result {
            Ok(preview) => {
                if let Some(form) = &mut self.form {
                    form.preview = Some(preview);
                }
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn update(&mut self, message: Message, path: &Path) -> Option<Action> {
        match message {
            Message::Finished(result) => {
                self.finish_catalog(result);
                return None;
            }
            Message::PreviewFinished(revision, result) => {
                self.finish_preview(revision, result);
                return None;
            }
            _ if self.busy() => return None,
            Message::KeepEditing if self.confirm_close => {
                self.confirm_close = false;
                return None;
            }
            _ if self.confirm_close => return None,
            _ => {}
        }
        let (operation, action) = match message {
            Message::Delete => {
                let previous = self.confirm_delete.take()?;
                (
                    Operation::Delete,
                    Action::Delete(path.to_path_buf(), previous),
                )
            }
            Message::CancelDelete if self.confirm_delete.is_some() => {
                self.confirm_delete = None;
                return None;
            }
            _ if self.confirm_delete.is_some() => return None,
            Message::Reload if self.form.is_none() => {
                (Operation::Load, Action::List(path.to_path_buf()))
            }
            _ if !self.loaded => return None,
            Message::Save => {
                let form = self.form.as_ref().filter(|form| form.can_save())?;
                let action = if let Some(preview) = &form.preview {
                    Action::SaveReviewed(path.to_path_buf(), form.change(), preview.clone())
                } else {
                    Action::Save(path.to_path_buf(), form.change())
                };
                (Operation::Save, action)
            }
            Message::Select(choice) if self.form.is_none() && self.choices.contains(&choice) => {
                self.selected = Some(choice.fingerprint().to_owned());
                self.error = None;
                return None;
            }
            Message::New if self.form.is_none() => {
                self.form = Some(Form::default());
                self.error = None;
                return None;
            }
            Message::Edit if self.form.is_none() => {
                self.form = self.selected_choice().cloned().map(Form::for_choice);
                self.error = None;
                return None;
            }
            Message::Discard => {
                self.form = None;
                self.error = None;
                return None;
            }
            Message::Name(name) => {
                let form = self.form.as_mut()?;
                let (value, error) = bounded(name, MAX_NAME_BYTES, "Recipient name");
                form.name = value;
                self.error = error;
                return None;
            }
            Message::Fingerprint(fingerprint) => {
                let form = self.form.as_mut().filter(|form| {
                    form.previous.is_none()
                        && form
                            .preview
                            .as_ref()
                            .is_some_and(|preview| !preview.revoked)
                })?;
                let (value, error) = bounded(fingerprint, MAX_FINGERPRINT_BYTES, "Fingerprint");
                form.fingerprint = value;
                self.error = error;
                return None;
            }
            Message::Certificate(certificate) if self.can_browse_certificate() => {
                let form = self.form.as_mut()?;
                let (value, error) =
                    bounded(certificate, MAX_CERTIFICATE_PATH_BYTES, "Certificate path");
                if form.certificate != value {
                    form.certificate = value;
                    form.clear_preview();
                }
                form.invalid_certificate_path = error.is_some();
                self.error = error;
                return None;
            }
            Message::AskDelete if self.form.is_none() => {
                self.confirm_delete = self.selected_choice().cloned();
                self.error = None;
                return None;
            }
            _ => return None,
        };
        self.pending = Some(operation);
        self.error = None;
        Some(action)
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        if self.confirm_close {
            return column![
                text("Discard unsaved recipient edits?").size(21),
                text("The draft will be discarded when you close this window."),
                row![
                    button("Keep Editing").on_press(Message::KeepEditing),
                    button("Discard and Close")
                        .style(button::danger)
                        .on_press(Message::DiscardAndClose),
                ]
                .spacing(8),
            ]
            .padding(20)
            .spacing(12)
            .into();
        }
        if let Some(operation) = self.pending {
            let progress = match operation {
                Operation::Load => "Loading OpenPGP recipients…",
                Operation::Save => "Saving OpenPGP recipient…",
                Operation::Delete => "Removing OpenPGP recipient…",
                Operation::Preview(_) => "Inspecting public certificate…",
            };
            return column![text("OpenPGP recipients").size(22), text(progress)]
                .padding(20)
                .spacing(12)
                .into();
        }
        if let Some(choice) = &self.confirm_delete {
            return column![
                text("Remove saved OpenPGP recipient?").size(21),
                text(choice.name()),
                text(choice.fingerprint())
                    .font(iced::Font::MONOSPACE)
                    .size(13),
                row![
                    button("Cancel").on_press(Message::CancelDelete),
                    button("Remove Recipient")
                        .style(button::danger)
                        .on_press(Message::Delete),
                ]
                .spacing(8),
            ]
            .padding(20)
            .spacing(12)
            .into();
        }
        if let Some(form) = &self.form {
            return self.form_view(form);
        }
        self.list_view()
    }

    fn list_view(&self) -> Element<'_, Message> {
        let mut entries = column![].spacing(3);
        for choice in &self.choices {
            let selected = self.selected.as_deref() == Some(choice.fingerprint());
            let label = column![
                row![
                    text(choice.name()).width(Length::Fill),
                    choice.revoked().then(|| text("Revoked").size(13)),
                ]
                .align_y(Alignment::Center),
                text(format!(
                    "Key ID {}  ·  {}",
                    choice.key_id(),
                    choice.fingerprint()
                ))
                .font(iced::Font::MONOSPACE)
                .size(12),
            ]
            .spacing(4);
            entries = entries.push(
                button(label)
                    .width(Length::Fill)
                    .style(if selected {
                        button::primary
                    } else {
                        button::text
                    })
                    .on_press(Message::Select(choice.clone())),
            );
        }
        if self.choices.is_empty() {
            let empty = if self.loaded {
                "No OpenPGP recipients saved."
            } else {
                "The saved recipients could not be loaded."
            };
            entries = entries.push(text(empty).size(14));
        }
        let has_selection = self.selected_choice().is_some();
        column![
            text("OpenPGP recipients").size(22),
            text("Saved recipients"),
            container(scrollable(entries).height(Length::Fill))
                .style(container::bordered_box)
                .padding(8)
                .height(Length::Fill)
                .width(Length::Fill),
            row![
                button("Add…").on_press_maybe(self.loaded.then_some(Message::New)),
                button("Edit…").on_press_maybe(has_selection.then_some(Message::Edit)),
                button("Remove…").on_press_maybe(has_selection.then_some(Message::AskDelete)),
                button("Reload").on_press(Message::Reload),
                space::horizontal(),
                button("Done").on_press(Message::Done),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
            text(
                "Recipients are enrolled by a verified full fingerprint and a public certificate."
            )
            .size(13),
            self.error
                .as_deref()
                .map(|error| text(error).size(13).style(text::danger)),
        ]
        .padding(20)
        .spacing(12)
        .height(Length::Fill)
        .into()
    }

    fn form_view<'a>(&'a self, form: &'a Form) -> Element<'a, Message> {
        let editing = form.previous.is_some();
        let can_browse = self.can_browse_certificate();
        let matching_preview = form.preview.as_ref().filter(|preview| !preview.revoked);
        let can_confirm = !editing && matching_preview.is_some();
        let certificate = can_browse.then(|| {
            row![
                text_input(
                    if editing {
                        "Optional replacement public certificate"
                    } else {
                        "Absolute path to the public certificate"
                    },
                    &form.certificate,
                )
                .on_input(Message::Certificate),
                button("Browse…").on_press(Message::Browse),
                button("Inspect Certificate").on_press_maybe(
                    form.certificate_path()
                        .is_some()
                        .then_some(Message::PreviewCertificate),
                ),
            ]
            .spacing(8)
            .align_y(Alignment::Center)
        });
        let preview = form.preview.as_ref().map(|preview| {
            column![
                text("Certificate Preview").size(16),
                text(format!("Key ID {}", preview.key_id)).size(13),
                text(&preview.fingerprint).font(iced::Font::MONOSPACE).size(13),
                preview.user_id.as_ref().map(|user_id| {
                    text(format!("User ID (as stated by the certificate): {user_id}")).size(13)
                }),
                preview.revoked.then(|| text(
                    if editing {
                        "The certificate is revoked. Saving a matching replacement records its revocation."
                    } else {
                        "A revoked certificate cannot be enrolled."
                    }
                ).size(13).style(text::danger)),
                form.previous.as_ref().filter(|previous| {
                    previous.fingerprint() != preview.fingerprint
                }).map(|_| text("This certificate does not match the enrolled fingerprint.").size(13).style(text::danger)),
            ].spacing(5)
        });
        let body = column![
            text(if editing {
                "Edit OpenPGP Recipient"
            } else {
                "Import Public Certificate"
            })
            .size(22),
            text_input("Display name", &form.name).on_input(Message::Name),
            form.previous.as_ref().map(|previous| {
                text(format!("Enrolled fingerprint: {}", previous.fingerprint()))
                    .font(iced::Font::MONOSPACE)
                    .size(13)
            }),
            (!can_browse).then(|| text("This revoked certificate cannot be replaced.").size(13)),
            certificate,
            preview,
            can_confirm.then(|| column![
                text("Confirm the complete fingerprint independently with the recipient.").size(13),
                text_input("Independently verified full fingerprint", &form.fingerprint)
                    .on_input(Message::Fingerprint),
            ]
            .spacing(5)),
            self.error
                .as_deref()
                .map(|error| text(error).size(13).style(text::danger)),
            row![
                space::horizontal(),
                button("Cancel")
                    .style(button::secondary)
                    .on_press(Message::Discard),
                button("Save Recipient").on_press_maybe(form.can_save().then_some(Message::Save)),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        ]
        .padding(20)
        .spacing(12);
        scrollable(body).height(Length::Fill).into()
    }
}

fn bounded(value: String, max: usize, name: &str) -> (String, Option<String>) {
    if value.len() <= max {
        (value, None)
    } else {
        (
            String::new(),
            Some(format!("{name} exceeds {max} UTF-8 bytes.")),
        )
    }
}
