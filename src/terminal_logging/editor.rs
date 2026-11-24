use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::futures::future::BoxFuture;
use iced::widget::{Id, button, checkbox, column, pick_list, row, rule, space, text, text_input};
use iced::{Alignment, Element, Font};

use crate::workspace::dialog_design::{self, keyboard_table, number_input};

use super::{
    DirectoryMode, Edit, Form, Format, MAX_LOG_MIB, MAX_RETENTION_DAYS, filename, valid_folder,
};

const LABEL_WIDTH: f32 = 174.0;

#[derive(Clone, Copy, Default)]
pub(crate) struct ManagedFields {
    pub(crate) include_username: Option<bool>,
    pub(crate) directory_mode: Option<DirectoryMode>,
}

impl ManagedFields {
    pub(crate) fn allows(self, edit: &Edit) -> bool {
        match edit {
            Edit::Username(_) => self.include_username.is_none(),
            Edit::Directory(_) => self.directory_mode.is_none(),
            _ => true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum Action {
    Edit(Edit),
    Browse,
}

pub(super) struct Controls {
    format: Id,
    directory: Id,
    picker: Option<Arc<()>>,
}

impl Default for Controls {
    fn default() -> Self {
        Self {
            format: Id::unique(),
            directory: Id::unique(),
            picker: None,
        }
    }
}

impl Form {
    pub(crate) fn start_folder_picker(
        &mut self,
        eligible: bool,
        profile_root: &Path,
    ) -> Option<(Arc<()>, PathBuf)> {
        if !eligible || !self.config.enabled || self.editor.picker.is_some() {
            return None;
        }
        let ticket = Arc::new(());
        self.editor.picker = Some(ticket.clone());
        Some((ticket, self.config.directory(profile_root)))
    }

    pub(crate) fn finish_folder_picker(
        &mut self,
        ticket: &Arc<()>,
        eligible: bool,
        path: Option<&Path>,
    ) -> Option<Edit> {
        if !Arc::ptr_eq(self.editor.picker.as_ref()?, ticket) {
            return None;
        }
        self.editor.picker = None;
        let path = path.filter(|path| eligible && self.config.enabled && valid_folder(path))?;
        let path = path.to_str()?;
        (path != self.config.folder && path.len() <= 4096 && !path.chars().any(char::is_control))
            .then(|| Edit::Folder(path.to_owned()))
    }

    pub(crate) fn accepts_editor_edit(&self, eligible: bool, edit: &Edit) -> bool {
        eligible && (self.config.enabled || matches!(edit, Edit::Enabled(_)))
    }

    pub(crate) fn editor_view(
        &self,
        eligible: bool,
        managed: ManagedFields,
    ) -> Element<'_, Action> {
        let enabled = eligible && self.config.enabled;
        let browse = (enabled && self.editor.picker.is_none()).then_some(Action::Browse);
        let include_username = managed
            .include_username
            .unwrap_or(self.config.include_username);
        let directory = managed.directory_mode.unwrap_or(self.config.directory_mode);
        column![
            check("Enable terminal logging", enabled, eligible.then_some(Edit::Enabled)),
            rule::horizontal(1),
            field(
                "Log directory",
                row![
                    text_input("Default folder in the application profile", &self.config.folder)
                        .width(300)
                        .on_input_maybe(enabled.then_some(|path| Action::Edit(Edit::Folder(path))))
                        .style(dialog_design::form_input),
                    keyboard_table::action(
                        button("Browse…")
                            .style(dialog_design::secondary_button)
                            .on_press_maybe(browse.clone())
                            .into(),
                        browse,
                        true,
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ),
            field("Format", picker(
                self.config.format,
                [Format::Text, Format::Xml, Format::Json],
                Edit::Format,
                enabled,
                self.editor.format.clone(),
                200.0,
            )),
            field(
                "Example filename",
                text(self.filename_preview()).font(Font::MONOSPACE).size(13).style(text::secondary),
            ),
            field("Maximum file size", quantity(
                &self.size,
                1..=i64::from(MAX_LOG_MIB),
                enabled.then_some(Edit::Size),
                "MiB",
            )),
            field("Retention", quantity(
                &self.retention,
                0..=i64::from(MAX_RETENTION_DAYS),
                enabled.then_some(Edit::Retention),
                "days (0 = keep everything)",
            )),
            indented(managed_row(
                check(
                    "Include SSH username",
                    include_username,
                    (enabled && managed.include_username.is_none()).then_some(Edit::Username),
                ),
                managed.include_username.is_some(),
            )),
            field("Directory in log entries", managed_row(
                picker(
                    directory,
                    [DirectoryMode::None, DirectoryMode::Full, DirectoryMode::Relative],
                    Edit::Directory,
                    enabled && managed.directory_mode.is_none(),
                    self.editor.directory.clone(),
                    260.0,
                ),
                managed.directory_mode.is_some(),
            )),
            indented(check("Rotate daily", self.config.rotate_daily, enabled.then_some(Edit::Daily))),
            indented(check("Compress closed logs (gzip)", self.config.compress, enabled.then_some(Edit::Compress))),
            (!eligible).then(|| text("Terminal logging is unavailable for this session.").size(13)),
            text("Automatic retention applies in the private default folder. Known SSH credentials and managed patterns are masked.")
                .size(13).style(text::secondary),
        ]
        .spacing(10)
        .into()
    }

    fn filename_preview(&self) -> String {
        let name = filename("2026-01-15-09-30-00-ssh_1", 1, self.config.format);
        if self.config.compress {
            super::compressed_filename(&name)
        } else {
            name
        }
    }
}

fn field<'a>(label: &'static str, control: impl Into<Element<'a, Action>>) -> Element<'a, Action> {
    row![text(label).size(14).width(LABEL_WIDTH), control.into()]
        .align_y(Alignment::Center)
        .spacing(8)
        .into()
}

fn indented(control: Element<'_, Action>) -> Element<'_, Action> {
    row![space::horizontal().width(LABEL_WIDTH), control]
        .align_y(Alignment::Center)
        .spacing(8)
        .into()
}

fn managed_row(control: Element<'_, Action>, managed: bool) -> Element<'_, Action> {
    row![
        control,
        managed.then(|| text("Managed by policy").size(13).style(text::secondary)),
    ]
    .align_y(Alignment::Center)
    .spacing(8)
    .wrap()
    .into()
}

fn check<'a>(
    label: &'static str,
    value: bool,
    edit: Option<fn(bool) -> Edit>,
) -> Element<'a, Action> {
    let control = checkbox(value)
        .label(label)
        .text_size(13)
        .on_toggle_maybe(edit);
    keyboard_table::action(
        Element::from(control).map(Action::Edit),
        edit.map(|edit| Action::Edit(edit(!value))),
        false,
    )
}

fn quantity<'a>(
    value: &'a str,
    bounds: std::ops::RangeInclusive<i64>,
    edit: Option<fn(String) -> Edit>,
    unit: &'static str,
) -> Element<'a, Action> {
    row![
        number_input(
            text_input("", value).on_input_maybe(edit),
            value,
            bounds,
            1,
            edit,
            74.0
        )
        .map(Action::Edit),
        text(unit).size(13),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn picker<'a, T: Copy + Eq + std::fmt::Display + 'a>(
    value: T,
    options: [T; 3],
    edit: fn(T) -> Edit,
    enabled: bool,
    id: Id,
    width: f32,
) -> Element<'a, Action> {
    if enabled {
        pick_list(options, Some(value), move |value| Action::Edit(edit(value)))
            .width(width)
            .keyboard_navigation(id)
            .into()
    } else {
        button(text(value.to_string()))
            .width(width)
            .style(dialog_design::secondary_button)
            .into()
    }
}

pub(crate) fn pick_folder(
    owner: &dyn iced::window::Window,
    directory: PathBuf,
) -> BoxFuture<'static, Option<PathBuf>> {
    let dialog = rfd::AsyncFileDialog::new()
        .set_parent(owner)
        .set_title("Choose terminal log directory")
        .set_directory(directory)
        .pick_folder();
    Box::pin(async move { dialog.await.map(|folder| folder.path().to_owned()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_each_writer_extension_and_archive_suffix() {
        let mut form = Form::new(Default::default());
        for (format, extension) in [
            (Format::Text, "log"),
            (Format::Xml, "xml"),
            (Format::Json, "json"),
        ] {
            form.update(Edit::Format(format));
            let example = format!("2026-01-15-09-30-00-ssh_1.{extension}");
            form.update(Edit::Compress(false));
            assert_eq!(form.filename_preview(), example);
            form.update(Edit::Compress(true));
            assert_eq!(form.filename_preview(), format!("{example}.gz"));
        }
    }

    #[test]
    fn chooser_only_updates_the_enabled_draft_with_the_matching_ticket() {
        let root = std::env::temp_dir();
        let folder = root.join("korTTY logs");
        let mut form = Form::new(Default::default());
        assert!(form.start_folder_picker(true, &root).is_none());
        form.update(Edit::Enabled(true));
        assert!(form.start_folder_picker(false, &root).is_none());
        let (cancelled, directory) = form.start_folder_picker(true, &root).unwrap();
        assert_eq!(directory, root.join("terminal-logs"));
        assert!(form.start_folder_picker(true, &root).is_none());
        assert!(form.finish_folder_picker(&cancelled, true, None).is_none());
        let (current, _) = form.start_folder_picker(true, &root).unwrap();
        assert!(
            form.finish_folder_picker(&cancelled, true, Some(&folder))
                .is_none()
        );
        assert!(form.config.custom_folder().is_none());
        let edit = form
            .finish_folder_picker(&current, true, Some(&folder))
            .unwrap();
        assert!(form.config.custom_folder().is_none());
        form.update(edit);
        assert_eq!(
            form.config().unwrap().custom_folder(),
            Some(folder.as_path())
        );
    }

    #[test]
    fn policy_values_lock_only_the_managed_fields() {
        let rule =
            "[logging]\nterminal-include-username = false\nterminal-directory-mode = 'relative'"
                .parse::<toml_edit::DocumentMut>()
                .unwrap();
        let mut policy = super::super::Policy::default();
        policy.read_rule(rule.as_table()).unwrap();
        let managed = policy.managed_terminal_fields();
        assert_eq!(managed.include_username, Some(false));
        assert_eq!(managed.directory_mode, Some(DirectoryMode::Relative));
        assert!(!managed.allows(&Edit::Username(true)));
        assert!(!managed.allows(&Edit::Directory(DirectoryMode::Full)));
        assert!(managed.allows(&Edit::Format(Format::Xml)));
    }
}
