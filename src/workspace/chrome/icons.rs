//! The two-layer toolbar glyphs from the MIT-licensed Java reference.

use std::cell::RefCell;

use iced::widget::svg::{self, Handle};

use crate::terminal_theme::Rgb;

#[derive(Clone, Copy)]
pub(crate) enum Icon {
    Tab,
    Shell,
    PowerShell,
    CommandPrompt,
    Connections,
    Sftp,
    Snippets,
    Journal,
    Logfile,
    Ai,
    Jobs,
    Video,
    Font,
    Colors,
    Keyboard,
    Window,
    Add,
    Gear,
    Server,
    Group,
    Separator,
    Editor,
    Security,
    Privacy,
    Language,
    Translation,
    Themes,
    Appearance,
    Backup,
    Export,
    Updates,
    Resources,
    Pin,
    Check,
    Warning,
    Star,
    Flag,
    Search,
    Clock,
}

impl Icon {
    const COUNT: usize = Self::Clock as usize + 1;

    pub(crate) fn widget(self, enabled: bool, size: u16, color: Option<Rgb>) -> svg::Svg<'static> {
        thread_local! {
            static CACHE: RefCell<Vec<Handles>> = const { RefCell::new(Vec::new()) };
        }
        let handle = CACHE.with_borrow_mut(|cache| {
            let index = if let Some(index) = cache.iter().position(|entry| entry.color == color) {
                index
            } else {
                // Toolbar, themed badges and the two solid-badge foregrounds use separate colors.
                if cache.len() == 4 {
                    cache.remove(0);
                }
                cache.push(Handles {
                    color,
                    active: std::array::from_fn(|_| None),
                    dimmed: std::array::from_fn(|_| None),
                });
                cache.len() - 1
            };
            let handles = &mut cache[index];
            let group = if enabled {
                &mut handles.active
            } else {
                &mut handles.dimmed
            };
            group[self as usize]
                .get_or_insert_with(|| {
                    let contour = color.map_or_else(|| "#e4e8ed".into(), |rgb| rgb.to_string());
                    self.handle(enabled, &contour)
                })
                .clone()
        });
        svg::Svg::new(handle)
            .width(u32::from(size))
            .height(u32::from(size))
    }

    fn handle(self, enabled: bool, foreground: &str) -> Handle {
        let (contour, core, color) = self.paths();
        let opacity = if enabled { "1" } else { "0.4" };
        Handle::from_memory(format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><g opacity="{opacity}" fill="none" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path stroke="{foreground}" d="{contour}"/><path stroke="{color}" d="{core}"/></g></svg>"##
        ).into_bytes())
    }

    fn paths(self) -> (&'static str, &'static str, &'static str) {
        const BLUE: &str = "#368af5";
        const TEAL: &str = "#13a8ad";
        const PURPLE: &str = "#a56ae6";
        const AMBER: &str = "#dc9825";
        match self {
            Self::Tab => (
                "M3 20 V6 Q3 3 6 3 H18 Q21 3 21 6 V20",
                "M8 12 H16 M12 8 V16",
                BLUE,
            ),
            Self::Shell => (
                "M5 3 H19 Q21 3 21 5 V19 Q21 21 19 21 H5 Q3 21 3 19 V5 Q3 3 5 3",
                "M7 8 L11 12 L7 16 M13 16 H17",
                BLUE,
            ),
            Self::PowerShell => (
                "M7 4 H22 L17 20 H2 Z",
                "M8 8.5 L12 12 L7 15.5 M11.5 15.5 H15",
                "#5391fe",
            ),
            Self::CommandPrompt => (
                "M3 4 H21 V20 H3 Z M3 8 H21",
                "M10 11.5 Q6.5 11 6.5 14 Q6.5 17 10 16.5 M12.5 12.5 V12.6 M12.5 16 V16.1 M15 11 L18 17",
                TEAL,
            ),
            Self::Connections => (
                "M3 3 H13 V9 H3 Z M3 15 H13 V21 H3 Z M8 9 V15",
                "M6 6 H7 M6 18 H7 M16 5 H21 M16 12 H21 M16 19 H21",
                BLUE,
            ),
            Self::Sftp => (
                "M3 7 V5 Q3 3 5 3 H10 L13 6 H19 Q21 6 21 8 V19 Q21 21 19 21 H5 Q3 21 3 19 Z",
                "M7 11 H17 L14 8 M17 16 H7 L10 19",
                TEAL,
            ),
            Self::Snippets => (
                "M6 2 H20 V18 M3 6 H16 V22 H3 Z",
                "M8 10 L5 14 L8 18 M11 10 L14 14 L11 18",
                TEAL,
            ),
            Self::Journal => (
                "M7 3 H20 V21 H7 Q4 21 4 18 V6 Q4 3 7 3 Z M8 3 V21 M2 7 H5 M2 12 H5 M2 17 H5",
                "M13 3 V11 L16 9 L19 11 V3",
                TEAL,
            ),
            Self::Logfile => (
                "M6 3 H14 L19 8 V21 H6 Z M14 3 V8 H19",
                "M9 12 H16 M9 15 H16 M9 18 H13",
                TEAL,
            ),
            Self::Ai => (
                "M9 4 Q5 2 5 7 Q1 8 4 12 Q1 16 5 18 Q5 23 10 20 M15 4 Q19 2 19 7 Q23 8 20 12 Q23 16 19 18 Q19 23 14 20",
                "M12 3 V21 M8 8 H12 M12 15 H17 M8 17 V13 H12 M16 6 V10 H12",
                PURPLE,
            ),
            Self::Jobs => (
                "M12 21 H5 Q3 21 3 19 V6 Q3 4 5 4 H18 Q20 4 20 6 V10 M3 9 H20 M7 2 V6 M16 2 V6 M7 13 H9 M7 17 H9 M16 12 A5 5 0 1 0 16.01 12",
                "M17 15 V18 H20",
                AMBER,
            ),
            Self::Video => (
                "M3 4 H21 V20 H3 Z M7 4 V20 M17 4 V20 M3 9 H7 M3 15 H7 M17 9 H21 M17 15 H21",
                "M10 9 L14 12 L10 15 Z",
                PURPLE,
            ),
            Self::Font => (
                "M3 17 L8 4 L13 17 M5 12 H11 M16 10 Q22 7 22 12 V17 M22 13 Q15 11 16 16 Q18 19 22 15",
                "M4 21 H20",
                BLUE,
            ),
            Self::Colors => (
                "M12 3 C1 3 0 17 8 20 C13 23 11 17 16 17 C24 18 24 3 12 3 Z M7 8 H7.1 M13 6 H13.1 M18 9 H18.1",
                "M7 14 A1 1 0 1 0 7.01 14",
                BLUE,
            ),
            Self::Keyboard => (
                "M3 5 H21 V19 H3 Z M6 9 H7 M10 9 H11 M14 9 H15 M18 9 H18.1 M6 13 H7 M10 13 H11 M6 16 H14",
                "M15 13 H18 V16 H15 Z",
                BLUE,
            ),
            Self::Window => (
                "M3 4 H21 V20 H3 Z M3 9 H21",
                "M6 7 H7 M10 7 H11 M14 7 H15",
                BLUE,
            ),
            Self::Add => ("M12 3 A9 9 0 1 0 12.01 3", "M7 12 H17 M12 7 V17", BLUE),
            Self::Gear => (
                "M9 3 H15 L16 6 L19 5 L22 10 L19 12 L22 14 L19 19 L16 18 L15 21 H9 L8 18 L5 19 L2 14 L5 12 L2 10 L5 5 L8 6 Z",
                "M12 8 A4 4 0 1 0 12.01 8",
                BLUE,
            ),
            Self::Server => (
                "M5 3 H19 Q21 3 21 5 V9 Q21 11 19 11 H5 Q3 11 3 9 V5 Q3 3 5 3 M5 13 H19 Q21 13 21 15 V19 Q21 21 19 21 H5 Q3 21 3 19 V15 Q3 13 5 13",
                "M16 7 H17 M16 17 H17",
                BLUE,
            ),
            Self::Group => (
                "M8 2 H19 V10 H8 Z M2 12 H12 V21 H2 Z M15 13 H23 V21 H15 Z",
                "M15 6 H16 M8 16 H9 M19 17 H20",
                BLUE,
            ),
            Self::Separator => ("M12 3 V21", "M12 10 V14", BLUE),
            Self::Editor => (
                "M3 3 H17 L21 7 V21 H3 Z M17 3 V7 H21",
                "M8 10 L5 13 L8 16 M16 10 L19 13 L16 16 M13 9 L11 18",
                BLUE,
            ),
            Self::Security => (
                "M12 2 L21 6 V12 Q21 18 12 22 Q3 18 3 12 V6 Z",
                "M9 12 V10 A3 3 0 0 1 15 10 V12 M8 12 H16 V17 H8 Z",
                BLUE,
            ),
            Self::Privacy => (
                "M2 12 Q12 1 22 12 Q12 23 2 12 Z",
                "M12 8 A4 4 0 1 0 12.01 8 M4 21 L21 3",
                "#a56ae6",
            ),
            Self::Language => (
                "M12 2 A10 10 0 1 0 12.01 2 M2 12 H22 M12 2 C5 7 5 17 12 22 C19 17 19 7 12 2",
                "M8 7 H16 M8 17 H16",
                BLUE,
            ),
            Self::Translation => (
                "M2 4 H13 V15 H5 L2 18 Z M15 9 H22 V21 H12 V17",
                "M5 8 H10 M7 6 V8 M6 8 Q6 11 10 12 M10 8 Q10 11 5 12 M15 18 L17 12 L19 18 M16 16 H18",
                TEAL,
            ),
            Self::Themes => (
                "M12 2 A10 10 0 1 0 12.01 2 M12 2 V22",
                "M16 6 V18 M19 9 V15",
                "#a56ae6",
            ),
            Self::Appearance => (
                "M3 4 H21 V20 H3 Z M3 9 H21 M9 9 V20",
                "M13 13 H18 M13 16 H17",
                BLUE,
            ),
            Self::Backup => (
                "M3 8 H21 V21 H3 Z M2 3 H22 V8 H2 Z",
                "M8 13 H16 M12 10 V18 M9 15 L12 18 L15 15",
                TEAL,
            ),
            Self::Export => ("M13 3 H5 V21 H19 V13", "M10 14 L21 3 M15 3 H21 V9", TEAL),
            Self::Updates => (
                "M20 9 A8 8 0 0 0 6 5 L3 8 M3 15 A8 8 0 0 0 17 19 L21 16",
                "M3 3 V8 H8 M16 16 H21 V21",
                BLUE,
            ),
            Self::Resources => (
                "M6 6 H18 V18 H6 Z M3 8 H6 M3 12 H6 M3 16 H6 M18 8 H21 M18 12 H21 M18 16 H21 M8 3 V6 M12 3 V6 M16 3 V6 M8 18 V21 M12 18 V21 M16 18 V21",
                "M10 10 H14 V14 H10 Z",
                TEAL,
            ),
            Self::Pin => ("M8 3 H16 M9 3 V10 L6 15 H18 L15 10 V3", "M12 15 V22", BLUE),
            Self::Check => ("M12 3 A9 9 0 1 0 12.01 3", "M7 12 L10 15 L17 8", TEAL),
            Self::Warning => ("M12 3 L22 21 H2 Z", "M12 9 V14 M12 18 H12.1", "#dc9825"),
            Self::Star => (
                "M12 2 L15 8 L22 9 L17 14 L18 21 L12 18 L6 21 L7 14 L2 9 L9 8 Z",
                "M12 9 V14",
                BLUE,
            ),
            Self::Flag => ("M5 22 V3 H13 L15 5 H21 V15 H13 L11 13 H5", "M8 7 H11", BLUE),
            Self::Search => ("M10 3 A7 7 0 1 0 10.01 3 M15 15 L22 22", "M7 10 H13", BLUE),
            Self::Clock => ("M12 2 A10 10 0 1 0 12.01 2", "M12 6 V12 L16 15", BLUE),
        }
    }
}

struct Handles {
    color: Option<Rgb>,
    active: [Option<Handle>; Icon::COUNT],
    dimmed: [Option<Handle>; Icon::COUNT],
}
