use super::*;
use iced::widget::{column, row};

pub(super) mod java;

impl Workspace {
    pub(super) fn sftp_settings_page<'a>(&self, dialog: &'a Dialog) -> Element<'a, Desktop> {
        let id = dialog.id;
        let preferences = &dialog.draft.preferences;
        let policy = &self.clipboard.policy;
        let conflict_policy = dialog.installed_sftp_conflict_policy.restrict(policy);
        let cap = conflict_policy.parallel_cap;
        let allowed = conflict_policy.transfer_allowed;
        let parallel: Element<'a, Desktop> = if !allowed {
            text("File transfers are disabled by policy.").into()
        } else if cap == 1 {
            text("1 · Managed by your organization").into()
        } else {
            pick_list(
                (1..=cap).collect::<Vec<_>>(),
                Some(preferences.sftp_parallel_transfers.min(cap)),
                move |value| Desktop::Settings(Message::SftpParallel(id, value)),
            )
            .into()
        };
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
        let idle = preferences.sftp_idle_minutes;
        let archive = &preferences.sftp_archive;
        column![
            highlighted(dialog, Setting::SftpParallel,
                column![
                    text(Setting::SftpParallel.label()),
                    parallel,
                    text(format!("Up to {cap} files can transfer at the same time. Attached SSH sessions use at most two lanes. Transfers to the same destination stay ordered.")).size(12),
                    (cap < crate::sftp_window::parallel::MAX_PARALLEL && cap > 1).then(|| text(format!("Organization limit · {cap} parallel transfers")).size(12)),
                ].spacing(8)),
            rule::horizontal(1),
            highlighted(dialog, Setting::SftpResume,
                column![
                    checkbox(preferences.sftp_resume_partial_transfers).label(Setting::SftpResume.label())
                        .on_toggle_maybe(allowed.then_some(move |enabled| Desktop::Settings(Message::SftpResume(id, enabled)))),
                    checkbox(preferences.sftp_keep_partial_on_cancel).label("Keep partial files when cancelling a transfer")
                        .on_toggle_maybe((allowed && preferences.sftp_resume_partial_transfers).then_some(move |enabled| Desktop::Settings(Message::SftpKeepPartial(id, enabled)))),
                    text("Resume verified partial uploads and downloads after an interruption. Turning off resume releases the retained offers.").size(12),
                    dialog.sftp_xml.view(allowed, cap).map(move |action| Desktop::Settings(Message::SftpXml(id, action))),
                ].spacing(8)),
            rule::horizontal(1),
            highlighted(dialog, Setting::SftpConflictDefault,
                column![
                    text(Setting::SftpConflictDefault.label()),
                    conflict_default,
                    conflict_lock_reason.map(|reason| text(reason).size(12)),
                ].spacing(8)),
            rule::horizontal(1),
            highlighted(dialog, Setting::SftpArchive,
                column![
                    text(Setting::SftpArchive.label()),
                    text_input("Absolute remote directory", &archive.remote_directory)
                        .on_input(move |path| Desktop::Settings(Message::SftpArchiveDirectory(id, path))),
                    text("Leave blank to use the current remote directory.").size(12),
                    row![
                        text("Compression level"),
                        pick_list((0_u8..=9).collect::<Vec<_>>(), Some(archive.level), move |level| Desktop::Settings(Message::SftpArchiveLevel(id, level))),
                    ].spacing(8).align_y(iced::Alignment::Center),
                    (!archive.valid()).then(|| text("Enter an absolute remote directory with ordinary path components.").size(12)),
                ].spacing(8)),
            rule::horizontal(1),
            highlighted(dialog, Setting::SftpIdle,
                column![
                    checkbox(idle.is_some()).label(Setting::SftpIdle.label())
                        .on_toggle(move |enabled| Desktop::Settings(Message::SftpIdleEnabled(id, enabled))),
                    idle.map(|minutes| row![
                        slider(1..=profile::RemoteProjectTool::MAX_IDLE_MINUTES, minutes, move |minutes| Desktop::Settings(Message::SftpIdleMinutes(id, minutes)))
                            .step(1_u16).width(280),
                        text(format!("{minutes} minutes")),
                    ].align_y(Alignment::Center).spacing(10)),
                    text("Applies to detached SFTP windows and hosted tabs. A timeout saved in a project takes precedence. Running or queued transfers, review dialogs and open remote editors keep the connection open.").size(12),
                ].spacing(10)),
        ].spacing(18).into()
    }
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
            [Setting::SftpArchive]
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
