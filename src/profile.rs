use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use kortty_terminal::TerminalProfile;
use serde::{Deserialize, Serialize};

use crate::session::{IbmKind, SerialConnection};
use crate::session_security::LocalSettings;
use crate::terminal_effects::Selection as TerminalEffect;
use crate::workspace::SshTarget;

pub(crate) mod dialog_geometry;
pub(crate) mod projects;
pub(crate) mod session;

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 256 * 1024;
const MAX_WINDOWS: usize = 16;
const MAX_TABS_PER_WINDOW: usize = 32;
const MAX_TAB_TITLE_CHARS: usize = 120;
const MAX_TOTAL_PANES: usize = 128;
const MAX_SPLIT_DEPTH: usize = 16;
const MAX_POSITION: f32 = 100_000.0;
const MAX_PLACEMENT_BYTES: u64 = 4096;
pub(crate) const MIN_TERMINAL_FONT: u8 = 8;
pub(crate) const MAX_TERMINAL_FONT: u8 = 72;
static NEXT_SAVE: AtomicU64 = AtomicU64::new(0);
static RUNTIME_ROOT: OnceLock<PathBuf> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceLayout {
    version: u32,
    pub windows: Vec<WindowLayout>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowLayout {
    pub width: f32,
    pub height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<SavedPosition>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard_visible: Option<bool>,
    pub active: usize,
    pub tabs: Vec<PaneLayout>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tab_titles: BTreeMap<usize, String>,
    /// Absent in native layouts written before tab groups were supported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tab_groups: Option<BTreeMap<usize, String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tab_primary_panes: BTreeMap<usize, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) tab_terminal_effects: BTreeMap<usize, PaneEffects>,
}

pub(crate) type PaneEffects = BTreeMap<usize, TerminalEffect>;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedPosition {
    pub x: f32,
    pub y: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_factor: Option<f32>,
}

impl SavedPosition {
    pub fn new(x: f32, y: f32) -> Option<Self> {
        let limit = -MAX_POSITION..=MAX_POSITION;
        (limit.contains(&x) && limit.contains(&y)).then_some(Self {
            x,
            y,
            scale_factor: None,
        })
    }

    fn valid(self) -> bool {
        Self::new(self.x, self.y).is_some()
            && self
                .scale_factor
                .is_none_or(|scale| self.physical_pixels(scale).is_some())
    }

    pub fn physical_pixels(self, scale: f32) -> Option<(i32, i32)> {
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let coordinate = |value: f32| {
            let value = (f64::from(value) * f64::from(scale)).round();
            (f64::from(i32::MIN)..=f64::from(i32::MAX))
                .contains(&value)
                .then_some(value as i32)
        };
        Some((coordinate(self.x)?, coordinate(self.y)?))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowPlacement {
    pub width: f32,
    pub height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<SavedPosition>,
}

impl WindowPlacement {
    fn valid(self) -> bool {
        Self::valid_size(self.width, self.height) && self.position.is_none_or(SavedPosition::valid)
    }

    pub fn valid_size(width: f32, height: f32) -> bool {
        (320.0..=8192.0).contains(&width) && (200.0..=8192.0).contains(&height)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct LastWindow {
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<SavedPosition>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) maximized: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) dashboard_visible: bool,
}

impl LastWindow {
    pub(crate) fn placement(self) -> Option<WindowPlacement> {
        Some(WindowPlacement {
            width: self.width?,
            height: self.height?,
            position: self.position,
        })
    }

    pub(crate) fn set_placement(&mut self, placement: WindowPlacement, maximized: bool) {
        self.width = Some(placement.width);
        self.height = Some(placement.height);
        self.position = placement.position;
        self.maximized = maximized;
    }

    fn valid(self) -> bool {
        if let Some(placement) = self.placement() {
            return placement.valid();
        }
        self.width.is_none() && self.height.is_none() && self.position.is_none() && !self.maximized
    }
}

impl WindowLayout {
    pub fn placement(&self) -> WindowPlacement {
        WindowPlacement {
            width: self.width,
            height: self.height,
            position: self.position,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PaneLayout {
    Terminal {
        font: u8,
        focused: bool,
        #[serde(default)]
        vt100: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        term: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        security: Option<LocalSettings>,
    },
    Ibm {
        font: u8,
        focused: bool,
        kind: IbmKind,
    },
    Serial {
        font: u8,
        focused: bool,
        term: String,
        connection: SerialConnection,
    },
    Ssh {
        font: u8,
        focused: bool,
        term: String,
        target: SshTarget,
    },
    Split {
        vertical: bool,
        ratio: f32,
        first: Box<PaneLayout>,
        second: Box<PaneLayout>,
    },
}

impl WorkspaceLayout {
    pub fn new(windows: Vec<WindowLayout>) -> Self {
        Self {
            version: VERSION,
            windows,
        }
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.version != VERSION || self.windows.is_empty() || self.windows.len() > MAX_WINDOWS {
            return Err(invalid("unsupported layout version or window count"));
        }
        let mut panes = 0;
        for window in &self.windows {
            if !window.placement().valid()
                || window.tabs.len() > MAX_TABS_PER_WINDOW
                || window.active >= window.tabs.len().max(1)
            {
                return Err(invalid("invalid window size, tab count, or selection"));
            }
            for (index, tab) in window.tabs.iter().enumerate() {
                let before = panes;
                if tab.validate(0, &mut panes)? != 1 {
                    return Err(invalid("each tab must have exactly one focused pane"));
                }
                if window
                    .tab_primary_panes
                    .get(&index)
                    .is_some_and(|primary| *primary >= panes - before)
                {
                    return Err(invalid("invalid primary pane index"));
                }
                if window
                    .tab_terminal_effects
                    .get(&index)
                    .is_some_and(|effects| {
                        effects.iter().any(|(&pane, effect)| {
                            pane >= panes - before || effect.speed != effect.speed()
                        })
                    })
                {
                    return Err(invalid("invalid terminal effect speed or pane index"));
                }
            }
            if window
                .tab_primary_panes
                .keys()
                .any(|&index| index >= window.tabs.len())
            {
                return Err(invalid("invalid primary pane tab index"));
            }
            if window
                .tab_terminal_effects
                .keys()
                .any(|&index| index >= window.tabs.len())
            {
                return Err(invalid("invalid terminal effect tab index"));
            }
            if window.tab_titles.iter().any(|(&index, title)| {
                index >= window.tabs.len()
                    || title.is_empty()
                    || normalize_tab_title(title) != *title
            }) {
                return Err(invalid("invalid tab title or tab title index"));
            }
            if window.tab_groups.iter().flatten().any(|(&index, group)| {
                index >= window.tabs.len() || normalize_tab_group(group).as_ref() != Some(group)
            }) {
                return Err(invalid("invalid tab group or tab group index"));
            }
        }
        Ok(())
    }
}

pub(crate) fn normalize_tab_title(title: &str) -> String {
    let clean = clean_tab_text(title, true);
    let java_whitespace = |character: char| {
        character.is_whitespace() && !matches!(character, '\u{a0}' | '\u{2007}' | '\u{202f}')
    };
    let mut title: String = clean
        .trim_matches(java_whitespace)
        .chars()
        .take(MAX_TAB_TITLE_CHARS)
        .collect();
    title.truncate(title.trim_end_matches(java_whitespace).len());
    title
}

/// Keeps the full identity of a valid connection folder instead of using the title limit.
pub(crate) fn normalize_tab_group(group: &str) -> Option<String> {
    let group = group.trim();
    if group.is_empty() || group.len() > crate::connection_store::MAX_GROUP_PATH_BYTES {
        return None;
    }
    let clean = clean_tab_text(group, false);
    let group = clean.trim();
    (!group.is_empty()).then(|| group.to_owned())
}

fn clean_tab_text(value: &str, normalize_unicode_line_breaks: bool) -> String {
    let mut clean = String::new();
    for character in value.chars() {
        match character {
            character
                if matches!(character, '\t'..='\r' | '\u{85}')
                    || (normalize_unicode_line_breaks
                        && matches!(character, '\u{2028}' | '\u{2029}')) =>
            {
                if !clean.ends_with(' ') {
                    clean.push(' ');
                }
            }
            '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => {}
            character if !character.is_control() => clean.push(character),
            _ => {}
        }
    }
    clean
}

impl PaneLayout {
    fn validate(&self, depth: usize, panes: &mut usize) -> io::Result<usize> {
        if depth > MAX_SPLIT_DEPTH {
            return Err(invalid("too many nested splits"));
        }
        if let Self::Terminal { vt100, term, .. } = self
            && local_profile(term.as_deref(), *vt100).is_none()
        {
            return Err(invalid("unknown or conflicting terminal profile"));
        }
        if let Self::Terminal {
            security: Some(settings),
            ..
        } = self
        {
            settings.validate()?;
        }
        if let Self::Serial {
            term, connection, ..
        } = self
        {
            if TerminalProfile::from_term_name(term).is_none() {
                return Err(invalid("unknown serial terminal profile"));
            }
            connection.validate().map_err(invalid)?;
        }
        if let Self::Ssh { term, target, .. } = self {
            if TerminalProfile::from_term_name(term).is_none() {
                return Err(invalid("unknown SSH terminal profile"));
            }
            target.validate().map_err(invalid)?;
        }
        match self {
            Self::Terminal { font, focused, .. }
            | Self::Ibm { font, focused, .. }
            | Self::Serial { font, focused, .. }
            | Self::Ssh { font, focused, .. } => {
                *panes += 1;
                if *panes > MAX_TOTAL_PANES
                    || !(MIN_TERMINAL_FONT..=MAX_TERMINAL_FONT).contains(font)
                {
                    return Err(invalid("invalid terminal pane count or font size"));
                }
                Ok(usize::from(*focused))
            }
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !ratio.is_finite() || !(0.0..=1.0).contains(ratio) {
                    return Err(invalid("invalid divider ratio"));
                }
                Ok(first.validate(depth + 1, panes)? + second.validate(depth + 1, panes)?)
            }
        }
    }
}

pub fn local_profile(term: Option<&str>, vt100: bool) -> Option<TerminalProfile> {
    let profile = match term {
        Some(name) => TerminalProfile::from_term_name(name)?,
        None if vt100 => TerminalProfile::Vt100,
        None => TerminalProfile::Xterm,
    };
    (!vt100 || profile == TerminalProfile::Vt100).then_some(profile)
}

pub fn load() -> io::Result<WorkspaceLayout> {
    load_from(&path()?)
}

pub fn save(layout: &WorkspaceLayout) -> io::Result<()> {
    save_to(&path()?, layout)
}

pub(crate) fn load_last_window() -> io::Result<LastWindow> {
    read_last_window(&path()?.with_file_name("last-window.json"))
}

pub(crate) fn save_last_window(window: LastWindow) -> io::Result<()> {
    write_last_window(&path()?.with_file_name("last-window.json"), window)
}

fn read_last_window(path: &Path) -> io::Result<LastWindow> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("window placement requires a regular file"));
    }
    let mut content = Vec::new();
    File::open(path)?
        .take(MAX_PLACEMENT_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > MAX_PLACEMENT_BYTES {
        return Err(invalid("window placement file exceeds the size limit"));
    }
    let window: LastWindow = serde_json::from_slice(&content).map_err(invalid)?;
    if !window.valid() {
        return Err(invalid("invalid window placement"));
    }
    Ok(window)
}

fn write_last_window(path: &Path, window: LastWindow) -> io::Result<()> {
    if !window.valid() {
        return Err(invalid("invalid window placement"));
    }
    save_atomic(path, &serde_json::to_vec(&window).map_err(invalid)?)
}

pub fn jobs_path() -> io::Result<PathBuf> {
    Ok(path()?.with_file_name("jobs.json"))
}

pub fn connections_path() -> io::Result<PathBuf> {
    Ok(path()?.with_file_name("connections.json"))
}

pub(crate) fn local_dock_path() -> io::Result<PathBuf> {
    Ok(path()?.with_file_name("local-dock.json"))
}

pub(crate) fn window_settings_path() -> io::Result<PathBuf> {
    Ok(path()?.with_file_name("window-settings.json"))
}

pub(crate) fn session_security_path() -> io::Result<PathBuf> {
    Ok(root()?.join("session-security.json"))
}

#[cfg(unix)]
pub(crate) fn open_preference(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};

    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    Ok(File::from(open(path, flags, Mode::empty())?))
}

#[cfg(windows)]
pub(crate) fn open_preference(path: &Path) -> io::Result<File> {
    kortty_files::open_regular_without_reparse(path)
}

pub fn vault_path() -> io::Result<PathBuf> {
    Ok(path()?.with_file_name("vault").join("vault.bin"))
}

pub fn ssh_known_hosts() -> io::Result<PathBuf> {
    let directory = path()?.with_file_name("ssh");
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&directory)?;
    let metadata = fs::symlink_metadata(&directory)?;
    if !metadata.is_dir() {
        return Err(invalid("SSH trust directory must be a directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "SSH trust directory must be private to the current user",
            ));
        }
    }
    Ok(directory.join("known_hosts"))
}

fn path() -> io::Result<PathBuf> {
    Ok(root()?.join("layout.json"))
}

pub(crate) fn root() -> io::Result<PathBuf> {
    if let Some(root) = RUNTIME_ROOT.get() {
        return Ok(root.clone());
    }
    let home = || env::var_os("HOME").map(PathBuf::from);
    let directory = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|home| home.join("Library/Application Support"))
    } else {
        env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .or_else(|| home().map(|home| home.join(".config")))
    };
    let directory = directory
        .filter(|path| path.is_absolute())
        .ok_or_else(|| io::Error::other("profile directory is unavailable"))?;
    Ok(directory.join("kortty"))
}

/// Bind desktop stores to the same resolved profile directory as the startup lease.
pub(crate) fn set_runtime_root(root: PathBuf) {
    let _ = RUNTIME_ROOT.set(root);
}

fn load_from(path: &Path) -> io::Result<WorkspaceLayout> {
    let mut content = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > MAX_BYTES {
        return Err(invalid("layout exceeds the profile size limit"));
    }
    let layout: WorkspaceLayout = serde_json::from_slice(&content).map_err(invalid)?;
    layout.validate()?;
    Ok(layout)
}

fn save_to(path: &Path, layout: &WorkspaceLayout) -> io::Result<()> {
    layout.validate()?;
    let contents = serde_json::to_vec_pretty(layout).map_err(invalid)?;
    if contents.len() as u64 > MAX_BYTES {
        return Err(invalid("layout exceeds the profile size limit"));
    }
    save_atomic(path, &contents)
}

pub(crate) fn save_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| invalid("profile has no directory"))?;
    fs::create_dir_all(directory)?;
    write_staged(
        directory,
        |file| file.write_all(contents),
        |temporary| fs::rename(temporary, path),
    )
}

pub(crate) fn save_new_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    create_new_atomic(path, |file| file.write_all(contents))
}

pub(crate) fn create_new_atomic(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    let directory = path
        .parent()
        .filter(|_| path.is_absolute() && path.file_name().is_some())
        .ok_or_else(|| invalid("enter an absolute file path"))?;
    write_staged(directory, write, |temporary| publish_new(temporary, path))
}

pub(crate) fn publish_new(temporary: &Path, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        Ok(renameat_with(
            CWD,
            temporary,
            CWD,
            path,
            RenameFlags::NOREPLACE,
        )?)
    }
    #[cfg(windows)]
    {
        kortty_files::rename_no_replace(temporary, path)
    }
}

fn write_staged(
    directory: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
    publish: impl FnOnce(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    loop {
        let index = NEXT_SAVE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(".layout-{}-{index}.tmp", std::process::id()));
        let mut file = match options.open(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = write(&mut file).and_then(|()| file.sync_all());
        drop(file);
        let result = result.and_then(|()| publish(&temporary));
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
}

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_profiles_and_the_legacy_vt100_flag_restore_exactly() {
        assert_eq!(local_profile(None, false), Some(TerminalProfile::Xterm));
        assert_eq!(local_profile(None, true), Some(TerminalProfile::Vt100));
        assert_eq!(
            local_profile(Some("wy160"), false),
            Some(TerminalProfile::Wy160)
        );
        assert_eq!(local_profile(Some("wy160"), true), None);
        assert_eq!(local_profile(Some("wy160\n"), false), None);
        assert_eq!(local_profile(Some("IBM-3279-2-E"), false), None);
    }

    fn example() -> WorkspaceLayout {
        WorkspaceLayout::new(vec![WindowLayout {
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            width: 920.0,
            height: 620.0,
            position: SavedPosition::new(-90.0, 45.0),
            maximized: false,
            dashboard_visible: None,
            active: 0,
            tabs: vec![PaneLayout::Split {
                vertical: true,
                ratio: 0.3,
                first: Box::new(PaneLayout::Terminal {
                    font: 12,
                    focused: false,
                    vt100: false,
                    term: Some("dtterm".into()),
                    security: None,
                }),
                second: Box::new(PaneLayout::Terminal {
                    font: 17,
                    focused: true,
                    vt100: true,
                    term: None,
                    security: None,
                }),
            }],
        }])
    }

    #[test]
    fn workspace_layout_without_coordinates_keeps_its_saved_size() {
        let mut document = serde_json::to_value(example()).unwrap();
        document["windows"][0]
            .as_object_mut()
            .unwrap()
            .remove("position");
        let layout: WorkspaceLayout = serde_json::from_value(document).unwrap();
        layout.validate().unwrap();
        assert_eq!(
            layout.windows[0].placement(),
            WindowPlacement {
                width: 920.0,
                height: 620.0,
                position: None
            }
        );
    }

    #[test]
    fn workspace_layout_effects_are_additive_and_reject_invalid_coordinates_or_speed() {
        use crate::terminal_effects::Effect;

        let mut layout = example();
        let value = serde_json::to_value(&layout).unwrap();
        assert!(value["windows"][0].get("tab_terminal_effects").is_none());
        assert_eq!(
            serde_json::from_value::<WorkspaceLayout>(value).unwrap(),
            layout
        );

        let selection = TerminalEffect {
            effect: Effect::NeonCity,
            speed: 7,
        };
        layout.windows[0]
            .tab_terminal_effects
            .insert(0, BTreeMap::from([(1, selection)]));
        layout.validate().unwrap();
        let value = serde_json::to_value(&layout).unwrap();
        assert_eq!(
            serde_json::from_value::<WorkspaceLayout>(value).unwrap(),
            layout
        );

        for (tab, pane, speed) in [(1, 1, 7), (0, 2, 7), (0, 1, 0), (0, 1, 100)] {
            layout.windows[0].tab_terminal_effects = BTreeMap::from([(
                tab,
                BTreeMap::from([(pane, TerminalEffect { speed, ..selection })]),
            )]);
            assert!(layout.validate().is_err());
        }
    }

    #[test]
    fn workspace_layout_accepts_the_maximum_terminal_font() {
        let mut layout = example();
        let PaneLayout::Split { first, .. } = &mut layout.windows[0].tabs[0] else {
            panic!("expected the example split");
        };
        let PaneLayout::Terminal { font, .. } = first.as_mut() else {
            panic!("expected the example terminal pane");
        };
        *font = MAX_TERMINAL_FONT;
        let loaded: WorkspaceLayout =
            serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
        loaded.validate().unwrap();
        assert_eq!(loaded, layout);
    }

    #[test]
    fn last_window_state_reads_old_geometry_and_rejects_unusable_bounds() -> io::Result<()> {
        let directory = env::temp_dir().join(format!("kortty-window-test-{}", std::process::id()));
        let path = directory.join("last-window.json");
        let result = (|| {
            let mut placement = example().windows[0].placement();
            placement.position.as_mut().unwrap().scale_factor = Some(1.5);
            save_atomic(&path, &serde_json::to_vec(&placement).unwrap())?;
            let mut state = read_last_window(&path)?;
            assert_eq!(state.placement(), Some(placement));
            assert!(!state.maximized);
            assert!(!state.dashboard_visible);
            state.set_placement(placement, true);
            state.dashboard_visible = true;
            write_last_window(&path, state)?;
            assert_eq!(read_last_window(&path)?, state);
            let invalid_size = WindowPlacement {
                width: 0.0,
                ..placement
            };
            let mut invalid_scale = placement;
            invalid_scale.position.as_mut().unwrap().scale_factor = Some(0.0);
            for invalid in [invalid_size, invalid_scale] {
                let mut invalid_state = state;
                invalid_state.set_placement(invalid, false);
                assert_eq!(
                    write_last_window(&path, invalid_state).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
            }
            assert_eq!(read_last_window(&path)?, state);
            let dashboard_only = LastWindow {
                dashboard_visible: true,
                ..LastWindow::default()
            };
            write_last_window(&path, dashboard_only)?;
            assert_eq!(read_last_window(&path)?, dashboard_only);
            fs::write(
                &path,
                b"{\"width\":920,\"height\":620,\"position\":{\"x\":500000,\"y\":0}}",
            )?;
            assert_eq!(
                read_last_window(&path).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            Ok(())
        })();
        let _ = fs::remove_dir_all(directory);
        result
    }

    #[test]
    fn atomically_replaces_layout_and_preserves_the_last_good_value_on_error() -> io::Result<()> {
        let directory = env::temp_dir().join(format!("kortty-profile-test-{}", std::process::id()));
        let path = directory.join("layout.json");
        let result = (|| {
            let mut layout = example();
            save_to(&path, &layout)?;
            assert_eq!(load_from(&path)?, layout);
            layout.windows[0].active = 8;
            assert_eq!(
                save_to(&path, &layout).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
            assert_eq!(load_from(&path)?, example());
            layout.windows[0].active = 0;
            layout.windows[0].width = 1200.0;
            save_to(&path, &layout)?;
            assert_eq!(load_from(&path)?, layout);
            Ok(())
        })();
        let _ = fs::remove_dir_all(directory);
        result
    }
}
