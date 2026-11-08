//! Two-layer glyphs from the pinned MIT-licensed Java ToolbarIcons.

use std::sync::OnceLock;

use iced::widget::svg::{Handle, Svg};

#[derive(Clone, Copy)]
pub(super) enum Icon {
    AddServer,
    Edit,
    Duplicate,
    Delete,
    Folder,
    FolderAdd,
    Rename,
    Search,
    ViewList,
    Star,
    Clock,
    Tab,
    Tag,
    Server,
    Jump,
    Tunnel,
    Connect,
    Sftp,
    Colors,
    ImportConnections,
    ExportConnections,
    ExportOverview,
}

impl Icon {
    const ALL: [Self; 22] = [
        Self::AddServer,
        Self::Edit,
        Self::Duplicate,
        Self::Delete,
        Self::Folder,
        Self::FolderAdd,
        Self::Rename,
        Self::Search,
        Self::ViewList,
        Self::Star,
        Self::Clock,
        Self::Tab,
        Self::Tag,
        Self::Server,
        Self::Jump,
        Self::Tunnel,
        Self::Connect,
        Self::Sftp,
        Self::Colors,
        Self::ImportConnections,
        Self::ExportConnections,
        Self::ExportOverview,
    ];

    pub(super) fn widget<'a>(self, size: u32, enabled: bool) -> Svg<'a> {
        static ACTIVE: OnceLock<[Handle; Icon::ALL.len()]> = OnceLock::new();
        static DIMMED: OnceLock<[Handle; Icon::ALL.len()]> = OnceLock::new();
        let cache = if enabled { &ACTIVE } else { &DIMMED };
        let handles = cache.get_or_init(|| Self::ALL.map(|icon| icon.handle(enabled)));
        Svg::new(handles[self as usize].clone())
            .width(size)
            .height(size)
    }

    fn handle(self, enabled: bool) -> Handle {
        let (outline, accent, color) = match self {
            Self::AddServer => (
                "M5 3 H19 Q21 3 21 5 V9 Q21 11 19 11 H5 Q3 11 3 9 V5 Q3 3 5 3 M12 21 H5 Q3 21 3 19 V15 Q3 13 5 13 H12 M16 7 H17",
                "M18 14 V22 M14 18 H22",
                "#13a8ad",
            ),
            Self::Edit => (
                "M4 20 L5 15 L15.5 4.5 Q17 3 18.5 4.5 L19.5 5.5 Q21 7 19.5 8.5 L9 19 Z",
                "M13.5 6.5 L17.5 10.5 M14 20.5 H20",
                "#368af5",
            ),
            Self::Duplicate => (
                "M8 8 H20 V20 H8 Z M16 8 V4 H4 V16 H8",
                "M14 11 V17 M11 14 H17",
                "#13a8ad",
            ),
            Self::Delete => (
                "M4 6 H20 M9 6 V3.5 H15 V6 M6 6 L7 21 H17 L18 6",
                "M10 10 V17 M14 10 V17",
                "#e5534b",
            ),
            Self::Folder => (
                "M3 7 V5 Q3 3 5 3 H10 L13 6 H19 Q21 6 21 8 V19 Q21 21 19 21 H5 Q3 21 3 19 Z",
                "M3 10 H21",
                "#dc9825",
            ),
            Self::FolderAdd => (
                "M3 7 V5 Q3 3 5 3 H10 L13 6 H19 Q21 6 21 8 V19 Q21 21 19 21 H5 Q3 21 3 19 Z",
                "M12 10.5 V17.5 M8.5 14 H15.5",
                "#13a8ad",
            ),
            Self::Rename => (
                "M14 7 H20 Q21 7 21 8 V16 Q21 17 20 17 H14 M10 17 H4 Q3 17 3 16 V8 Q3 7 4 7 H10",
                "M12 3 V21 M9.5 3 H14.5 M9.5 21 H14.5",
                "#368af5",
            ),
            Self::Colors => (
                "M12 3 C1 3 0 17 8 20 C13 23 11 17 16 17 C24 18 24 3 12 3 Z M7 8 H7.1 M13 6 H13.1 M18 9 H18.1",
                "M7 14 A1 1 0 1 0 7.01 14",
                "#368af5",
            ),
            Self::Search => (
                "M10 3 A7 7 0 1 0 10.01 3 M15 15 L22 22",
                "M7 10 H13",
                "#368af5",
            ),
            Self::ViewList => (
                "M8.5 6 H21 M8.5 12 H21 M8.5 18 H21",
                "M3.5 6 H4.5 M3.5 12 H4.5 M3.5 18 H4.5",
                "#368af5",
            ),
            Self::Star => (
                "M12 2 L15 8 L22 9 L17 14 L18 21 L12 18 L6 21 L7 14 L2 9 L9 8 Z",
                "M12 9 V14",
                "#368af5",
            ),
            Self::Clock => ("M12 2 A10 10 0 1 0 12.01 2", "M12 6 V12 L16 15", "#368af5"),
            Self::Tab => (
                "M3 20 V6 Q3 3 6 3 H18 Q21 3 21 6 V20",
                "M8 12 H16 M12 8 V16",
                "#368af5",
            ),
            Self::Tag => (
                "M3 4 Q3 3 4 3 H11 L21 13 L13 21 L3 11 Z",
                "M7.5 6 A1.5 1.5 0 1 0 7.51 6",
                "#dc9825",
            ),
            Self::Server => (
                "M5 3 H19 Q21 3 21 5 V9 Q21 11 19 11 H5 Q3 11 3 9 V5 Q3 3 5 3 M5 13 H19 Q21 13 21 15 V19 Q21 21 19 21 H5 Q3 21 3 19 V15 Q3 13 5 13",
                "M16 7 H17 M16 17 H17",
                "#368af5",
            ),
            Self::Jump => (
                "M2 16 H7 V21 H2 Z M17 16 H22 V21 H17 Z M9.5 3 H14.5 V8 H9.5 Z",
                "M4.5 16 Q4.5 5.5 9.5 5.5 M14.5 5.5 Q19.5 5.5 19.5 13.5 M18 12 L19.5 14 L21 12",
                "#dc9825",
            ),
            Self::Tunnel => (
                "M2 21 H22 M3.5 21 V12 A8.5 8.5 0 0 1 20.5 12 V21",
                "M8 21 V15 A4 4 0 0 1 16 15 V21",
                "#13a8ad",
            ),
            Self::Connect => (
                "M9 2 V7 M15 2 V7 M6 7 H18 V11 Q18 16 12 16 Q6 16 6 11 Z",
                "M12 16 V18.5 Q12 21.5 15 21.5 H19",
                "#368af5",
            ),
            Self::Sftp => (
                "M3 7 V5 Q3 3 5 3 H10 L13 6 H19 Q21 6 21 8 V19 Q21 21 19 21 H5 Q3 21 3 19 Z",
                "M7 11 H17 L14 8 M17 16 H7 L10 19",
                "#13a8ad",
            ),
            Self::ImportConnections => (
                "M4 14 V19 Q4 20 5 20 H19 Q20 20 20 19 V14",
                "M12 3 V15 M7.5 10.5 L12 15 L16.5 10.5",
                "#13a8ad",
            ),
            Self::ExportConnections => (
                "M4 14 V19 Q4 20 5 20 H19 Q20 20 20 19 V14",
                "M12 15 V3 M7.5 7.5 L12 3 L16.5 7.5",
                "#13a8ad",
            ),
            Self::ExportOverview => (
                "M6 3 H14 L19 8 V21 H6 Z M14 3 V8 H19",
                "M10.5 11 Q9 11 9 12.5 V13.2 Q9 14 8 14 Q9 14 9 14.8 V15.5 Q9 17 10.5 17 M14.5 11 Q16 11 16 12.5 V13.2 Q16 14 17 14 Q16 14 16 14.8 V15.5 Q16 17 14.5 17",
                "#13a8ad",
            ),
        };
        let opacity = if enabled { 1.0 } else { 0.4 };
        Handle::from_memory(format!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><g fill="none" opacity="{opacity}" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path stroke="#aab4bf" d="{outline}"/><path stroke="{color}" d="{accent}"/></g></svg>"##
        ).into_bytes())
    }
}
