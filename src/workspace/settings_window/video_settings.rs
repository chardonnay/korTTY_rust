use iced::Element;
use iced::widget::{checkbox, column, text, tooltip};

use crate::recording_window::preferences::Preferences;

use super::{Desktop, Dialog, Message, Setting, highlighted};

#[derive(Clone, Copy, Debug)]
pub enum Edit {
    EnableAfterRestart(bool),
    CaptureColors(bool),
}

pub(super) struct Editor {
    enabled_after_restart: bool,
    include_color: bool,
    enabled_edit: Option<bool>,
    color_edit: Option<bool>,
}

impl Editor {
    pub(super) fn new(preferences: &Preferences) -> Self {
        Self {
            enabled_after_restart: preferences.enabled_after_restart,
            include_color: preferences.include_color,
            enabled_edit: None,
            color_edit: None,
        }
    }

    pub(super) fn update(&mut self, edit: Edit, allowed: bool) {
        match edit {
            Edit::EnableAfterRestart(value) if allowed => self.enabled_edit = Some(value),
            Edit::CaptureColors(value) => self.color_edit = Some(value),
            _ => {}
        }
    }

    pub(super) fn merge(&self, current: &Preferences, allowed: bool) -> Option<Preferences> {
        let mut next = current.clone();
        if allowed
            && let Some(value) = self
                .enabled_edit
                .filter(|&value| value != self.enabled_after_restart)
        {
            next.enabled_after_restart = value;
        }
        if let Some(value) = self.color_edit.filter(|&value| value != self.include_color) {
            next.include_color = value;
        }
        (next != *current).then_some(next)
    }
}

pub(super) fn view(dialog: &Dialog, allowed: bool) -> Element<'_, Desktop> {
    let id = dialog.id;
    let editor = &dialog.video;
    column![
        text("Video").size(18),
        highlighted(
            dialog,
            Setting::RecordingAfterRestart,
            tooltip(
                checkbox(allowed && editor.enabled_edit.unwrap_or(editor.enabled_after_restart))
                    .label(Setting::RecordingAfterRestart.label())
                    .on_toggle_maybe(allowed.then_some(move |value| Desktop::Settings(Message::Video(id, Edit::EnableAfterRestart(value))))),
                text("Makes recording available on startup. Start a recording from the terminal when you need it."),
                tooltip::Position::Bottom,
            ),
        ),
        (!allowed).then(|| text("Terminal recording is unavailable under the current policy.").size(13)),
        highlighted(
            dialog,
            Setting::RecordingColors,
            checkbox(editor.color_edit.unwrap_or(editor.include_color))
                .label(Setting::RecordingColors.label())
                .on_toggle(move |value| Desktop::Settings(Message::Video(id, Edit::CaptureColors(value)))),
        ),
        text("Color capture applies to new recordings.").size(13),
    ].spacing(10).into()
}

#[cfg(test)]
mod tests {
    use super::super::Section;
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn video_settings_save_retains_the_draft_on_failure_and_commits_both_preferences() {
        let scratch = crate::snippets::tests::Profile::new();
        let path = scratch.0.join("video-settings.json");
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Video));
            workspace.settings.0.as_ref().unwrap().id
        };
        let id = open(&mut workspace);
        let _ = workspace.update_settings(Message::Video(id, Edit::EnableAfterRestart(true)));
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(!workspace.video.preferences().enabled_after_restart);

        let id = open(&mut workspace);
        for edit in [Edit::EnableAfterRestart(true), Edit::CaptureColors(true)] {
            let _ = workspace.update_settings(Message::Video(id, edit));
        }
        let next = workspace
            .settings
            .0
            .as_ref()
            .unwrap()
            .video
            .merge(workspace.video.preferences(), true)
            .unwrap();
        // Test doubles for async Iced completion. The successful publication uses a real private file.
        let _ = workspace.save_settings_to(id, |_| panic!("video has its own preferences file"));
        assert!(workspace.video.save_pending());
        let _ = workspace.update_settings(Message::Cancel(id));
        assert!(workspace.settings.title(id).is_some());
        let _ =
            workspace.update_settings(Message::VideoSaved(id, Err("fixture write error".into())));
        assert!(!workspace.video.save_pending());
        assert!(!workspace.video.preferences().enabled_after_restart);
        assert!(!workspace.video.preferences().include_color);
        assert!(workspace.settings.title(id).is_some());

        let _ = workspace.save_settings_to(id, |_| panic!("video has its own preferences file"));
        next.save_to(&path).unwrap();
        let _ = workspace.update_settings(Message::VideoSaved(id, Ok(next)));
        assert!(workspace.settings.title(id).is_none());
        let loaded = Preferences::load_from(&path).unwrap();
        assert_eq!(*workspace.video.preferences(), loaded);
        assert!(loaded.enabled_after_restart && loaded.include_color);
    }

    #[test]
    fn managed_recording_deny_discards_the_queued_enable_and_preserves_other_edits() {
        let current = Preferences::default();
        let mut editor = Editor::new(&current);
        editor.update(Edit::EnableAfterRestart(true), true);
        editor.update(Edit::CaptureColors(true), true);
        let merged = editor.merge(&current, false).unwrap();
        assert!(!merged.enabled_after_restart);
        assert!(merged.include_color);
    }
}
