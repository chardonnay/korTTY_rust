//! Portable theme definitions from the Java application's themes.xml.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::io::{self, Read as _};
use std::path::Path;

use roxmltree::{Document, Node, ParsingOptions};

use crate::profile::{MAX_TERMINAL_FONT, MIN_TERMINAL_FONT};
use crate::ssh_dialog::unsafe_text;

use super::*;

const MAX_BYTES: usize = 256 * 1024;
const MAX_NODES: u32 = 2048;
const MAX_DEFINITIONS: usize = 64;
const MAX_ID_BYTES: usize = 160;
const MAX_FONT_BYTES: usize = 160;
const MAX_PATH_BYTES: usize = 4096;
const AGENT_PALETTE_VERSION: i32 = 2;
const PANEL_FIELDS: [&str; 6] = [
    "agentPanelBackgroundColor",
    "agentPanelBorderColor",
    "agentPanelTextColor",
    "agentPanelMutedTextColor",
    "agentPanelAccentColor",
    "agentPanelErrorColor",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Exchange {
    pub(super) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) font_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) background_image_path: Option<String>,
    #[serde(default, skip_serializing_if = "no_agent_panel_colors")]
    pub(super) panel_colors: AgentPanelColors,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    built_in: bool,
}

impl Exchange {
    pub(super) fn generate() -> io::Result<Self> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes).map_err(io::Error::other)?;
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let mut id = String::with_capacity(36);
        for (index, byte) in bytes.into_iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                id.push('-');
            }
            write!(id, "{byte:02x}").expect("write to String");
        }
        Ok(Self {
            id,
            font_family: None,
            background_image_path: None,
            panel_colors: [None; PANEL_FIELDS.len()],
            built_in: false,
        })
    }

    pub(super) fn valid(&self) -> bool {
        valid_id(&self.id)
            && Preset::from_java_id(&self.id).is_none()
            && self
                .font_family
                .as_deref()
                .is_none_or(|value| valid_text(value, MAX_FONT_BYTES))
            && self
                .background_image_path
                .as_deref()
                .is_none_or(|value| valid_text(value, MAX_PATH_BYTES))
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.trim().is_empty() && valid_text(id, MAX_ID_BYTES)
}

fn valid_text(value: &str, maximum: usize) -> bool {
    value.len() <= maximum && !unsafe_text(value) && !value.contains(['\u{fffe}', '\u{ffff}'])
}

#[derive(Debug)]
pub(crate) struct Library {
    entries: Vec<Definition>,
    built_in: usize,
}

#[derive(Debug)]
struct Definition {
    name: String,
    scheme: Scheme,
    exchange: Exchange,
    limited: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Import {
    pub(crate) added: usize,
    pub(crate) unchanged: usize,
    pub(crate) built_in: usize,
    pub(crate) limited: usize,
}

pub(crate) fn read(path: &Path) -> io::Result<Library> {
    if !path.is_absolute() {
        return Err(invalid(
            "Choose an absolute path to a regular themes.xml file.",
        ));
    }
    let file = crate::profile::open_preference(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(invalid(
            "The selected themes.xml path is not a regular file.",
        ));
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err(invalid("Java themes XML exceeds 256 KiB."));
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    decode(&bytes)
}

pub(crate) fn decode(bytes: &[u8]) -> io::Result<Library> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Java themes XML exceeds 256 KiB."));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("Java themes XML must be UTF-8."))?;
    let document = Document::parse_with_options(
        text,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid("Invalid Java themes XML or XML node limit exceeded."))?;
    let root = document.root_element();
    if !root.has_tag_name("themes") || root.tag_name().namespace().is_some() {
        return Err(invalid("Expected a Java themes document."));
    }
    let agent_palette_version = root
        .attribute("agentPaletteVersion")
        .map(|value| value.trim().parse::<i32>())
        .transpose()
        .map_err(|_| invalid("Invalid Java Agent palette version."))?
        .unwrap_or(0);
    let mut library = Library {
        entries: Vec::new(),
        built_in: 0,
    };
    let mut ids = BTreeSet::new();
    for row in root.children().filter(Node::is_element) {
        if !row.has_tag_name("theme")
            || row.tag_name().namespace().is_some()
            || ids.len() == MAX_DEFINITIONS
        {
            return Err(invalid(
                "Invalid Java theme entry or theme count limit exceeded.",
            ));
        }
        let id = row
            .attribute("id")
            .filter(|id| valid_id(id))
            .ok_or_else(|| invalid("A Java theme has an invalid or missing ID."))?;
        if !ids.insert(id) {
            return Err(invalid("Java themes XML repeats a theme ID."));
        }
        if Preset::from_java_id(id).is_some() {
            library.built_in += 1;
        } else {
            library.entries.push(definition(
                row,
                id,
                agent_palette_version < AGENT_PALETTE_VERSION,
            )?);
        }
    }
    Ok(library)
}

fn definition(row: Node<'_, '_>, id: &str, legacy_palette: bool) -> io::Result<Definition> {
    let name = scalar(row, "name")?
        .filter(|name| valid_name(name) && valid_text(name, MAX_NAME))
        .ok_or_else(|| invalid("A Java theme requires a name of 1–72 bytes."))?;
    let font = scalar(row, "fontFamily")?.unwrap_or_else(|| FontFamily::Monospaced.label().into());
    if !valid_text(&font, MAX_FONT_BYTES) {
        return Err(invalid("A Java theme has an invalid font family."));
    }
    let family = FontFamily::ALL
        .into_iter()
        .find(|family| family.label().eq_ignore_ascii_case(&font));
    let font_size = match scalar(row, "fontSize")? {
        Some(size) => size
            .trim()
            .parse::<i32>()
            .map_err(|_| invalid("A Java theme has an invalid font size."))?,
        None => 14,
    };
    let font_size = if font_size <= 0 { 14 } else { font_size };
    let font_size = u8::try_from(font_size)
        .ok()
        .filter(|size| (MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(size))
        .ok_or_else(|| invalid("A Java theme font size must be between 8 and 72."))?;
    let cursor_style = match scalar(row, "cursorStyle")? {
        Some(style) => {
            let style = style.trim().to_ascii_lowercase().replace('_', "-");
            serde_json::from_value(style.into())
                .map_err(|_| invalid("A Java theme has an unsupported cursor style."))?
        }
        None => CursorStyle::default(),
    };
    let scheme = Scheme {
        foreground: color(row, "foregroundColor")?.unwrap_or(Rgb::from_hex(0xffffff)),
        background: color(row, "backgroundColor")?.unwrap_or(Rgb::from_hex(0x1e1e1e)),
        cursor: color(row, "cursorColor")?.unwrap_or(Rgb::from_hex(0xffffff)),
        cursor_style,
        family: family.unwrap_or_default(),
        font_size,
    };
    let mut panel_colors = [None; PANEL_FIELDS.len()];
    for (field, output) in PANEL_FIELDS.into_iter().zip(&mut panel_colors) {
        *output = color(row, field)?;
    }
    if legacy_palette {
        panel_colors = panel_colors::migrate_legacy_colors(scheme, panel_colors);
    }
    let built_in = match scalar(row, "builtIn")?.as_deref().map(str::trim) {
        Some("true" | "1") => true,
        None | Some("false" | "0") => false,
        _ => return Err(invalid("A Java theme has an invalid built-in flag.")),
    };
    let exchange = Exchange {
        id: id.to_owned(),
        font_family: (font != scheme.family.label()).then_some(font),
        background_image_path: scalar(row, "backgroundImagePath")?,
        panel_colors,
        built_in,
    };
    if !exchange.valid() {
        return Err(invalid("A Java theme contains invalid interchange fields."));
    }
    let limited = family.is_none()
        || exchange.background_image_path.is_some()
        || built_in
        || row.children().filter(Node::is_element).any(|node| {
            node.tag_name().namespace().is_some()
                || !matches!(
                    node.tag_name().name(),
                    "name"
                        | "fontFamily"
                        | "fontSize"
                        | "foregroundColor"
                        | "backgroundColor"
                        | "cursorColor"
                        | "cursorStyle"
                        | "backgroundImagePath"
                        | "builtIn"
                ) && !PANEL_FIELDS.contains(&node.tag_name().name())
        });
    Ok(Definition {
        name,
        scheme,
        exchange,
        limited,
    })
}

fn scalar(row: Node<'_, '_>, name: &str) -> io::Result<Option<String>> {
    let mut fields = row.children().filter(|node| {
        node.is_element() && node.has_tag_name(name) && node.tag_name().namespace().is_none()
    });
    let Some(field) = fields.next() else {
        return Ok(None);
    };
    if fields.next().is_some() || field.children().any(|node| node.is_element()) {
        return Err(invalid(
            "A Java theme contains a duplicate or non-scalar field.",
        ));
    }
    Ok(Some(
        field
            .children()
            .filter(Node::is_text)
            .filter_map(|node| node.text())
            .collect(),
    ))
}

fn color(row: Node<'_, '_>, field: &str) -> io::Result<Option<Rgb>> {
    scalar(row, field)?
        .as_deref()
        .map(|value| {
            Rgb::parse(value).ok_or_else(|| invalid("Java theme colors must use #RRGGBB."))
        })
        .transpose()
}

pub(crate) fn merge(themes: &mut Themes, library: Library) -> io::Result<Import> {
    merge_inner(themes, library, false)
}

pub(crate) fn merge_backup(themes: &mut Themes, incoming: Themes) -> io::Result<Import> {
    let entries = incoming
        .edited
        .into_iter()
        .map(|entry| {
            let exchange = entry
                .java
                .ok_or_else(|| invalid("Expected a Java theme in the backup"))?;
            Ok(Definition {
                name: entry.name,
                scheme: entry.scheme,
                exchange,
                limited: false,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    merge_inner(
        themes,
        Library {
            entries,
            built_in: 0,
        },
        true,
    )
}

fn merge_inner(themes: &mut Themes, library: Library, keep_existing: bool) -> io::Result<Import> {
    let mut draft = themes.clone();
    let mut report = Import {
        built_in: library.built_in,
        ..Import::default()
    };
    for entry in library.entries {
        report.limited += usize::from(entry.limited);
        if let Some(existing) = draft.edited.iter().find(|theme| {
            theme
                .java
                .as_ref()
                .is_some_and(|exchange| exchange.id == entry.exchange.id)
        }) {
            if !keep_existing
                && (existing.name != entry.name
                    || existing.scheme != entry.scheme
                    || existing.java.as_ref() != Some(&entry.exchange))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "The Java theme ID ‘{}’ already has a different definition.",
                        entry.exchange.id
                    ),
                ));
            }
            report.unchanged += 1;
        } else {
            let id = draft.allocate_custom_id().map_err(io::Error::other)?;
            draft.edited.push(StoredTheme {
                id,
                name: entry.name,
                scheme: entry.scheme,
                java: Some(entry.exchange),
                background_asset: None,
                agent_panel_colors: Default::default(),
            });
            report.added += 1;
        }
    }
    if !draft.valid() {
        return Err(invalid("The merged theme catalogue is invalid."));
    }
    *themes = draft;
    Ok(report)
}

pub(crate) fn encode(themes: &Themes) -> io::Result<Vec<u8>> {
    if !themes.valid() {
        return Err(invalid("The theme catalogue is invalid."));
    }
    let mut xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<themes agentPaletteVersion=\"{AGENT_PALETTE_VERSION}\">\n",
    );
    for theme in themes.edited.iter().filter(|theme| !theme.id.built_in()) {
        let exchange = theme
            .java
            .as_ref()
            .ok_or_else(|| invalid("Assign Java exchange IDs to existing custom themes first."))?;
        if !valid_text(&theme.name, MAX_NAME) {
            return Err(invalid("A theme name cannot be represented in Java XML."));
        }
        xml.push_str("  <theme id=\"");
        escape(&mut xml, &exchange.id);
        xml.push_str("\">\n");
        let scheme = theme.scheme;
        field(&mut xml, "name", &theme.name);
        field(
            &mut xml,
            "fontFamily",
            exchange
                .font_family
                .as_deref()
                .unwrap_or(scheme.family.label()),
        );
        field(&mut xml, "fontSize", &scheme.font_size.to_string());
        field(&mut xml, "foregroundColor", &scheme.foreground.to_string());
        field(&mut xml, "backgroundColor", &scheme.background.to_string());
        field(&mut xml, "cursorColor", &scheme.cursor.to_string());
        field(&mut xml, "cursorStyle", java_cursor(scheme.cursor_style));
        if let Some(path) = &exchange.background_image_path {
            field(&mut xml, "backgroundImagePath", path);
        }
        for (name, value) in PANEL_FIELDS.into_iter().zip(exchange.panel_colors) {
            if let Some(value) = value {
                field(&mut xml, name, &value.to_string());
            }
        }
        field(
            &mut xml,
            "builtIn",
            if exchange.built_in { "true" } else { "false" },
        );
        xml.push_str("  </theme>\n");
    }
    xml.push_str("</themes>\n");
    if xml.len() > MAX_BYTES {
        return Err(invalid("Java themes XML exceeds 256 KiB."));
    }
    Ok(xml.into_bytes())
}

fn java_cursor(style: CursorStyle) -> &'static str {
    match style {
        CursorStyle::BlinkBlock => "BLINK_BLOCK",
        CursorStyle::SteadyBlock => "STEADY_BLOCK",
        CursorStyle::BlinkUnderline => "BLINK_UNDERLINE",
        CursorStyle::SteadyUnderline => "STEADY_UNDERLINE",
        CursorStyle::BlinkBar => "BLINK_VERTICAL_BAR",
        CursorStyle::SteadyBar => "STEADY_VERTICAL_BAR",
    }
}

fn field(xml: &mut String, name: &str, value: &str) {
    write!(xml, "    <{name}>").expect("write to String");
    escape(xml, value);
    writeln!(xml, "</{name}>").expect("write to String");
}

fn escape(xml: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '&' => xml.push_str("&amp;"),
            '<' => xml.push_str("&lt;"),
            '>' => xml.push_str("&gt;"),
            '"' => xml.push_str("&quot;"),
            '\'' => xml.push_str("&apos;"),
            ch => xml.push(ch),
        }
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_store::ThemeOverride;

    const CUSTOM: &str = r##"<themes agentPaletteVersion="2">
      <theme id="nord"><name>Stock</name><builtIn>true</builtIn></theme>
      <theme id="NORD"><name>Custom &amp; &lt;Nord&gt;</name><fontFamily>Fira Code</fontFamily>
        <fontSize>21</fontSize><foregroundColor>#123456</foregroundColor>
        <backgroundColor>#112233</backgroundColor><cursorColor>#090807</cursorColor>
        <cursorStyle>STEADY_VERTICAL_BAR</cursorStyle>
        <backgroundImagePath>C:\Private\a &amp; b.png</backgroundImagePath>
        <agentPanelAccentColor>#9966CC</agentPanelAccentColor><builtIn>false</builtIn>
      </theme>
    </themes>"##;

    #[test]
    fn java_agent_palette_migrates_only_matching_legacy_fields() {
        let fields = r##"
            <agentPanelBackgroundColor>#1D3C2C</agentPanelBackgroundColor>
            <agentPanelBorderColor>#1B6340</agentPanelBorderColor>
            <agentPanelTextColor>#E0E1E2</agentPanelTextColor>
            <agentPanelAccentColor>#18C26E</agentPanelAccentColor>
            <agentPanelErrorColor>#963333</agentPanelErrorColor>
        "##;
        let decode_colors = |version, fields| {
            let xml = format!(
                "<themes{version}><theme id=\"legacy\"><name>Legacy</name>{fields}</theme></themes>"
            );
            decode(xml.as_bytes()).unwrap().entries[0]
                .exchange
                .panel_colors
        };
        let migrated = [0x282d33, 0x324962, 0xe0e1e2, 0xb0b0b0, 0x4f9cf0, 0x963333]
            .map(|value| Some(Rgb::from_hex(value)));
        assert_eq!(decode_colors("", fields), migrated);
        assert_eq!(
            decode_colors(r#" agentPaletteVersion="1""#, fields),
            migrated
        );
        let overridden = fields.replace("#1B6340", "#774433");
        let mut customized = migrated;
        customized[1] = Some(Rgb::from_hex(0x774433));
        assert_eq!(decode_colors("", overridden.as_str()), customized);

        let explicit = [
            Some(Rgb::from_hex(0x1d3c2c)),
            Some(Rgb::from_hex(0x1b6340)),
            Some(Rgb::from_hex(0xe0e1e2)),
            None,
            Some(Rgb::from_hex(0x18c26e)),
            Some(Rgb::from_hex(0x963333)),
        ];
        assert_eq!(
            decode_colors(r#" agentPaletteVersion="2""#, fields),
            explicit
        );
        assert_eq!(
            decode_colors(r#" agentPaletteVersion="3""#, fields),
            explicit
        );
    }

    #[test]
    fn java_theme_round_trip_keeps_exact_custom_ids_and_retained_fields() {
        for preset in Preset::ALL {
            assert_eq!(serde_json::to_value(preset).unwrap(), preset.java_id());
        }
        let mut themes = Themes::default();
        let stock = themes.find(Id::BuiltIn(Preset::Nord)).unwrap().scheme;
        themes.select(Some(Id::BuiltIn(Preset::Dark)));
        let link = ThemeOverride::from_java("NORD".into()).unwrap();
        assert!(themes.find_java("NORD").is_none());
        assert_eq!(link.scheme(&themes), Some(stock));
        assert_eq!(
            ThemeOverride::from_java("\n SOLARIZED_DARK\t".into()),
            Some(ThemeOverride::BuiltIn(Preset::SolarizedDark))
        );
        assert_eq!(
            merge(&mut themes, decode(CUSTOM.as_bytes()).unwrap()).unwrap(),
            Import {
                added: 1,
                unchanged: 0,
                built_in: 1,
                limited: 1
            }
        );
        let custom = themes.find_java("NORD").unwrap();
        assert_eq!(custom.name, "Custom & <Nord>");
        assert_eq!(custom.scheme.foreground, Rgb::from_hex(0x123456));
        assert_eq!(custom.scheme.cursor_style, CursorStyle::SteadyBar);
        assert_eq!(custom.scheme.family, FontFamily::Monospaced);
        assert_eq!(link.scheme(&themes), Some(custom.scheme));
        assert_eq!(themes.java_id(custom.id), Some("NORD"));
        assert_eq!(themes.find_java("nord").unwrap().scheme, stock);
        assert_eq!(themes.selected(), Some(Id::BuiltIn(Preset::Dark)));

        let xml = encode(&themes).unwrap();
        let text = std::str::from_utf8(&xml).unwrap();
        assert!(text.contains("<fontFamily>Fira Code</fontFamily>"));
        assert!(text.contains("<agentPanelAccentColor>#9966CC</agentPanelAccentColor>"));
        assert!(
            text.contains(r"<backgroundImagePath>C:\Private\a &amp; b.png</backgroundImagePath>")
        );
        assert_eq!(
            merge(&mut themes, decode(&xml).unwrap()).unwrap(),
            Import {
                added: 0,
                unchanged: 1,
                built_in: 0,
                limited: 1
            }
        );

        let mut fresh = Themes::default();
        let local = fresh
            .store(None, "Custom & <Nord>", Scheme::default())
            .unwrap();
        assert_eq!(merge(&mut fresh, decode(&xml).unwrap()).unwrap().added, 1);
        let imported = fresh.find_java("NORD").unwrap();
        assert_ne!(imported.id, local);
        assert_eq!(link.scheme(&fresh), Some(imported.scheme));
        let before = fresh.clone();
        assert_eq!(
            merge(&mut fresh, decode(&xml).unwrap()).unwrap().unchanged,
            1
        );
        assert_eq!(fresh, before);
    }

    #[test]
    #[ignore = "requires the classpath for Java kortty at 1ebbe724ee84"]
    fn pinned_java_theme_manager_reads_and_saves_the_rust_theme() {
        let profile = crate::snippets::tests::Profile::new();
        let path = profile.0.join("themes.xml");
        let mut themes = Themes::default();
        merge(&mut themes, decode(CUSTOM.as_bytes()).unwrap()).unwrap();
        let id = themes.find_java("NORD").unwrap().id;
        let colors = [0x203040, 0x324252, 0xF0E0D0, 0xA0B0C0, 0x517191, 0xE05060]
            .map(|color| Some(Rgb::from_hex(color)));
        themes.set_agent_panel_colors(id, colors);
        std::fs::write(&path, encode(&themes).unwrap()).unwrap();
        let checker = profile.0.join("CheckThemeExport.java");
        std::fs::write(&checker, include_str!("java/CheckThemeExport.java")).unwrap();
        let classpath = std::env::var_os("KORTTY_JAVA_PROJECT_CLASSPATH")
            .expect("set KORTTY_JAVA_PROJECT_CLASSPATH to the pinned Java runtime classpath");
        let result = std::process::Command::new("java")
            .arg(format!("-Duser.home={}", profile.0.display()))
            .arg(format!("-Djava.io.tmpdir={}", profile.0.display()))
            .arg("-cp")
            .arg(classpath)
            .arg(checker)
            .arg(&profile.0)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&result.stdout);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(result.status.success(), "{stdout}\n{stderr}");
        assert!(stdout.contains("Pinned Java ThemeManager read and saved the Rust theme."));
        let mut fresh = Themes::default();
        merge(&mut fresh, read(&path).unwrap()).unwrap();
        let imported = fresh.find_java("NORD").unwrap();
        assert_eq!(fresh.agent_panel_colors(imported.id), Some(colors));
        assert_eq!(
            fresh.java_background_path(imported.id),
            themes.java_background_path(id)
        );
        println!("{stdout}");
    }

    #[test]
    fn conflicting_theme_id_rolls_back_the_entire_import() {
        let mut themes = Themes::default();
        merge(&mut themes, decode(CUSTOM.as_bytes()).unwrap()).unwrap();
        let before = themes.clone();
        let conflict = br#"<themes>
          <theme id="new"><name>New theme</name></theme>
          <theme id="NORD"><name>Different definition</name></theme>
        </themes>"#;
        assert!(merge(&mut themes, decode(conflict).unwrap()).is_err());
        assert_eq!(themes, before);
        assert!(themes.find_java("new").is_none());

        for _ in themes.edited.len()..MAX_CUSTOM - 1 {
            themes.store(None, "Native", Scheme::default()).unwrap();
        }
        let full = themes.clone();
        let additions = br#"<themes><theme id="one"><name>One</name></theme>
          <theme id="two"><name>Two</name></theme></themes>"#;
        assert!(merge(&mut themes, decode(additions).unwrap()).is_err());
        assert_eq!(themes, full);
    }

    #[test]
    fn legacy_and_new_native_themes_keep_stable_java_export_ids() {
        let mut themes = Themes::default();
        let id = themes.store(None, "Native", Scheme::default()).unwrap();
        let assigned = themes.java_id(id).unwrap().to_owned();
        assert_eq!(assigned.len(), 36);
        let original = encode(&themes).unwrap();
        let mut fresh = Themes::default();
        merge(&mut fresh, decode(&original).unwrap()).unwrap();
        assert_eq!(encode(&fresh).unwrap(), original);
        themes
            .store(Some(id), "Native revised", Scheme::default())
            .unwrap();
        assert_eq!(themes.java_id(id), Some(assigned.as_str()));

        let mut legacy = serde_json::to_value(&themes).unwrap();
        legacy["edited"][0].as_object_mut().unwrap().remove("java");
        let mut legacy: Themes = serde_json::from_value(legacy).unwrap();
        assert!(legacy.java_id(id).is_none());
        assert!(encode(&legacy).is_err());
        legacy.prepare_java_export().unwrap();
        let generated = legacy.java_id(id).unwrap().to_owned();
        assert_ne!(generated, assigned);
        let mut reopened: Themes =
            serde_json::from_slice(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        reopened.prepare_java_export().unwrap();
        assert_eq!(reopened.java_id(id), Some(generated.as_str()));
        assert_eq!(reopened.find_java(&generated).unwrap().id, id);
    }

    #[test]
    fn java_theme_xml_rejects_dtd_duplicate_fields_and_excess_input() {
        for bad in [
            r#"<!DOCTYPE themes [<!ENTITY name "unsafe">]><themes><theme id="x"><name>&name;</name></theme></themes>"#,
            r#"<themes><theme id="x"><name>First</name><name>Second</name></theme></themes>"#,
            r#"<themes><theme id="x"><name>First</name></theme><theme id="x"><name>Second</name></theme></themes>"#,
            r#"<themes><theme id="x"><name><other>Invalid</other></name></theme></themes>"#,
            r#"<themes><theme id="x"><name>Large</name><fontSize>256</fontSize></theme></themes>"#,
        ] {
            assert!(decode(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(decode(&vec![b' '; MAX_BYTES + 1]).is_err());
        let mut xml = String::from("<themes>");
        for index in 0..=MAX_DEFINITIONS {
            write!(xml, "<theme id=\"t{index}\"><name>Same</name></theme>").unwrap();
        }
        xml.push_str("</themes>");
        assert!(decode(xml.as_bytes()).is_err());
    }
}
