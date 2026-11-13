use std::fmt;

use iced::widget::{button, checkbox, column, pick_list, row, text};
use iced::{Alignment, Element, Length, Task, window};

use super::{Desktop, Dialog, Message, Section, Setting, Workspace, highlighted};
use crate::workspace::highlight;

#[derive(Clone, Debug)]
pub enum Edit {
    Enabled(bool),
    AlternateScreen(bool),
    DefaultSet(Option<String>),
    Triggers(bool),
    OpenRules,
}

#[derive(Clone, PartialEq, Eq)]
struct Choice {
    id: Option<String>,
    label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.label)
    }
}

fn choices(sets: &[(String, String)], selected: Option<&str>) -> (Vec<Choice>, Choice) {
    let none = Choice {
        id: None,
        label: "None".into(),
    };
    let mut choices = vec![none.clone()];
    choices.extend(sets.iter().map(|(id, label)| Choice {
        id: Some(id.clone()),
        label: label.clone(),
    }));
    let selected = selected.filter(|id| *id != "none").map_or(none, |id| {
        choices
            .iter()
            .find(|choice| choice.id.as_deref() == Some(id))
            .cloned()
            .unwrap_or_else(|| {
                let missing = Choice {
                    id: Some(id.to_owned()),
                    label: format!("Missing set ({id})"),
                };
                choices.push(missing.clone());
                missing
            })
    });
    (choices, selected)
}

impl Workspace {
    pub(in crate::workspace) fn remove_deleted_highlight_defaults(&mut self, deleted: &[String]) {
        if let Some(dialog) = &mut self.settings.0 {
            for prefs in [&mut dialog.initial_highlighting, &mut dialog.highlighting] {
                if prefs
                    .default_set
                    .as_ref()
                    .is_some_and(|id| deleted.contains(id))
                {
                    prefs.default_set = None;
                }
            }
        }
    }

    pub(super) fn update_highlight_settings(
        &mut self,
        id: window::Id,
        edit: Edit,
    ) -> Task<Desktop> {
        if let Some(child) = self.highlight_rules_for_settings(id) {
            return window::gain_focus(child);
        }
        if matches!(edit, Edit::OpenRules) {
            if self
                .settings
                .0
                .as_ref()
                .is_some_and(|dialog| dialog.id == id && dialog.section == Section::Terminal)
                && self.clipboard.policy.problem.is_none()
                && self.openpgp_for_source(id).is_none()
            {
                return self.update_highlight(highlight::Message::OpenFromSettings(id));
            }
            return Task::none();
        }
        let Some(dialog) = self
            .settings
            .dialog_mut(id)
            .filter(|dialog| dialog.section == Section::Terminal)
        else {
            return Task::none();
        };
        let draft = &mut dialog.highlighting;
        match edit {
            Edit::Enabled(enabled) => draft.enabled = enabled,
            Edit::AlternateScreen(enabled) if draft.enabled => draft.alternate_screen = enabled,
            Edit::DefaultSet(value)
                if draft.enabled
                    && value.as_deref().is_none_or(|id| self.highlight.has_set(id)) =>
            {
                draft.default_set = value
            }
            Edit::Triggers(enabled)
                if draft.enabled
                    && self.clipboard.policy.terminal_triggers_allowed
                    && self.clipboard.policy.problem.is_none() =>
            {
                draft.triggers_enabled = enabled
            }
            _ => return Task::none(),
        }
        dialog.problem = None;
        Task::none()
    }

    pub(super) fn highlight_settings_section<'a>(
        &'a self,
        dialog: &'a Dialog,
    ) -> Element<'a, Desktop> {
        let draft = &dialog.highlighting;
        let id = dialog.id;
        let message = move |edit| Desktop::Settings(Message::Highlighting(id, edit));
        let (options, selected) = choices(
            self.highlight.connection_choices().sets,
            draft.default_set.as_deref(),
        );
        let default: Element<'_, Desktop> = if draft.enabled {
            pick_list(options, Some(selected), move |choice: Choice| {
                message(Edit::DefaultSet(choice.id))
            })
            .width(Length::Fill)
            .into()
        } else {
            text(selected.label).width(Length::Fill).into()
        };
        let triggers_allowed = self.clipboard.policy.terminal_triggers_allowed
            && self.clipboard.policy.problem.is_none();
        highlighted(
            dialog,
            Setting::Highlighting,
            column![
                text(Setting::Highlighting.label()).size(16),
                checkbox(draft.enabled)
                    .label("Enable terminal highlighting")
                    .on_toggle(move |enabled| message(Edit::Enabled(enabled))),
                checkbox(draft.alternate_screen)
                    .label("Highlight full-screen applications")
                    .on_toggle_maybe(
                        draft
                            .enabled
                            .then_some(move |enabled| message(Edit::AlternateScreen(enabled)))
                    ),
                row![
                    text("Default rule set"),
                    default,
                    button("Edit Rules…").on_press_maybe(
                        self.clipboard
                            .policy
                            .problem
                            .is_none()
                            .then_some(message(Edit::OpenRules))
                    ),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                checkbox(triggers_allowed && draft.triggers_enabled)
                    .label("Allow rule notifications and reviewed snippet requests")
                    .on_toggle_maybe(
                        (draft.enabled && triggers_allowed)
                            .then_some(move |enabled| message(Edit::Triggers(enabled)))
                    ),
                (!triggers_allowed).then(|| text("Rule actions are disabled by policy.").size(13)),
            ]
            .spacing(8),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snippets::tests::Profile;
    use crate::terminal_highlight::Configuration;

    #[test]
    fn terminal_highlight_settings_preserve_independent_rule_saves_and_policy() {
        let profile = Profile::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let open = |workspace: &mut Workspace| {
            let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
            workspace.settings.for_owner(owner).unwrap()
        };
        let edit = |workspace: &mut Workspace, id, action| {
            let _ = workspace.update_settings(Message::Highlighting(id, action));
        };
        let open_rules = |workspace: &mut Workspace, id| {
            edit(workspace, id, Edit::OpenRules);
            let child = workspace.highlight_rules_for_settings(id).unwrap();
            assert_eq!(workspace.placed_dialog_owner(child), Some(id));
            child
        };
        let rule = |workspace: &mut Workspace, child, action| {
            let _ = workspace.update_highlight(highlight::Message::Edit(child, action));
        };
        let settings = open(&mut workspace);
        edit(&mut workspace, settings, Edit::AlternateScreen(true));
        let child = open_rules(&mut workspace, settings);
        let _ = workspace.update_settings(Message::Cancel(settings));
        assert!(workspace.settings.title(settings).is_some());
        rule(&mut workspace, child, highlight::Edit::Duplicate);
        workspace.save_highlight_rules(child, || Ok(profile.0.clone()));
        rule(&mut workspace, child, highlight::Edit::Close);
        let _ = workspace.update_settings(Message::Cancel(settings));
        let first = Configuration::read(&profile.0).unwrap();
        assert_eq!(first.sets.len(), 1);
        assert!(!first.alternate_screen);

        let settings = open(&mut workspace);
        edit(&mut workspace, settings, Edit::Enabled(false));
        edit(&mut workspace, settings, Edit::AlternateScreen(true));
        assert!(
            !workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .highlighting
                .alternate_screen
        );
        edit(&mut workspace, settings, Edit::Enabled(true));
        edit(&mut workspace, settings, Edit::AlternateScreen(true));
        edit(&mut workspace, settings, Edit::Triggers(true));
        let first_id = &first.sets[0].id;
        edit(
            &mut workspace,
            settings,
            Edit::DefaultSet(Some(first_id.clone())),
        );
        let child = open_rules(&mut workspace, settings);
        rule(&mut workspace, child, highlight::Edit::Duplicate);
        rule(
            &mut workspace,
            child,
            highlight::Edit::Select(first_id.clone()),
        );
        rule(&mut workspace, child, highlight::Edit::Delete);
        workspace.save_highlight_rules(child, || Ok(profile.0.clone()));
        rule(&mut workspace, child, highlight::Edit::Close);
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .highlighting
                .default_set,
            None
        );
        let revised = Configuration::read(&profile.0).unwrap();
        assert_eq!(revised.sets.len(), 1);
        assert_ne!(revised.sets[0].id, *first_id);

        workspace.clipboard.policy.terminal_triggers_allowed = false;
        let _ =
            workspace.apply_settings_to(settings, |_| Ok(()), &mut None, || Ok(profile.0.clone()));
        let saved = Configuration::read(&profile.0).unwrap();
        assert!(saved.alternate_screen);
        assert!(!saved.triggers_enabled);
        assert_eq!(saved.sets, revised.sets);
        assert_eq!(saved.default_set, None);
        assert_eq!(
            workspace.highlight.preferences(),
            highlight::Preferences::from(&saved)
        );
        assert!(workspace.settings.title(settings).is_none());
    }

    #[test]
    fn terminal_highlight_settings_reject_external_revision_without_publishing() {
        let profile = Profile::new();
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_settings(Message::OpenAt(owner, Section::Terminal));
        let id = workspace.settings.for_owner(owner).unwrap();
        let _ = workspace.update_settings(Message::Highlighting(id, Edit::AlternateScreen(true)));
        let current = Configuration {
            default_set: Some("missing-shared-set".into()),
            ..Configuration::default()
        };
        current.save(&profile.0).unwrap();
        let _ = workspace.apply_settings_to(id, |_| Ok(()), &mut None, || Ok(profile.0.clone()));
        assert_eq!(Configuration::read(&profile.0).unwrap(), current);
        assert!(!workspace.highlight.preferences().alternate_screen);
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .problem
                .as_deref()
                .is_some_and(|problem| problem.contains("changed on disk"))
        );

        let (_, selected) = choices(&[], current.default_set.as_deref());
        assert_eq!(selected.id, current.default_set);
        assert!(selected.label.contains("Missing set"));
    }
}
