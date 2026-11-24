use super::*;
use iced::widget::{column, row};
use iced::{Alignment, Font};

pub(in crate::workspace) mod java;

#[derive(Clone, Copy, Debug)]
pub enum Number {
    Idle,
    Parallel,
    Compression,
}

impl Number {
    const ALL: [Self; 3] = [Self::Idle, Self::Parallel, Self::Compression];

    fn setting(self) -> Setting {
        match self {
            Self::Idle => Setting::SftpIdleMinutes,
            Self::Parallel => Setting::SftpParallel,
            Self::Compression => Setting::SftpArchiveCompression,
        }
    }

    fn bounds(self, policy: SftpConflictPolicy) -> RangeInclusive<i64> {
        match self {
            Self::Idle => 1..=i64::from(profile::RemoteProjectTool::MAX_IDLE_MINUTES),
            Self::Parallel => 1..=i64::from(policy.parallel_cap),
            Self::Compression => 0..=9,
        }
    }

    fn editable(self, preferences: &WindowSettings, policy: SftpConflictPolicy) -> bool {
        match self {
            Self::Idle => preferences.sftp_idle_minutes.is_some(),
            Self::Parallel => policy.transfer_allowed && policy.parallel_cap > 1,
            Self::Compression => true,
        }
    }
}

#[derive(Default)]
pub(super) struct Numbers {
    invalid: [Option<String>; Number::ALL.len()],
    disabled_idle_minutes: Option<u16>,
}

impl Numbers {
    pub(super) fn clear(&mut self, number: Number) {
        self.invalid[number as usize] = None;
    }

    pub(super) fn first_problem(
        &self,
        preferences: &WindowSettings,
        policy: SftpConflictPolicy,
    ) -> Option<Setting> {
        Number::ALL.into_iter().find_map(|number| {
            (number.editable(preferences, policy) && self.invalid[number as usize].is_some())
                .then(|| number.setting())
        })
    }

    pub(super) fn update(
        &mut self,
        preferences: &mut WindowSettings,
        policy: SftpConflictPolicy,
        number: Number,
        mut input: String,
    ) {
        if !number.editable(preferences, policy) {
            return;
        }
        input.truncate(input.floor_char_boundary(16));
        let Some(value) = input
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|value| number.bounds(policy).contains(value))
        else {
            self.invalid[number as usize] = Some(input);
            return;
        };
        self.clear(number);
        match number {
            Number::Idle => preferences.sftp_idle_minutes = Some(value as u16),
            Number::Parallel => preferences.sftp_parallel_transfers = value as u8,
            Number::Compression => preferences.sftp_archive.level = value as u8,
        }
    }

    pub(super) fn set_idle_enabled(&mut self, preferences: &mut WindowSettings, enabled: bool) {
        if enabled {
            preferences
                .sftp_idle_minutes
                .get_or_insert(self.disabled_idle_minutes.unwrap_or(10));
        } else if let Some(minutes) = preferences.sftp_idle_minutes.take() {
            self.disabled_idle_minutes = Some(minutes);
            self.clear(Number::Idle);
        }
    }

    fn view<'a>(
        &self,
        dialog: &'a Dialog,
        number: Number,
        policy: SftpConflictPolicy,
    ) -> Element<'a, Desktop> {
        let id = dialog.id;
        let preferences = &dialog.draft.preferences;
        let setting = number.setting();
        let enabled = number.editable(preferences, policy);
        let value = match number {
            Number::Idle => preferences
                .sftp_idle_minutes
                .or(self.disabled_idle_minutes)
                .unwrap_or(10)
                .to_string(),
            Number::Parallel => preferences
                .sftp_parallel_transfers
                .min(policy.parallel_cap)
                .to_string(),
            Number::Compression => preferences.sftp_archive.level.to_string(),
        };
        let invalid = self.invalid[number as usize].as_deref().filter(|_| enabled);
        let value = invalid.unwrap_or(&value);
        let bounds = number.bounds(policy);
        let edit = enabled
            .then_some(move |input| Desktop::Settings(Message::SftpNumber(id, number, input)));
        let input =
            dialog.reveal_input(setting, enabled, text_input("", value).on_input_maybe(edit));
        let problem = invalid.map(|_| {
            help(format!(
                "Enter a whole number from {} to {}.",
                bounds.start(),
                bounds.end()
            ))
        });
        let input = super::super::dialog_design::number_input(input, value, bounds, 1, edit, 54.0);
        highlighted(
            dialog,
            setting,
            column![labeled(setting, input), problem].spacing(5),
        )
    }
}

impl Workspace {
    pub(super) fn sftp_settings_page<'a>(&self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let id = dialog.id;
        let preferences = &dialog.draft.preferences;
        let policy = &self.clipboard.policy;
        let conflict_policy = dialog.installed_sftp_conflict_policy.restrict(policy);
        let cap = conflict_policy.parallel_cap;
        let allowed = conflict_policy.transfer_allowed;
        let effective_conflict_default = conflict_policy
            .managed
            .unwrap_or(preferences.sftp_conflict_default);
        let conflict_default: Element<'a, Desktop> = if conflict_policy.editable() {
            pick_list(
                ConflictDefault::ALL,
                Some(effective_conflict_default),
                move |value| Desktop::Settings(Message::SftpConflictDefault(id, value)),
            )
            .into()
        } else {
            text(effective_conflict_default.to_string()).into()
        };
        let conflict_lock_reason = if conflict_policy.managed.is_some() {
            Some("Managed by your organization.")
        } else if !conflict_policy.editable() {
            Some("Locked by file transfer policy.")
        } else {
            None
        };
        let archive = &preferences.sftp_archive;
        let directory = dialog.reveal_input(
            Setting::SftpArchive,
            true,
            text_input("Current remote directory", &archive.remote_directory)
                .on_input(move |path| Desktop::Settings(Message::SftpArchiveDirectory(id, path)))
                .width(250)
                .style(super::super::dialog_design::form_input),
        );
        column![
            heading("SFTP Manager Settings"),
            highlighted(dialog, Setting::SftpIdle,
                checkbox(preferences.sftp_idle_minutes.is_some()).label(Setting::SftpIdle.label())
                    .on_toggle(move |enabled| Desktop::Settings(Message::SftpIdleEnabled(id, enabled)))),
            dialog.sftp_numbers.view(dialog, Number::Idle, conflict_policy),
            help("Applies to detached windows and tabs. A project timeout takes precedence. Active transfers, reviews and remote editors keep the connection open."),
            rule::horizontal(1),
            heading("Transfers"),
            dialog.sftp_numbers.view(dialog, Number::Parallel, conflict_policy),
            highlighted(dialog, Setting::SftpConflictDefault, column![
                labeled(Setting::SftpConflictDefault, conflict_default),
                conflict_lock_reason.map(help),
            ].spacing(5)),
            highlighted(dialog, Setting::SftpResume,
                checkbox(preferences.sftp_resume_partial_transfers).label(Setting::SftpResume.label())
                    .on_toggle_maybe(allowed.then_some(move |enabled| Desktop::Settings(Message::SftpResume(id, enabled))))),
            highlighted(dialog, Setting::SftpKeepPartial,
                checkbox(preferences.sftp_keep_partial_on_cancel).label(Setting::SftpKeepPartial.label())
                    .on_toggle_maybe((allowed && preferences.sftp_resume_partial_transfers).then_some(move |enabled| Desktop::Settings(Message::SftpKeepPartial(id, enabled))))),
            (!allowed).then(|| help("File transfers are disabled by policy.")),
            (allowed && cap < crate::sftp_window::parallel::MAX_PARALLEL).then(|| help(format!("Organization limit · {cap} parallel {}", if cap == 1 { "transfer" } else { "transfers" }))),
            help("Attached SSH sessions use at most two transfer lanes. Transfers to the same destination stay ordered. Turning off resume releases retained partial transfers."),
            rule::horizontal(1),
            heading("ZIP Creation Settings"),
            highlighted(dialog, Setting::SftpArchive, column![
                labeled(Setting::SftpArchive, directory),
                (!archive.valid()).then(|| help("Enter an absolute remote directory with ordinary path components.")),
            ].spacing(5)),
            dialog.sftp_numbers.view(dialog, Number::Compression, conflict_policy),
            help("Leave the path empty to use the current remote directory. Compression 0 stores files as-is and 9 applies the strongest compression."),
            rule::horizontal(1),
            dialog.sftp_xml.view(allowed, cap).map(move |action| Desktop::Settings(Message::SftpXml(id, action))),
        ].spacing(10).into()
    }
}

fn labeled<'a>(setting: Setting, field: impl Into<Element<'a, Desktop>>) -> Element<'a, Desktop> {
    row![text(setting.label()).width(220), field.into()]
        .align_y(Alignment::Center)
        .spacing(10)
        .into()
}

fn heading(title: &'static str) -> Element<'static, Desktop> {
    text(title)
        .size(16)
        .font(Font {
            weight: iced::font::Weight::Bold,
            ..Font::DEFAULT
        })
        .into()
}

fn help(value: impl Into<String>) -> Element<'static, Desktop> {
    text(value.into())
        .size(13)
        .width(420)
        .style(text::secondary)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_archive_preferences_merge_independent_fields_and_validate_on_save() {
        let initial = Draft {
            preferences: WindowSettings::default(),
            restore: None,
            backup: BackupPreferences::default(),
        };
        let mut edited = initial.clone();
        edited.preferences.sftp_archive.level = 3;
        let mut current = initial.preferences.clone();
        current.sftp_archive.remote_directory = "/var/tmp/archives".into();
        let merged = edited.merge_preferences(&initial, current, &ClipboardPolicy::default());
        assert_eq!(merged.sftp_archive.level, 3);
        assert_eq!(merged.sftp_archive.remote_directory, "/var/tmp/archives");
        assert!(merged.sftp_archive.valid());
        assert_eq!(
            search_matches("archive compression").collect::<Vec<_>>(),
            [Setting::SftpArchiveCompression]
        );
    }

    #[test]
    fn sftp_settings_merge_changed_fields_only_and_obey_parallel_policy_caps() {
        let initial = Draft {
            preferences: WindowSettings::default(),
            restore: None,
            backup: BackupPreferences::default(),
        };
        let mut edited = initial.clone();
        edited.preferences.sftp_idle_minutes = Some(7);
        let mut current = initial.preferences.clone();
        current.sftp_parallel_transfers = 5;
        let policy = ClipboardPolicy::default();
        let merged = edited.merge_preferences(&initial, current.clone(), &policy);
        assert_eq!(merged.sftp_parallel_transfers, 5);
        assert_eq!(merged.sftp_idle_minutes, Some(7));
        edited.preferences.sftp_parallel_transfers = 8;
        let mut managed = policy;
        managed.sftp_max_parallel = Some(1);
        assert_eq!(
            edited
                .merge_preferences(&initial, current.clone(), &managed)
                .sftp_parallel_transfers,
            5
        );
        managed.sftp_max_parallel = Some(2);
        assert_eq!(
            edited
                .merge_preferences(&initial, current.clone(), &managed)
                .sftp_parallel_transfers,
            2
        );
        managed.file_transfer_allowed = false;
        assert_eq!(
            edited
                .merge_preferences(&initial, current, &managed)
                .sftp_parallel_transfers,
            5
        );
        assert_eq!(
            search_matches("sftp inactivity").collect::<Vec<_>>(),
            [Setting::SftpIdle]
        );
        assert_eq!(
            search_matches("parallel transfers").collect::<Vec<_>>(),
            [Setting::SftpParallel]
        );
    }

    #[test]
    fn sftp_conflict_default_preserves_current_preferences_under_policy() {
        let initial = Draft {
            preferences: WindowSettings::default(),
            restore: None,
            backup: BackupPreferences::default(),
        };
        let mut edited = initial.clone();
        edited.preferences.sftp_conflict_default = ConflictDefault::Overwrite;
        let mut current = initial.preferences.clone();
        current.sftp_conflict_default = ConflictDefault::Skip;
        let mut policy = ClipboardPolicy::default();
        assert_eq!(
            initial
                .merge_preferences(&initial, current.clone(), &policy)
                .sftp_conflict_default,
            ConflictDefault::Skip
        );
        assert_eq!(
            edited
                .merge_preferences(&initial, current.clone(), &policy)
                .sftp_conflict_default,
            ConflictDefault::Overwrite
        );

        policy.sftp_conflict_default = Some(ConflictDefault::Ask);
        assert_eq!(
            edited
                .merge_preferences(&initial, current.clone(), &policy)
                .sftp_conflict_default,
            ConflictDefault::Skip
        );
        policy.sftp_conflict_default = None;
        policy.file_transfer_allowed = false;
        assert_eq!(
            edited
                .merge_preferences(&initial, current, &policy)
                .sftp_conflict_default,
            ConflictDefault::Skip
        );
        assert_eq!(
            search_matches("sftp conflict").collect::<Vec<_>>(),
            [Setting::SftpConflictDefault]
        );
    }

    #[test]
    fn sftp_number_search_opens_the_exact_field_and_preserves_valid_drafts() {
        let mut workspace = Workspace::default();
        workspace.window_settings.sftp_idle_minutes = Some(17);
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Font));
        let dialog = workspace.settings.0.as_mut().unwrap();
        let id = dialog.id;
        for (query, expected) in [
            ("sftp timeout minutes", Setting::SftpIdleMinutes),
            ("keep partial cancellation", Setting::SftpKeepPartial),
            ("default zip path", Setting::SftpArchive),
            ("archive compression", Setting::SftpArchiveCompression),
        ] {
            dialog.search = query.into();
            assert_eq!(
                dialog.search_results(true).next(),
                Some(SearchTarget::Setting(expected))
            );
        }
        let _ = dialog.reveal_result(Setting::SftpIdleMinutes);
        assert_eq!(dialog.section, Section::Sftp);
        assert_eq!(dialog.revealed, Some(Setting::SftpIdleMinutes));

        let _ = workspace.update_settings(Message::SftpNumber(id, Number::Idle, "37".into()));
        let _ = workspace.update_settings(Message::SftpIdleEnabled(id, false));
        let _ = workspace.update_settings(Message::SftpIdleEnabled(id, true));
        let _ =
            workspace.update_settings(Message::SftpNumber(id, Number::Compression, "10".into()));
        let _ = workspace.apply_settings_to(
            id,
            |_| panic!("invalid numbers must keep the Settings draft open"),
            &mut None,
            || panic!("highlighting was not edited"),
        );
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.preferences.sftp_idle_minutes, Some(37));
        assert_eq!(dialog.revealed, Some(Setting::SftpArchiveCompression));

        workspace.clipboard.policy.sftp_max_parallel = Some(2);
        let parallel = dialog.draft.preferences.sftp_parallel_transfers;
        let _ = workspace.update_settings(Message::SftpNumber(id, Number::Parallel, "3".into()));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.preferences.sftp_parallel_transfers, parallel);
        assert_eq!(
            dialog.sftp_numbers.invalid[Number::Parallel as usize].as_deref(),
            Some("3")
        );
        workspace.clipboard.policy.sftp_max_parallel = Some(1);
        let _ = workspace.update_settings(Message::SftpNumber(id, Number::Parallel, "1".into()));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.preferences.sftp_parallel_transfers, parallel);
        let _ = workspace.update_settings(Message::SftpNumber(id, Number::Compression, "9".into()));
        let dialog = workspace.settings.0.as_ref().unwrap();
        assert_eq!(dialog.draft.preferences.sftp_archive.level, 9);
        let policy = SftpConflictPolicy::from_policy(&workspace.clipboard.policy);
        assert_eq!(
            dialog
                .sftp_numbers
                .first_problem(&dialog.draft.preferences, policy),
            None
        );
    }

    #[test]
    fn sftp_settings_rejects_queued_edits_and_saves_after_policy_locks_the_default() {
        let mut workspace = Workspace::default();
        workspace.window_settings.sftp_conflict_default = ConflictDefault::Skip;
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Sftp));
        let id = workspace.settings.0.as_ref().unwrap().id;
        let _ = workspace.update_settings(Message::SftpConflictDefault(id, ConflictDefault::Ask));
        workspace.clipboard.policy.sftp_conflict_default = Some(ConflictDefault::Overwrite);
        let _ =
            workspace.update_settings(Message::SftpConflictDefault(id, ConflictDefault::Overwrite));
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .sftp_conflict_default,
            ConflictDefault::Ask
        );
        let _ = workspace.apply_settings_to(
            id,
            |_| panic!("managed conflict defaults must not be saved"),
            &mut None,
            || panic!("highlighting was not edited"),
        );
        assert_eq!(
            workspace.window_settings.sftp_conflict_default,
            ConflictDefault::Skip
        );
    }
}
