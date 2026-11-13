//! Writes the Java JAXB project model and references a separately chosen Java catalogue.
use std::collections::BTreeMap;
use std::fmt::{Display, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kortty_terminal::TerminalProfile;

use crate::profile::projects::Name;
use crate::profile::{
    InitialTerminalSize, LocalFileTool, PaneLayout, ProjectTerminalAppearance, RemoteProjectTool,
    WindowLayout, WorkspaceLayout, invalid, local_profile, save_new_atomic,
};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::unsafe_text;
use crate::terminal_effects::{Selection, Speed};

use super::{
    CONNECTION_BYTES, PROJECT_BYTES, appearance, connection_nodes, document, effect_id, leaf_count,
    read_xml, routes,
};

#[cfg(test)]
mod tests;

// SplitLayoutRestorePlan.MAX_PANES in the pinned Java reference.
const JAVA_SPLIT_PANES: usize = 32;

#[derive(Debug)]
pub(crate) struct Export {
    pub(crate) destination: PathBuf,
    pub(crate) catalogue: Option<PathBuf>,
    pub(crate) windows: usize,
    pub(crate) tabs: usize,
    pub(crate) notes: Vec<String>,
    xml: String,
    bindings: Vec<Binding>,
}

#[derive(Debug)]
struct Binding {
    id: String,
    pane: PaneLayout,
    appearance: Option<ProjectTerminalAppearance>,
}

impl Binding {
    fn new(id: &str, pane: &PaneLayout, appearance: Option<&ProjectTerminalAppearance>) -> Self {
        Self {
            id: id.into(),
            pane: pane.clone(),
            appearance: appearance.cloned(),
        }
    }
}

impl Export {
    pub(crate) fn publish(&self) -> io::Result<()> {
        if let Some(path) = &self.catalogue {
            let current = Catalogue::read(path)?;
            for binding in &self.bindings {
                if current.unique(&binding.pane) != Ok(binding.id.as_str()) {
                    return Err(invalid(
                        "the Java connection mappings changed after the export preview",
                    ));
                }
                if let Some(appearance) = &binding.appearance
                    && !current.appearance_matches(&binding.id, appearance)
                {
                    return Err(invalid(
                        "the Java connection appearance changed after the export preview",
                    ));
                }
            }
        }
        save_new_atomic(&self.destination, self.xml.as_bytes())
    }
}

pub(crate) fn prepare(
    name: &Name,
    layout: &WorkspaceLayout,
    destination: PathBuf,
    catalogue: Option<PathBuf>,
) -> io::Result<Export> {
    layout.validate()?;
    if !destination.is_absolute()
        || destination.extension().and_then(|value| value.to_str()) != Some("kortty")
    {
        return Err(invalid("enter a new absolute .kortty file path"));
    }
    match fs::symlink_metadata(&destination) {
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "choose a new .kortty file; the output already exists",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if catalogue.is_none()
        && layout.windows.iter().any(|window| {
            window.tabs.iter().any(needs_catalogue)
                || window
                    .tab_terminal_appearance
                    .values()
                    .any(|appearance| appearance.style.is_some())
        })
    {
        return Err(invalid(
            "choose the Java connections.xml that matches this project's linked tabs",
        ));
    }
    let available = catalogue
        .as_deref()
        .map(Catalogue::read)
        .transpose()?
        .unwrap_or_default();
    let mut output = Xml::new();
    output.open("project");
    output.field("name", name.as_str());
    // Java's restoreOrDeferSavedTab skips even local terminals when this is false.
    output.field("autoReconnect", true);
    output.open("windows");
    let mut bindings = Vec::new();
    let mut notes = Vec::new();
    let mut tabs = 0;
    let mut first_omission = None;
    for (index, window) in layout.windows.iter().enumerate() {
        let (kept, omission) = write_window(
            &mut output,
            index + 1,
            window,
            &available,
            &mut bindings,
            &mut notes,
        )?;
        tabs += kept;
        first_omission = first_omission.or(omission);
    }
    output.close("windows");
    output.close("project");
    if tabs == 0 {
        return Err(invalid(first_omission.map_or_else(
            || "this project has no exportable Java tabs".into(),
            |reason| format!("this project has no exportable Java tabs ({reason})"),
        )));
    }
    if output.text.len() as u64 > PROJECT_BYTES {
        return Err(invalid("Java project exceeds the export size limit"));
    }
    Ok(Export {
        destination,
        catalogue: catalogue.filter(|_| !bindings.is_empty()),
        windows: layout.windows.len(),
        tabs,
        notes,
        xml: output.text,
        bindings,
    })
}

#[derive(Default)]
struct Catalogue(BTreeMap<String, CatalogueEntry>);

struct CatalogueEntry {
    pane: PaneLayout,
    initial_size: Option<InitialTerminalSize>,
    style: Option<Option<appearance::Style>>,
}

impl Catalogue {
    fn read(path: &Path) -> io::Result<Self> {
        let source = read_xml(path, CONNECTION_BYTES)?;
        let document = document(&source, CONNECTION_BYTES)?;
        let mut matches = BTreeMap::new();
        for (id, node) in connection_nodes(document.root_element())? {
            if id.trim() == id
                && !unsafe_text(id)
                && let Ok(Some(pane)) = routes::catalogue_pane(node)
            {
                let initial_size = routes::catalogue_initial_size(node).ok();
                let style = appearance::catalogue_style(node).ok();
                matches.insert(
                    id.to_owned(),
                    CatalogueEntry {
                        pane,
                        initial_size,
                        style,
                    },
                );
            }
        }
        Ok(Self(matches))
    }

    fn unique(&self, source: &PaneLayout) -> Result<&str, &'static str> {
        let mut candidates = self
            .0
            .iter()
            .filter(|(_, candidate)| equivalent(source, &candidate.pane));
        let Some((id, candidate)) = candidates.next() else {
            return Err(match source {
                PaneLayout::Terminal { initial_size, .. }
                    if !java_size_matches(*initial_size, routes::JAVA_DEFAULT_INITIAL_SIZE) =>
                {
                    "a custom initial terminal size requires a matching local shell in Java connections.xml"
                }
                _ => "no eligible Java connection matches the pane exactly",
            });
        };
        if candidates.next().is_some() {
            return Err("multiple Java connections match the pane; its ID is ambiguous");
        }
        if let PaneLayout::Terminal { initial_size, .. } | PaneLayout::Ssh { initial_size, .. } =
            source
        {
            let available = candidate
                .initial_size
                .ok_or("the matching Java connection has invalid initial terminal dimensions")?;
            if !java_size_matches(*initial_size, available) {
                return Err("the matching Java connection has different initial columns or rows");
            }
        }
        Ok(id)
    }

    fn appearance_matches(&self, id: &str, appearance: &ProjectTerminalAppearance) -> bool {
        self.0.get(id).and_then(|entry| entry.style.as_ref()) == Some(&appearance.style)
    }
}

fn java_size_matches(required: InitialTerminalSize, available: InitialTerminalSize) -> bool {
    required
        .columns
        .is_none_or(|columns| Some(columns) == available.columns)
        && required
            .rows
            .is_none_or(|rows| Some(rows) == available.rows)
}

fn equivalent(source: &PaneLayout, candidate: &PaneLayout) -> bool {
    match (source, candidate) {
        (
            PaneLayout::Ssh { term, target, .. },
            PaneLayout::Ssh {
                term: other_term,
                target: other,
                ..
            },
        ) => term == other_term && target.matches_imported_route(other),
        (PaneLayout::RemoteTool { target, .. }, PaneLayout::Ssh { target: other, .. }) => {
            target.matches_imported_route(other)
        }
        (
            PaneLayout::Serial {
                term, connection, ..
            },
            PaneLayout::Serial {
                term: other_term,
                connection: other,
                ..
            },
        ) => term == other_term && connection == other,
        (PaneLayout::Terminal { .. }, PaneLayout::Terminal { .. }) => {
            local_identity(source) == local_identity(candidate)
        }
        _ => false,
    }
}

fn local_identity(pane: &PaneLayout) -> Option<(TerminalProfile, IsolationLevel, bool)> {
    let PaneLayout::Terminal {
        vt100,
        term,
        security,
        ..
    } = pane
    else {
        return None;
    };
    if security
        .as_ref()
        .is_some_and(|settings| settings.saved_profile.is_some())
    {
        return None;
    }
    let isolation = security
        .as_ref()
        .map_or(IsolationLevel::None, |settings| settings.isolation);
    Some((
        local_profile(term.as_deref(), *vt100)?,
        isolation,
        security
            .as_ref()
            .is_some_and(|settings| settings.shell_integration),
    ))
}

fn needs_catalogue(pane: &PaneLayout) -> bool {
    match pane {
        PaneLayout::Ssh { .. } | PaneLayout::Serial { .. } | PaneLayout::RemoteTool { .. } => true,
        PaneLayout::Split { first, second, .. } => {
            needs_catalogue(first) || needs_catalogue(second)
        }
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Link<'a> {
    DefaultShell,
    Catalogue(&'a str),
}

fn link<'a>(
    pane: &PaneLayout,
    catalogue: &'a Catalogue,
    can_default: bool,
) -> Result<Link<'a>, &'static str> {
    match pane {
        PaneLayout::Ibm { .. } => {
            Err("a native IBM pane has no equivalent Java project connection")
        }
        PaneLayout::Terminal {
            security: Some(settings),
            ..
        } if settings.saved_profile.is_some() => {
            Err("a saved local shell must be restored from the private project store")
        }
        PaneLayout::Terminal {
            security: Some(settings),
            ..
        } if !settings.shell.is_default() => {
            Err("a selected Windows shell cannot be encoded in a portable Java project")
        }
        PaneLayout::Terminal { initial_size, .. }
            if local_identity(pane)
                == Some((TerminalProfile::Xterm, IsolationLevel::None, false))
                && can_default
                && java_size_matches(*initial_size, routes::JAVA_DEFAULT_INITIAL_SIZE) =>
        {
            Ok(Link::DefaultShell)
        }
        PaneLayout::Terminal { .. }
            if local_identity(pane)
                .is_some_and(|(_, isolation, _)| isolation == IsolationLevel::Sandbox) =>
        {
            Err("a local sandbox directory is device-specific")
        }
        _ => catalogue.unique(pane).map(Link::Catalogue),
    }
}

fn write_window(
    output: &mut Xml,
    index: usize,
    window: &WindowLayout,
    catalogue: &Catalogue,
    bindings: &mut Vec<Binding>,
    notes: &mut Vec<String>,
) -> io::Result<(usize, Option<&'static str>)> {
    output.open("window");
    output.field("windowId", uuid()?);
    output.open("geometry");
    if let Some(position) = window.position {
        output.field("x", position.x);
        output.field("y", position.y);
        if position.scale_factor.is_some() {
            notes.push(format!(
                "Window {index}: Java uses logical bounds and does not store monitor scale."
            ));
        }
    } else {
        notes.push(format!(
            "Window {index}: the source has no position. Java starts with coordinates (0, 0)."
        ));
    }
    output.field("width", window.width);
    output.field("height", window.height);
    output.field("maximized", window.maximized);
    output.field("fullScreen", false);
    output.close("geometry");
    output.open("tabs");
    let mut kept = Vec::new();
    let mut first_omission = None;
    for (tab, pane) in window.tabs.iter().enumerate() {
        let id = uuid()?;
        let effects = window
            .tab_terminal_effects
            .get(&tab)
            .filter(|effects| !effects.is_empty());
        let effect = effects.and_then(|effects| {
            let selection = *effects.values().next()?;
            (effects.len() == leaf_count(pane) && effects.values().all(|value| *value == selection))
                .then_some(selection)
        });
        let title = window.tab_titles.get(&tab).map(String::as_str);
        let group = window
            .tab_groups
            .as_ref()
            .and_then(|groups| groups.get(&tab))
            .map(String::as_str);
        let appearance = window.tab_terminal_appearance.get(&tab);
        match Tab::write(
            output.depth,
            pane,
            catalogue,
            &id,
            (title, group),
            effect,
            appearance,
        ) {
            Ok(draft) => {
                output.text.push_str(&draft.xml.text);
                bindings.extend(draft.bindings);
                kept.push((tab, id));
                if let PaneLayout::LocalFile { path, .. } = pane {
                    notes.push(format!(
                        "Window {index}, tab {} includes local file path {}",
                        tab + 1,
                        path.display()
                    ));
                }
                if let PaneLayout::RemoteTool { target, tool } = pane {
                    let scope = match tool {
                        RemoteProjectTool::Sftp { local, remote, .. } => format!(
                            "SFTP folders {} and {}",
                            local.as_deref().map_or_else(
                                || "local home".into(),
                                |path| path.display().to_string()
                            ),
                            remote.as_deref().unwrap_or("remote home")
                        ),
                        RemoteProjectTool::File { path, .. } => format!("remote file {path}"),
                    };
                    notes.push(format!(
                        "Window {index}, tab {} includes {} · {scope}",
                        tab + 1,
                        target.label()
                    ));
                }
                if draft.font_changes {
                    notes.push(format!("Window {index}, tab {}: Java stores one font size, so the first pane's size is used.", tab + 1));
                }
                if draft.focus_changes {
                    notes.push(format!("Window {index}, tab {}: Java's project format does not store which split pane has focus.", tab + 1));
                }
                if effects.is_some() && effect.is_none() {
                    notes.push(format!("Window {index}, tab {}: Java stores one effect and speed for all panes, so the individual pane effects were omitted.", tab + 1));
                }
            }
            Err(reason) => {
                first_omission.get_or_insert(reason);
                notes.push(format!(
                    "Window {index}, tab {} omitted ({reason}).",
                    tab + 1
                ));
            }
        }
    }
    output.close("tabs");
    if kept.is_empty() {
        output.field("activeTabIndex", -1);
    } else {
        let active = kept
            .iter()
            .position(|(tab, _)| *tab == window.active)
            .unwrap_or(0);
        if kept[active].0 != window.active {
            notes.push(format!("Window {index}: the first exported tab is selected because the original selection was omitted."));
        }
        output.field("activeTabIndex", active);
        output.field("activeSessionId", &kept[active].1);
    }
    if let Some(visible) = window.dashboard_visible {
        output.field("dashboardVisible", visible);
    }
    output.close("window");
    Ok((kept.len(), first_omission))
}

struct Tab {
    xml: Xml,
    bindings: Vec<Binding>,
    font: u8,
    font_changes: bool,
    focus_changes: bool,
    leaves: usize,
}

impl Tab {
    fn write(
        depth: usize,
        pane: &PaneLayout,
        catalogue: &Catalogue,
        id: &str,
        (title, group): (Option<&str>, Option<&str>),
        effect: Option<Selection>,
        appearance: Option<&ProjectTerminalAppearance>,
    ) -> Result<Self, &'static str> {
        if pane.is_project_tool() {
            let mut tab = Self {
                xml: Xml {
                    text: String::new(),
                    depth,
                },
                bindings: Vec::new(),
                font: 0,
                font_changes: false,
                focus_changes: false,
                leaves: 0,
            };
            tab.xml.open("tab");
            tab.xml.field("sessionId", id);
            if let Some(title) = title {
                tab.xml.field("tabTitle", title);
            }
            if let Some(group) = group {
                tab.xml.field("group", group);
            }
            match pane {
                PaneLayout::LocalFile { path, tool } => {
                    let path = path.to_str().ok_or("the local file path is not Unicode")?;
                    tab.file(path, *tool, false);
                }
                PaneLayout::RemoteTool { tool, .. } => {
                    let connection = catalogue.unique(pane)?;
                    tab.xml.field("connectionId", connection);
                    tab.bindings.push(Binding::new(connection, pane, None));
                    match tool {
                        RemoteProjectTool::File { path, tool } => tab.file(path, *tool, true),
                        RemoteProjectTool::Sftp {
                            local,
                            remote,
                            idle_minutes,
                        } => {
                            tab.xml.field("tabType", "SFTP_MANAGER");
                            if let Some(path) = local {
                                tab.xml.field(
                                    "sftpLocalPath",
                                    path.to_str().ok_or("the local folder is not Unicode")?,
                                );
                            }
                            if let Some(path) = remote {
                                tab.xml.field("sftpRemotePath", path);
                            }
                            if let Some(minutes) = idle_minutes {
                                tab.xml.field("sftpAutoCloseTimeout", minutes);
                            }
                        }
                    }
                }
                _ => unreachable!("checked project tool"),
            }
            tab.xml.close("tab");
            return Ok(tab);
        }
        let first = first_leaf(pane);
        let primary = link(
            first,
            catalogue,
            appearance.is_none_or(|appearance| appearance.style.is_none()),
        )?;
        let (font, focused) = presentation(first);
        let mut tab = Self {
            xml: Xml {
                text: String::new(),
                depth,
            },
            bindings: Vec::new(),
            font,
            font_changes: false,
            focus_changes: !focused,
            leaves: 0,
        };
        tab.xml.open("tab");
        tab.xml.field("tabType", "TERMINAL");
        tab.xml.field("sessionId", id);
        if let Some(title) = title {
            tab.xml.field("tabTitle", title);
        }
        if let Some(group) = group {
            tab.xml.field("group", group);
        }
        match primary {
            Link::DefaultShell => {
                // A real UUID is the unsaved local shell identity used by the JAXB tab model.
                tab.xml.field("connectionId", id);
                tab.xml.field("localShell", true);
            }
            Link::Catalogue(connection) => {
                if let Some(appearance) = appearance
                    && !catalogue.appearance_matches(connection, appearance)
                {
                    return Err(
                        "the matching Java connection has different terminal appearance settings",
                    );
                }
                tab.xml.field("connectionId", connection);
                tab.bindings
                    .push(Binding::new(connection, first, appearance));
            }
        }
        if let Some(font_zoom) = appearance.map_or(Some(font), |appearance| appearance.font_zoom) {
            tab.xml.field("fontSizeOverride", font_zoom);
        }
        if let Some(selection) = effect {
            tab.xml
                .field("terminalEffectPluginId", effect_id(selection.effect));
            if selection.speed != Speed::DEFAULT {
                tab.xml
                    .field("terminalEffectAnimationSpeed", selection.speed);
            }
        }
        if matches!(pane, PaneLayout::Split { .. }) {
            tab.split("splitPaneState", pane, catalogue, primary)?;
        }
        tab.xml.close("tab");
        Ok(tab)
    }

    fn file(&mut self, path: &str, tool: LocalFileTool, remote: bool) {
        match tool {
            LocalFileTool::Editor => {
                self.xml.field("tabType", "FILE_EDITOR");
                self.xml.field("editorFilePath", path);
                self.xml.field("editorIsRemote", remote);
            }
            LocalFileTool::Image { zoom } => {
                self.xml.field("tabType", "IMAGE_VIEWER");
                self.xml.field("imageFilePath", path);
                self.xml.field("imageIsRemote", remote);
                self.xml.field("imageZoomLevel", zoom);
            }
        }
    }

    fn split(
        &mut self,
        tag: &str,
        pane: &PaneLayout,
        catalogue: &Catalogue,
        primary: Link<'_>,
    ) -> Result<(), &'static str> {
        self.xml.open(tag);
        match pane {
            PaneLayout::Split {
                vertical,
                ratio,
                first,
                second,
            } => {
                self.xml.field(
                    "orientation",
                    if *vertical { "HORIZONTAL" } else { "VERTICAL" },
                );
                self.xml.field("dividerPosition", ratio);
                self.split("leftChild", first, catalogue, primary)?;
                self.split("rightChild", second, catalogue, primary)?;
            }
            leaf => {
                let index = self.leaves;
                self.leaves += 1;
                if self.leaves > JAVA_SPLIT_PANES {
                    return Err("the split exceeds Java's 32-pane limit");
                }
                let connection = if index == 0 {
                    primary
                } else {
                    link(leaf, catalogue, primary == Link::DefaultShell)?
                };
                self.xml.field("widgetIndex", index);
                if index != 0
                    && let Link::Catalogue(id) = connection
                {
                    if connection != primary {
                        self.xml.field("connectionId", id);
                    }
                    self.bindings.push(Binding::new(id, leaf, None));
                }
                self.font_changes |= presentation(leaf).0 != self.font;
            }
        }
        self.xml.close(tag);
        Ok(())
    }
}

fn first_leaf(mut pane: &PaneLayout) -> &PaneLayout {
    while let PaneLayout::Split { first, .. } = pane {
        pane = first;
    }
    pane
}

fn presentation(pane: &PaneLayout) -> (u8, bool) {
    match pane {
        PaneLayout::Terminal { font, focused, .. }
        | PaneLayout::Serial { font, focused, .. }
        | PaneLayout::Ssh { font, focused, .. }
        | PaneLayout::Ibm { font, focused, .. } => (*font, *focused),
        PaneLayout::Split { .. } | PaneLayout::LocalFile { .. } | PaneLayout::RemoteTool { .. } => {
            unreachable!("presentation is taken from a terminal leaf")
        }
    }
}

fn uuid() -> io::Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut value = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            value.push('-');
        }
        write!(value, "{byte:02x}").expect("write to String");
    }
    Ok(value)
}

struct Xml {
    text: String,
    depth: usize,
}

impl Xml {
    fn new() -> Self {
        Self {
            text: "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n".into(),
            depth: 0,
        }
    }

    fn indent(&mut self) {
        for _ in 0..self.depth {
            self.text.push_str("    ");
        }
    }

    fn open(&mut self, tag: &str) {
        self.indent();
        writeln!(self.text, "<{tag}>").expect("write to String");
        self.depth += 1;
    }

    fn close(&mut self, tag: &str) {
        self.depth -= 1;
        self.indent();
        writeln!(self.text, "</{tag}>").expect("write to String");
    }

    fn field(&mut self, tag: &str, value: impl Display) {
        self.indent();
        write!(self.text, "<{tag}>").expect("write to String");
        for character in value.to_string().chars() {
            match character {
                '&' => self.text.push_str("&amp;"),
                '<' => self.text.push_str("&lt;"),
                '>' => self.text.push_str("&gt;"),
                _ => self.text.push(character),
            }
        }
        writeln!(self.text, "</{tag}>").expect("write to String");
    }
}
