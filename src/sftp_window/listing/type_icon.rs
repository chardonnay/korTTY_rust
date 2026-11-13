//! SFTP file category glyphs from the pinned Java FileBrowserIcons reference.

use std::sync::LazyLock;

use iced::Theme;
use iced::widget::svg::{self, Handle};

use super::EntryKind;

const PATHS: [&str; 6] = [
    "M1 5 L1 14 L15 14 L15 4 L8 4 L7 2 L1 2 Z",
    "M5 4 L1.5 8 L5 12 L6.2 10.9 L3.7 8 L6.2 5.1 Z M11 4 L14.5 8 L11 12 L9.8 10.9 L12.3 8 L9.8 5.1 Z",
    "M1.5 13 L6 6.5 L9 10.2 L11 7.8 L14.5 13 Z M9.8 4.6 a1.7 1.7 0 1 0 3.4 0 a1.7 1.7 0 1 0 -3.4 0 Z",
    "M2 2.5 H14 V6 H2 Z M3 7 H13 V13.8 H3 Z M6.3 8.3 H9.7 V10 H6.3 Z",
    "M2.5 3.5 H13.5 V5 H2.5 Z M2.5 7 H13.5 V8.5 H2.5 Z M2.5 10.5 H10 V12 H2.5 Z",
    "M3 1 L10 1 L14 5 L14 15 L3 15 Z",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Glyph {
    Folder,
    Code,
    Image,
    Archive,
    Document,
    File,
}

impl Glyph {
    fn for_entry(kind: EntryKind, name: &str) -> Self {
        if kind == EntryKind::Directory {
            return Self::Folder;
        }
        let extension = name
            .rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or_else(String::new, |(_, extension)| extension.to_ascii_lowercase());
        match extension.as_str() {
            "java" | "kt" | "kts" | "py" | "js" | "ts" | "jsx" | "tsx" | "mjs" | "sh" | "bash"
            | "zsh" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "rs" | "go" | "rb" | "php"
            | "swift" | "css" | "scss" | "html" | "htm" | "sql" | "pl" | "lua" | "gradle"
            | "bat" | "ps1" => Self::Code,
            "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" | "tif" | "tiff"
            | "heic" => Self::Image,
            "zip" | "tar" | "gz" | "tgz" | "7z" | "rar" | "jar" | "war" | "bz2" | "xz" | "zst"
            | "dmg" | "iso" | "deb" | "rpm" => Self::Archive,
            "txt" | "md" | "pdf" | "doc" | "docx" | "odt" | "rtf" | "log" | "properties"
            | "json" | "yaml" | "yml" | "xml" | "csv" | "tsv" | "ini" | "conf" | "toml" => {
                Self::Document
            }
            _ => Self::File,
        }
    }
}

pub(super) fn widget(kind: EntryKind, name: &str, dimmed: bool) -> svg::Svg<'_> {
    static HANDLES: LazyLock<[Handle; PATHS.len()]> = LazyLock::new(|| {
        PATHS.map(|path| {
            Handle::from_memory(
                format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path fill-rule="evenodd" d="{path}"/></svg>"#
                )
                .into_bytes(),
            )
        })
    });
    let glyph = Glyph::for_entry(kind, name);
    svg::Svg::new(HANDLES[glyph as usize].clone())
        .width(15)
        .height(15)
        .style(move |theme: &Theme, _| {
            let mut color = theme.palette().text;
            if dimmed {
                color.a *= 0.75;
            }
            svg::Style { color: Some(color) }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_listing_type_icons_follow_the_reference_file_categories() {
        assert_eq!(
            Glyph::for_entry(EntryKind::Directory, "image.png"),
            Glyph::Folder
        );
        for (name, expected) in [
            ("app.RS", Glyph::Code),
            ("image.HEIC", Glyph::Image),
            ("archive.tar.gz", Glyph::Archive),
            (".settings.toml", Glyph::Document),
            (".toml", Glyph::File),
            ("file.", Glyph::File),
        ] {
            assert_eq!(Glyph::for_entry(EntryKind::File, name), expected);
        }
    }
}
