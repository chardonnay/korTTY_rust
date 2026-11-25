use super::super::dialog_design::keyboard_table::{self, TableKey};
use super::*;
use iced::widget::{column, row};

fn heading(label: &'static str) -> Element<'static, Desktop> {
    text(label)
        .size(14)
        .font(dialog_design::ui_font(iced::font::Weight::Bold))
        .into()
}

fn help(label: &'static str) -> Element<'static, Desktop> {
    container(text(label).size(12).style(text::secondary))
        .width(Length::Fill)
        .into()
}

pub(super) fn control_name(setting: Setting) -> &'static str {
    match setting {
        Setting::ToolbarIconSize => "window-toolbar-size",
        Setting::Transparency => "window-transparency",
        Setting::RememberGeometry => "window-remember-geometry",
        Setting::RememberDashboard => "window-remember-dashboard",
        Setting::ToolWindowTabs => "window-tool-tabs",
        Setting::ConnectionColorBorder => "window-connection-border",
        Setting::SplitPaneFocusRing => "window-focus-ring",
        Setting::ShellTitles => "window-shell-titles",
        Setting::RecentTabs => "window-recent-tabs",
        Setting::Restore => "window-restore-mode",
        Setting::RestoreOutput => "window-restore-output",
        Setting::RestoreOutputLines => "window-restore-output-lines",
        Setting::ImportedSessionRestore => "window-imported-restore",
        Setting::FixedGeometry => "window-fixed-geometry",
        Setting::FixedWidth => "window-fixed-width",
        Setting::FixedHeight => "window-fixed-height",
        Setting::FixedX => "window-fixed-x",
        Setting::FixedY => "window-fixed-y",
        Setting::FixedMaximized => "window-fixed-maximized",
        _ => unreachable!("only Window controls use these identifiers"),
    }
}

fn control_id(dialog: &Dialog, setting: Setting, enabled: bool) -> widget::Id {
    dialog
        .input_target(setting, enabled)
        .cloned()
        .unwrap_or_else(|| widget::Id::new(control_name(setting)))
}

fn toggle(
    dialog: &Dialog,
    setting: Setting,
    checked: bool,
    message: Option<Desktop>,
) -> Element<'static, Desktop> {
    let id = control_id(dialog, setting, message.is_some());
    highlighted(
        dialog,
        setting,
        keyboard_table::action_with_id(
            checkbox(checked)
                .label(setting.label())
                .text_size(13)
                .size(18)
                .spacing(8)
                .on_toggle_maybe(message.clone().map(|message| move |_| message.clone()))
                .into(),
            message,
            false,
            Some(id),
        ),
    )
}

pub(super) fn transparency_control(
    dialog: &Dialog,
    window: &super::super::Window,
) -> Element<'static, Desktop> {
    let percent = dialog.draft.preferences.terminal_transparency;
    let id = dialog.id;
    let slider = keyboard_table::view_with_id(
        slider(0..=100, percent, move |percent| {
            Desktop::Settings(Message::Transparency(id, percent))
        })
        .width(132)
        .into(),
        control_id(dialog, Setting::Transparency, true),
        move |key, modifiers, _| {
            if !modifiers.is_empty() {
                return TableKey::Ignored;
            }
            let next = match key {
                Key::Named(Named::ArrowLeft | Named::ArrowDown) => percent.saturating_sub(1),
                Key::Named(Named::ArrowRight | Named::ArrowUp) => {
                    percent.saturating_add(1).min(100)
                }
                Key::Named(Named::Home) => 0,
                Key::Named(Named::End) => 100,
                _ => return TableKey::Ignored,
            };
            TableKey::Handled(Some(Desktop::Settings(Message::Transparency(id, next))))
        },
    );
    row![
        text(Setting::Transparency.label()).size(13),
        slider,
        text(format!("{percent}%")).size(13),
        super::super::transparency::transparency_hint(window, percent)
            .map(|hint| text(hint).size(12)),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .wrap()
    .into()
}

impl Workspace {
    pub(super) fn window_settings_page<'a>(
        &self,
        dialog: &'a Dialog,
        transparency: Option<Element<'a, Desktop>>,
    ) -> Element<'a, Desktop> {
        let id = dialog.id;
        let icon_size = dialog
            .toolbar_icon_size
            .unwrap_or_else(|| self.chrome.toolbar_icon_size());
        let preferences = &dialog.draft.preferences;
        let startup = preferences.startup;
        let mut page = column![
            heading("Window Settings"),
            highlighted(
                dialog,
                Setting::ToolbarIconSize,
                row![
                    text("Toolbar icon size (px):").size(13).width(140),
                    toolbar_size_scale(dialog, icon_size),
                    text(format!("{icon_size} px")).size(13).width(48),
                    self.toolbar_icon_preview(icon_size),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap()
            ),
            toggle(
                dialog,
                Setting::RememberGeometry,
                startup.remember_geometry,
                Some(Desktop::Settings(Message::Startup(
                    id,
                    startup_settings::Edit::RememberGeometry(!startup.remember_geometry)
                )))
            ),
            help(
                "Restore the last size and position of main windows and native dialogs, and the maximized state of main windows."
            ),
            toggle(
                dialog,
                Setting::RememberDashboard,
                startup.remember_dashboard,
                Some(Desktop::Settings(Message::Startup(
                    id,
                    startup_settings::Edit::RememberDashboard(!startup.remember_dashboard)
                )))
            ),
            help("Open the dashboard if it was visible when the last window closed."),
            toggle(
                dialog,
                Setting::ToolWindowTabs,
                preferences.open_tool_windows_as_tabs,
                Some(Desktop::Settings(Message::ToolWindowTabs(
                    id,
                    !preferences.open_tool_windows_as_tabs
                )))
            ),
            help(
                "Takes effect the next time you open a supported tool. The tab opens in the window whose menu you used."
            ),
            rule::horizontal(1),
            heading("Tabs"),
            tooltip(
                toggle(
                    dialog,
                    Setting::ConnectionColorBorder,
                    preferences.connection_color_border_enabled,
                    Some(Desktop::Settings(Message::ConnectionColorBorder(
                        id,
                        !preferences.connection_color_border_enabled
                    )))
                ),
                text("The colored dot on the tab stays visible either way."),
                tooltip::Position::Bottom,
            ),
            help(
                "For colored connections, the frame takes 3 px per side. Switching it resizes open terminals."
            ),
            tooltip(
                toggle(
                    dialog,
                    Setting::SplitPaneFocusRing,
                    preferences.split_pane_focus_ring_enabled,
                    Some(Desktop::Settings(Message::SplitPaneFocusRing(
                        id,
                        !preferences.split_pane_focus_ring_enabled
                    )))
                ),
                text(
                    "Shows the pane with keyboard focus and dims the frame when focus moves outside the pane."
                ),
                tooltip::Position::Bottom,
            ),
            toggle(
                dialog,
                Setting::ShellTitles,
                preferences.tab_title_from_shell_enabled,
                Some(Desktop::Settings(Message::ShellTitles(
                    id,
                    !preferences.tab_title_from_shell_enabled
                )))
            ),
            help("Hover over a tab to see the connection it belongs to."),
            toggle(
                dialog,
                Setting::RecentTabs,
                preferences.tab_switch_most_recent_first,
                Some(Desktop::Settings(Message::RecentTabs(
                    id,
                    !preferences.tab_switch_most_recent_first
                )))
            ),
            help("Uses Ctrl on macOS too. The order is kept while this window is open."),
            rule::horizontal(1),
            heading("Session Restore"),
        ];
        let policy = &self.clipboard.policy;
        page =
            match (dialog.draft.restore, policy.session_restore_mode) {
                (Some(_), Some(mode)) => page.push(highlighted(
                    dialog,
                    Setting::Restore,
                    column![
                        row![
                            text("At startup:").size(13).width(140),
                            text(RestoreChoice(mode).to_string()).size(13)
                        ]
                        .spacing(8),
                        help("Managed by your organization."),
                    ]
                    .spacing(4),
                )),
                (Some(mode), None) => {
                    page.push(highlighted(
                        dialog,
                        Setting::Restore,
                        row![
                            text("At startup:").size(13).width(140),
                            pick_list(
                                RestoreMode::ALL.map(RestoreChoice),
                                Some(RestoreChoice(mode)),
                                move |choice| Desktop::Settings(Message::Restore(id, choice.0))
                            )
                            .text_size(13)
                            .keyboard_navigation(control_id(dialog, Setting::Restore, true)),
                        ]
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .wrap(),
                    ))
                }
                (None, _) => page.push(help("Session recovery is unavailable.")),
            };
        if dialog.draft.restore.is_some() {
            page = page.push(help(
                "File → Restore Previous Session remains available in every startup mode.",
            ));
        }
        let output = preferences.restore_output;
        let managed = policy.session_restore_output;
        let enabled = managed.unwrap_or(output.enabled);
        let editable = policy.problem.is_none() && managed.is_none();
        let number_enabled = editable && enabled;
        let formatted = output.lines.to_string();
        let value = dialog
            .restore_output_lines_input
            .as_deref()
            .filter(|_| number_enabled)
            .unwrap_or(&formatted);
        let edit = number_enabled
            .then_some(move |value| Desktop::Settings(Message::RestoreOutputLinesInput(id, value)));
        let input = dialog.reveal_input(
            Setting::RestoreOutputLines,
            number_enabled,
            text_input("100–5000", value).on_input_maybe(edit),
        );
        let number = dialog_design::number_input(input, value, 100..=5000, 1, edit, 80.0);
        page = page
            .push(toggle(
                dialog,
                Setting::RestoreOutput,
                enabled,
                editable.then(|| Desktop::Settings(Message::RestoreOutput(id, !enabled))),
            ))
            .push(highlighted(
                dialog,
                Setting::RestoreOutputLines,
                row![
                    text("Lines of output per pane:").size(13).width(170),
                    number,
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .wrap(),
            ))
            .push(
                dialog
                    .restore_output_lines_input
                    .as_ref()
                    .filter(|_| number_enabled)
                    .map(|_| {
                        text("Enter a number from 100 to 5000.")
                            .size(12)
                            .style(text::danger)
                    }),
            )
            .push(managed.map(|_| help("Managed by your organization.")))
            .push(help(
                "Saving and restoring output requires an unlocked credential vault.",
            ));
        let imported = preferences
            .java_session_restore
            .available(dialog.draft.restore.is_some(), policy);
        if !imported.is_empty() {
            let action = Desktop::Settings(Message::UseJavaSessionRestore(id));
            page = page.push(
                column![
                    heading("Imported Java session settings"),
                    imported
                        .mode
                        .map(|mode| text(format!("Startup mode · {mode}")).size(13)),
                    imported.output.map(|enabled| text(if enabled {
                        "Restore encrypted terminal output"
                    } else {
                        "Keep terminal output restoration off"
                    })
                    .size(13)),
                    imported
                        .lines
                        .map(|lines| text(format!("Lines per pane · {lines}")).size(13)),
                    highlighted(
                        dialog,
                        Setting::ImportedSessionRestore,
                        keyboard_table::action_with_id(
                            button(Setting::ImportedSessionRestore.label())
                                .on_press(action.clone())
                                .into(),
                            Some(action),
                            true,
                            Some(control_id(dialog, Setting::ImportedSessionRestore, true)),
                        )
                    ),
                ]
                .spacing(4),
            );
        }
        page = page.push(rule::horizontal(1)).push(
            dialog
                .startup
                .view(
                    startup,
                    dialog
                        .revealed
                        .map(|setting| (setting, &dialog.revealed_input)),
                    &dialog.revealed_row,
                )
                .map(move |edit| Desktop::Settings(Message::Startup(id, edit))),
        );
        if let Some(controls) = transparency {
            page = page.push(rule::horizontal(1)).push(highlighted(
                dialog,
                Setting::Transparency,
                controls,
            ));
        }
        page.spacing(6).into()
    }
}

fn toolbar_size_scale(dialog: &Dialog, size: u16) -> Element<'static, Desktop> {
    const MAJOR_TICK: usize = 8;
    let id = dialog.id;
    let min = *TOOLBAR_ICON_SIZE.start();
    let max = *TOOLBAR_ICON_SIZE.end();
    let ticks = TOOLBAR_ICON_SIZE.fold(row![], |ticks, mark| {
        let major = usize::from(mark - min).is_multiple_of(MAJOR_TICK);
        ticks.push(
            container(rule::vertical(1))
                .center_x(Length::Fill)
                .height(if major { 6 } else { 3 }),
        )
    });
    let labels = TOOLBAR_ICON_SIZE
        .step_by(MAJOR_TICK)
        .fold(row![], |mut labels, mark| {
            if mark != min {
                labels = labels.push(space::horizontal());
            }
            labels.push(text(mark.to_string()).size(11).font(iced::Font::MONOSPACE))
        });
    keyboard_table::view_with_id(
        column![
            slider(TOOLBAR_ICON_SIZE, size, move |size| Desktop::Settings(
                Message::ToolbarIconSize(id, size)
            ))
            .step(1_u16),
            container(ticks).padding([0, 5]),
            labels,
        ]
        .width(260)
        .into(),
        control_id(dialog, Setting::ToolbarIconSize, true),
        move |key, modifiers, _| {
            if !modifiers.is_empty() {
                return TableKey::Ignored;
            }
            let value = match key {
                Key::Named(Named::ArrowLeft | Named::ArrowDown) => size.saturating_sub(1).max(min),
                Key::Named(Named::ArrowRight | Named::ArrowUp) => size.saturating_add(1).min(max),
                Key::Named(Named::Home) => min,
                Key::Named(Named::End) => max,
                _ => return TableKey::Ignored,
            };
            TableKey::Handled(Some(Desktop::Settings(Message::ToolbarIconSize(id, value))))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_search::test_input;
    use iced::Font;
    use iced_runtime::{UserInterface, user_interface::Cache};

    fn activate(
        workspace: &mut Workspace,
        id: window::Id,
        setting: Setting,
        key: Named,
    ) -> Vec<Desktop> {
        let _ = workspace.update_settings(Message::Search(id, setting.label().into()));
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .search_results(false, &workspace.clipboard.policy)
                .next(),
            Some(SearchTarget::Setting(setting))
        );
        let reveal = workspace.update_settings(Message::SearchChoose(id));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(setting)
        );
        let mut renderer = iced::Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            workspace.settings_view(id).unwrap(),
            Size::new(640.0, 430.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(&mut interface, &renderer, reveal);
        if setting == Setting::Restore {
            assert!(
                test_input::press(
                    &mut interface,
                    &mut renderer,
                    Key::Named(Named::Space),
                    None
                )
                .is_empty()
            );
            assert!(
                test_input::press(
                    &mut interface,
                    &mut renderer,
                    Key::Named(Named::ArrowDown),
                    None
                )
                .is_empty()
            );
            test_input::press(
                &mut interface,
                &mut renderer,
                Key::Named(Named::Enter),
                None,
            )
        } else {
            test_input::press(&mut interface, &mut renderer, Key::Named(key), None)
        }
    }

    #[test]
    fn window_search_focuses_each_switch_and_selector_from_an_unopened_page() {
        for setting in [
            Setting::RememberGeometry,
            Setting::RememberDashboard,
            Setting::ToolWindowTabs,
            Setting::ConnectionColorBorder,
            Setting::SplitPaneFocusRing,
            Setting::ShellTitles,
            Setting::RecentTabs,
            Setting::RestoreOutput,
            Setting::FixedGeometry,
            Setting::FixedMaximized,
            Setting::Restore,
            Setting::ToolbarIconSize,
        ] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.open_settings(owner, Some(Section::Font));
            let id = workspace.settings.for_owner(owner).unwrap();
            if setting == Setting::Restore {
                workspace.settings.0.as_mut().unwrap().draft.restore = Some(RestoreMode::Ask);
            }
            if setting == Setting::FixedMaximized {
                workspace
                    .settings
                    .0
                    .as_mut()
                    .unwrap()
                    .draft
                    .preferences
                    .startup
                    .use_fixed_geometry = true;
            }
            let key = if matches!(setting, Setting::Restore | Setting::ToolbarIconSize) {
                Named::ArrowDown
            } else {
                Named::Space
            };
            let messages = activate(&mut workspace, id, setting, key);
            assert!(
                match (setting, messages.as_slice()) {
                    (
                        Setting::RememberGeometry,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::RememberGeometry(_),
                            )),
                        ],
                    )
                    | (
                        Setting::RememberDashboard,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::RememberDashboard(_),
                            )),
                        ],
                    )
                    | (
                        Setting::ToolWindowTabs,
                        [Desktop::Settings(Message::ToolWindowTabs(target, _))],
                    )
                    | (
                        Setting::ConnectionColorBorder,
                        [Desktop::Settings(Message::ConnectionColorBorder(target, _))],
                    )
                    | (
                        Setting::SplitPaneFocusRing,
                        [Desktop::Settings(Message::SplitPaneFocusRing(target, _))],
                    )
                    | (
                        Setting::ShellTitles,
                        [Desktop::Settings(Message::ShellTitles(target, _))],
                    )
                    | (Setting::RecentTabs, [Desktop::Settings(Message::RecentTabs(target, _))])
                    | (
                        Setting::RestoreOutput,
                        [Desktop::Settings(Message::RestoreOutput(target, _))],
                    )
                    | (
                        Setting::FixedGeometry,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Fixed(true),
                            )),
                        ],
                    )
                    | (
                        Setting::FixedMaximized,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Maximize(_),
                            )),
                        ],
                    )
                    | (
                        Setting::ToolbarIconSize,
                        [Desktop::Settings(Message::ToolbarIconSize(target, _))],
                    ) => *target == id,
                    (
                        Setting::Restore,
                        [Desktop::Settings(Message::Restore(target, RestoreMode::Auto))],
                    ) => *target == id,
                    _ => false,
                },
                "{setting:?}: {messages:?}"
            );
            assert!(!workspace.window_settings.restore_output.enabled);
        }
    }

    #[test]
    fn window_search_focuses_each_numeric_field_and_respects_disabled_controls() {
        for setting in [
            Setting::FixedWidth,
            Setting::FixedHeight,
            Setting::FixedX,
            Setting::FixedY,
            Setting::RestoreOutputLines,
        ] {
            let mut workspace = Workspace::default();
            let (owner, _) = workspace.open_window(false);
            let _ = workspace.open_settings(owner, Some(Section::Font));
            let id = workspace.settings.for_owner(owner).unwrap();
            let preferences = &mut workspace.settings.0.as_mut().unwrap().draft.preferences;
            preferences.startup.use_fixed_geometry = true;
            preferences.restore_output.enabled = true;
            let messages = activate(&mut workspace, id, setting, Named::ArrowUp);
            assert!(
                match (setting, messages.as_slice()) {
                    (
                        Setting::FixedWidth,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Value(startup_settings::Field::Width, _),
                            )),
                        ],
                    )
                    | (
                        Setting::FixedHeight,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Value(startup_settings::Field::Height, _),
                            )),
                        ],
                    )
                    | (
                        Setting::FixedX,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Value(startup_settings::Field::X, _),
                            )),
                        ],
                    )
                    | (
                        Setting::FixedY,
                        [
                            Desktop::Settings(Message::Startup(
                                target,
                                startup_settings::Edit::Value(startup_settings::Field::Y, _),
                            )),
                        ],
                    )
                    | (
                        Setting::RestoreOutputLines,
                        [Desktop::Settings(Message::RestoreOutputLinesInput(target, _))],
                    ) => *target == id,
                    _ => false,
                },
                "{setting:?}: {messages:?}"
            );
        }
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Font));
        let id = workspace.settings.for_owner(owner).unwrap();
        workspace.clipboard.policy.session_restore_output = Some(false);
        for setting in [Setting::RestoreOutput, Setting::RestoreOutputLines] {
            use iced::advanced::widget::operation::focusable;
            let _ = workspace.update_settings(Message::Search(id, setting.label().into()));
            let reveal = workspace.update_settings(Message::SearchChoose(id));
            let search = workspace.settings.0.as_ref().unwrap().search_input.clone();
            let mut renderer = iced::Renderer::new(Font::DEFAULT, 16.0.into());
            let mut interface = UserInterface::build(
                workspace.settings_view(id).unwrap(),
                Size::new(640.0, 430.0),
                Cache::default(),
                &mut renderer,
            );
            interface.operate(&renderer, &mut focusable::focus(search));
            test_input::apply(&mut interface, &renderer, reveal);
            let messages = test_input::press(
                &mut interface,
                &mut renderer,
                Key::Character("x".into()),
                Some("x"),
            );
            assert!(
                matches!(messages.as_slice(), [Desktop::Settings(Message::Search(target, query))] if *target == id && query == "x")
            );
        }
        let _ = workspace.update_settings(Message::RestoreOutputLinesInput(id, "1234".into()));
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .restore_output
                .lines,
            1000
        );
    }

    #[test]
    fn invalid_restore_output_limit_blocks_save_and_cancel_preserves_preferences() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.open_settings(owner, Some(Section::Window));
        let id = workspace.settings.for_owner(owner).unwrap();
        let initial = workspace.window_settings.restore_output;
        let _ = workspace.update_settings(Message::RestoreOutput(id, true));
        let _ = workspace.update_settings(Message::RestoreOutputLinesInput(id, "5001".into()));
        let _ = workspace.save_settings_to(id, |_| panic!("invalid input must not be saved"));
        assert_eq!(
            workspace.settings.0.as_ref().unwrap().revealed,
            Some(Setting::RestoreOutputLines)
        );
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .restore_output
                .lines,
            initial.lines
        );
        let _ = workspace.update_settings(Message::RestoreOutputLinesInput(id, "1234".into()));
        assert!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .restore_output_lines_input
                .is_none()
        );
        assert_eq!(
            workspace
                .settings
                .0
                .as_ref()
                .unwrap()
                .draft
                .preferences
                .restore_output
                .lines,
            1234
        );
        let _ = workspace.update_settings(Message::Cancel(id));
        assert_eq!(workspace.window_settings.restore_output, initial);
    }
}
