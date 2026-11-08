use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use iced::advanced::{layout, widget::Tree};
use iced::{Font, Pixels, Renderer, Size};

use crate::session_journal::{self, viewer::Source};
use crate::terminal_theme::Rgb;
use crate::workspace::{
    journal_access,
    journal_appearance::{PageTheme, SchemeId, Zoom},
    tab_titles::WindowSettings,
};

use super::*;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-journal-appearance-{}",
            crate::private_files::nonce().unwrap()
        ));
        let directory = root.join("journal");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("journal.xml"),
            r#"<session-journal formatVersion="1" id="appearance"><meta><title>Test journal</title></meta>
             <entries><entry><id>entry</id><kind>USER_NOTE</kind><title>Example</title>
             <text>Command summary</text><inputExcerpt>ls</inputExcerpt>
             <outputExcerpt>result</outputExcerpt></entry></entries></session-journal>"#,
        ).unwrap();
        Self(root)
    }

    fn viewer(&self, workspace: &mut Workspace) -> (window::Id, window::Id) {
        let (owner, _) = workspace.open_window(false);
        let document = Arc::new(session_journal::read(&self.0.join("journal")).unwrap());
        let source = Source::catalog(&document.summary).unwrap();
        let _ = workspace.open_journal_viewer(owner, source, "Test journal".into());
        let viewer = *workspace.journals.keys().next().unwrap();
        // Test double for Iced task delivery of a journal read from disk.
        let _ = workspace.update(Message::SessionJournal(
            viewer,
            journal_access::Message::DocumentLoaded(Ok(document)),
        ));
        (owner, viewer)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn edit(workspace: &mut Workspace, target: Target, event: controls::Event) {
    let _ = workspace.update(Message::JournalAppearance(target, event));
}

#[test]
fn journal_appearance_native_and_hosted_preview_persist_profile_values() {
    for hosted in [false, true] {
        let fixture = Fixture::new();
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = hosted;
        let (owner, viewer) = fixture.viewer(&mut workspace);
        let target = Target::Viewer(viewer);
        workspace.set_journal_system_theme(theme::Mode::Light);
        assert_eq!(
            workspace
                .journal_appearance_page(target)
                .content
                .palette
                .colors
                .background,
            Rgb::from_hex(0xf6f8fa),
        );
        edit(&mut workspace, target, controls::Event::CycleTheme);
        assert_eq!(
            workspace.window_settings.journal_appearance.theme,
            PageTheme::Dark
        );
        edit(&mut workspace, target, controls::Event::CycleTheme);
        assert_eq!(
            workspace.window_settings.journal_appearance.theme,
            PageTheme::Light
        );
        edit(&mut workspace, target, controls::Event::Toggle);
        edit(&mut workspace, target, controls::Event::ToggleSchemes);
        assert!(workspace.journal_appearance_page(target).schemes_open);
        edit(
            &mut workspace,
            target,
            controls::Event::Scheme(SchemeId::Paper),
        );
        edit(&mut workspace, target, controls::Event::Scale(250));
        let pending = workspace.journal_appearance_ui.save.as_ref().unwrap();
        assert_eq!(pending.owner, owner);
        let page = workspace.journal_appearance_page(target);
        assert!(page.open && !page.schemes_open);
        assert_eq!(page.content.size(12.0), 30.0);
        assert_eq!(
            page.content.palette.colors.background,
            Rgb::from_hex(0xf7f3ea)
        );
        edit(&mut workspace, target, controls::Event::CycleTheme);
        assert_eq!(
            workspace.window_settings.journal_appearance.theme,
            PageTheme::Light
        );

        let settings = fixture.0.join("window-settings.json");
        workspace.window_settings.save(&settings).unwrap();
        let restored: WindowSettings =
            serde_json::from_slice(&fs::read(settings).unwrap()).unwrap();
        assert_eq!(
            restored.journal_appearance,
            workspace.window_settings.journal_appearance
        );

        let renderer = Renderer::new(Font::DEFAULT, Pixels(13.0));
        for size in [Size::new(360.0, 280.0), Size::new(1000.0, 680.0)] {
            let mut view = workspace.journal_view(viewer).unwrap();
            let mut tree = Tree::new(view.as_widget());
            let bounds = view
                .as_widget_mut()
                .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, size))
                .bounds();
            assert!(bounds.width > 0.0 && bounds.height > 0.0);
        }

        edit(&mut workspace, target, controls::Event::Reset);
        let preferences = &workspace.window_settings.journal_appearance;
        assert_eq!(preferences.scheme, SchemeId::Auto);
        assert_eq!(preferences.scale, 100);
        assert_eq!(preferences.theme, PageTheme::Light);
    }
}

#[test]
fn journal_appearance_ignores_hidden_viewer_and_stale_dock_messages() {
    let fixture = Fixture::new();
    let mut workspace = Workspace::default();
    workspace.window_settings.open_tool_windows_as_tabs = true;
    workspace.window_settings.session_journal.directory = fixture.0.to_string_lossy().into_owned();
    let (owner, viewer) = fixture.viewer(&mut workspace);
    let target = Target::Viewer(viewer);
    edit(&mut workspace, target, controls::Event::Toggle);
    let stale = Target::Dock {
        owner,
        tab: 1,
        run: 1,
    };
    edit(&mut workspace, stale, controls::Event::Dismiss);
    edit(&mut workspace, stale, controls::Event::CycleTheme);
    assert!(workspace.journal_appearance_page(target).open);
    assert_eq!(
        workspace.window_settings.journal_appearance.theme,
        PageTheme::Auto
    );

    let _ = workspace.update(Message::SessionJournal(
        owner,
        journal_access::Message::Open,
    ));
    assert!(!workspace.journal_appearance_page(target).enabled);
    edit(&mut workspace, target, controls::Event::Zoom(Zoom::Larger));
    edit(
        &mut workspace,
        target,
        controls::Event::Scheme(SchemeId::Ocean),
    );
    assert!(workspace.window_settings.journal_appearance.is_default());
}
