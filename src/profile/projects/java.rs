//! Reads projects and the optional route catalogue written by the Java JAXB stores.
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use roxmltree::{Document, Node, ParsingOptions};

use crate::profile::{
    LocalFileTool, MAX_SPLIT_DEPTH, MAX_TABS_PER_WINDOW, MAX_TOTAL_PANES, MAX_WINDOWS, PaneLayout,
    ProjectTerminalAppearance, RemoteProjectTool, SavedPosition, WindowLayout, WorkspaceLayout,
    invalid, normalize_tab_group, normalize_tab_title, open_preference,
};
use crate::terminal_effects::{Effect, Selection, Speed};

pub(crate) mod appearance;
pub(crate) mod export;
mod routes;

pub(super) const PROJECT_BYTES: u64 = 2 * 1024 * 1024;
const CONNECTION_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 1024;
const XML_NODES: u32 = 100_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Note {
    MissingConnection,
    UnsupportedConnection,
    Mosh,
    PrivateConnection,
    ToolTab,
    Geometry,
    Appearance,
    InitialSize,
    SessionData,
}

impl Note {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::MissingConnection => {
                "Panes omitted because their Java connection ID could not be resolved"
            }
            Self::UnsupportedConnection => {
                "Panes omitted because their connection or terminal mode is unsupported"
            }
            Self::Mosh => "Mosh panes omitted",
            Self::PrivateConnection => "Incognito panes omitted",
            Self::ToolTab => "Java tool tabs with unsupported file locations or types omitted",
            Self::Geometry => {
                "Windows using supported normal bounds instead of the complete Java window state"
            }
            Self::Appearance => "Java-only appearance or encoding settings skipped",
            Self::InitialSize => "Unsupported initial terminal sizes skipped",
            Self::SessionData => "Saved text, commands, paths or other session-only data skipped",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Imported {
    pub(crate) layout: WorkspaceLayout,
    pub(crate) notes: BTreeMap<Note, usize>,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) auto_reconnect: bool,
}

pub(crate) fn read(project: &Path, connections: Option<&Path>) -> io::Result<Imported> {
    if project.extension().and_then(|extension| extension.to_str()) != Some("kortty") {
        return Err(invalid("choose a Java .kortty project file"));
    }
    let source = read_xml(project, PROJECT_BYTES)?;
    read_project_text(&source, connections)
}

pub(super) fn read_project_text(source: &str, connections: Option<&Path>) -> io::Result<Imported> {
    let routes = connections
        .map(|path| read_xml(path, CONNECTION_BYTES))
        .transpose()?;
    parse(source, routes.as_deref())
}

pub(super) fn read_backup(project: &[u8], connections: Option<&[u8]>) -> io::Result<Imported> {
    let project = std::str::from_utf8(project).map_err(invalid)?;
    let connections = connections
        .map(std::str::from_utf8)
        .transpose()
        .map_err(invalid)?;
    parse(project, connections)
}

fn read_xml(path: &Path, limit: u64) -> io::Result<String> {
    if !path.is_absolute() {
        return Err(invalid("enter an absolute path for each Java XML file"));
    }
    let file = open_preference(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("choose a regular Java XML file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("Java XML file exceeds the import size limit"));
    }
    String::from_utf8(bytes).map_err(invalid)
}

fn document(source: &str, limit: u64) -> io::Result<Document<'_>> {
    if source.len() as u64 > limit {
        return Err(invalid("Java XML file exceeds the import size limit"));
    }
    Document::parse_with_options(
        source.strip_prefix('\u{feff}').unwrap_or(source),
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(invalid)
}

fn parse(project: &str, connections: Option<&str>) -> io::Result<Imported> {
    let project = document(project, PROJECT_BYTES)?;
    let root = project.root_element();
    if !tag(root, "project") {
        return Err(invalid("expected a Java project XML document"));
    }
    let connections = connections
        .map(|source| document(source, CONNECTION_BYTES))
        .transpose()?;
    let mut importer = Importer {
        connections: BTreeMap::new(),
        notes: BTreeMap::new(),
        panes: 0,
    };
    if let Some(connections) = &connections {
        importer.connections = connection_nodes(connections.root_element())?;
    }
    let windows = required_child(root, "windows")?;
    let mut layout = Vec::new();
    for window in windows.children().filter(|node| tag(*node, "window")) {
        if layout.len() == MAX_WINDOWS {
            return Err(invalid(
                "Java project exceeds the native window count limit",
            ));
        }
        layout.push(importer.window(window)?);
    }
    if layout.iter().all(|window| window.tabs.is_empty()) {
        let notes = importer
            .notes
            .iter()
            .map(|(note, count)| format!("{} ({count})", note.description()))
            .collect::<Vec<_>>()
            .join(". ");
        let message = if notes.is_empty() {
            String::new()
        } else {
            format!(" {notes}.")
        };
        return Err(invalid(format!(
            "Java project contains no supported tabs.{message}"
        )));
    }
    let layout = WorkspaceLayout::new(layout);
    layout.validate()?;
    Ok(Imported {
        layout,
        notes: importer.notes,
        auto_reconnect: flag(root, "autoReconnect")?,
        name: value(root, "name")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into(),
        description: value(root, "description")
            .ok()
            .flatten()
            .unwrap_or_default()
            .into(),
    })
}

fn connection_nodes<'a, 'input>(
    root: Node<'a, 'input>,
) -> io::Result<BTreeMap<&'a str, Node<'a, 'input>>> {
    if !tag(root, "connections")
        || root
            .attribute("schemaVersion")
            .is_some_and(|version| !matches!(version, "1" | "2"))
    {
        return Err(invalid(
            "expected Java connections.xml with its supported schema version",
        ));
    }
    let mut nodes = BTreeMap::new();
    for (index, node) in root
        .children()
        .filter(|node| tag(*node, "connection"))
        .enumerate()
    {
        if index >= MAX_CONNECTIONS {
            return Err(invalid(
                "Java connection file exceeds its connection count limit",
            ));
        }
        let Some(id) = node
            .attribute("id")
            .filter(|id| !id.is_empty() && id.len() <= 128)
        else {
            continue;
        };
        if nodes.insert(id, node).is_some() {
            return Err(invalid(
                "Java connections.xml contains duplicate connection IDs",
            ));
        }
    }
    Ok(nodes)
}

struct Importer<'a, 'input> {
    connections: BTreeMap<&'a str, Node<'a, 'input>>,
    notes: BTreeMap<Note, usize>,
    panes: usize,
}

impl Importer<'_, '_> {
    fn note(&mut self, note: Note) {
        *self.notes.entry(note).or_default() += 1;
    }

    fn window(&mut self, window: Node<'_, '_>) -> io::Result<WindowLayout> {
        let geometry = child(window, "geometry")?;
        let width = geometry
            .map_or(Ok(None), |node| number::<f32>(node, "width"))?
            .unwrap_or(800.0);
        let height = geometry
            .map_or(Ok(None), |node| number::<f32>(node, "height"))?
            .unwrap_or(600.0);
        if !width.is_finite() || !height.is_finite() {
            return Err(invalid("Java project window bounds must be finite"));
        }
        let normal_width = width.clamp(320.0, 8192.0);
        let normal_height = height.clamp(200.0, 8192.0);
        let mut altered = width != normal_width || height != normal_height;
        let (position, maximized) = if let Some(geometry) = geometry {
            altered |= flag(geometry, "fullScreen")?;
            let x = number::<f32>(geometry, "x")?.unwrap_or(0.0);
            let y = number::<f32>(geometry, "y")?.unwrap_or(0.0);
            let position = SavedPosition::new(x, y);
            altered |= position.is_none();
            (position, flag(geometry, "maximized")?)
        } else {
            (None, false)
        };
        if altered || child(window, "dashboardDividerPosition")?.is_some() {
            self.note(Note::Geometry);
        }

        let active_id = value(window, "activeSessionId")?;
        let active_index = number::<i32>(window, "activeTabIndex")?.unwrap_or(0);
        let mut tabs = Vec::new();
        let mut tab_titles = BTreeMap::new();
        let mut tab_groups = BTreeMap::new();
        let mut tab_terminal_effects = BTreeMap::new();
        let mut tab_terminal_appearance = BTreeMap::new();
        let mut active = None;
        if let Some(list) = child(window, "tabs")? {
            for (index, tab) in list.children().filter(|node| tag(*node, "tab")).enumerate() {
                if index >= MAX_TABS_PER_WINDOW {
                    return Err(invalid("Java project exceeds the native tab count limit"));
                }
                let id = value(tab, "sessionId")?;
                let selected = active_id.map_or_else(
                    || usize::try_from(active_index) == Ok(index),
                    |wanted| id == Some(wanted),
                );
                let (pane, appearance) = self.tab(tab)?;
                if let Some(pane) = pane {
                    if selected && active.is_none() {
                        active = Some(tabs.len());
                    }
                    if let Some(title) = text_value(tab, "tabTitle")? {
                        let normalized = normalize_tab_title(title);
                        if normalized != title {
                            self.note(Note::Appearance);
                        }
                        if !normalized.is_empty() {
                            tab_titles.insert(tabs.len(), normalized);
                        }
                    }
                    if let Some(group) = text_value(tab, "group")? {
                        let normalized = normalize_tab_group(group);
                        if normalized.as_deref() != Some(group) {
                            self.note(Note::Appearance);
                        }
                        if let Some(normalized) = normalized {
                            tab_groups.insert(tabs.len(), normalized);
                        }
                    }
                    if !pane.is_project_tool()
                        && let Some(effect) = self.terminal_effect(tab)?
                    {
                        // The Java restore path applies its tab effect before
                        // creating splits, which inherit it from their source.
                        let effects = (0..leaf_count(&pane)).map(|pane| (pane, effect)).collect();
                        tab_terminal_effects.insert(tabs.len(), effects);
                    }
                    if let Some(appearance) = appearance {
                        tab_terminal_appearance.insert(tabs.len(), appearance);
                    }
                    tabs.push(pane);
                }
            }
        }
        Ok(WindowLayout {
            tab_titles,
            tab_groups: Some(tab_groups),
            tab_primary_panes: Default::default(),
            tab_terminal_effects,
            tab_terminal_appearance,
            width: normal_width,
            height: normal_height,
            position,
            maximized,
            dashboard_visible: boolean(window, "dashboardVisible")?,
            active: active.unwrap_or(0),
            tabs,
        })
    }

    fn tab(
        &mut self,
        node: Node<'_, '_>,
    ) -> io::Result<(Option<PaneLayout>, Option<ProjectTerminalAppearance>)> {
        if let Some(kind) = value(node, "tabType")?.filter(|&kind| kind != "TERMINAL") {
            if kind == "SFTP_MANAGER" {
                return Ok((self.sftp_tab(node)?, None));
            }
            if let Some(file) = self.file_tab(node, kind)? {
                return Ok((Some(file), None));
            }
            self.note(Note::ToolTab);
            return Ok((None, None));
        }
        if has_any(node, &["group", "settings"]) {
            self.note(Note::Appearance);
        }
        if has_any(
            node,
            &[
                "currentDirectory",
                "localShellCommand",
                "scrollbackRef",
                "historyFilePath",
                "terminalHistory",
                "terminalTimestamps",
                "currentApplication",
            ],
        ) {
            self.note(Note::SessionData);
        }
        let base = routes::Tab::read(node)?;
        let (mut pane, has_focus) = if let Some(split) = child(node, "splitPaneState")? {
            self.split(split, &base, 0, &mut BTreeSet::new())?
        } else {
            (self.leaf(&base, None, true)?, true)
        };
        if !has_focus && let Some(pane) = pane.as_mut() {
            focus_first(pane);
        }
        let appearance = if pane.is_some() {
            self.tab_appearance(&base)?
        } else {
            None
        };
        Ok((pane, appearance))
    }

    fn tab_appearance(
        &mut self,
        tab: &routes::Tab<'_>,
    ) -> io::Result<Option<ProjectTerminalAppearance>> {
        let style = if tab.local {
            None
        } else {
            let Some(connection) = tab
                .connection
                .and_then(|id| self.connections.get(id))
                .copied()
            else {
                return Ok(None);
            };
            if flag(connection, "incognito")?
                || !matches!(
                    value(connection, "protocol")?,
                    None | Some("SSH_TCP" | "LOCAL_SHELL")
                )
            {
                return Ok(None);
            }
            match appearance::catalogue_style(connection) {
                Ok(style) => style,
                Err(_) => {
                    self.note(Note::Appearance);
                    None
                }
            }
        };
        Ok(Some(ProjectTerminalAppearance {
            style,
            font_zoom: tab.font,
        }))
    }

    fn file_tab(&mut self, node: Node<'_, '_>, kind: &str) -> io::Result<Option<PaneLayout>> {
        let (path, remote, tool) = match kind {
            "FILE_EDITOR" => ("editorFilePath", "editorIsRemote", LocalFileTool::Editor),
            "IMAGE_VIEWER" => {
                let zoom = number::<f32>(node, "imageZoomLevel")?.unwrap_or(1.0);
                if !zoom.is_finite() || zoom <= 0.0 {
                    return Ok(None);
                }
                let supported = zoom.clamp(LocalFileTool::MIN_ZOOM, LocalFileTool::MAX_ZOOM);
                if supported != zoom {
                    self.note(Note::Appearance);
                }
                (
                    "imageFilePath",
                    "imageIsRemote",
                    LocalFileTool::Image { zoom: supported },
                )
            }
            _ => return Ok(None),
        };
        let Some(path) = text_value(node, path)? else {
            return Ok(None);
        };
        if flag(node, remote)? {
            let tool = RemoteProjectTool::File {
                path: path.to_owned(),
                tool,
            };
            return if tool.valid() {
                self.remote_tool_tab(node, tool, false)
            } else {
                Ok(None)
            };
        }
        let path = PathBuf::from(path);
        Ok(LocalFileTool::valid_path(&path).then_some(PaneLayout::LocalFile { path, tool }))
    }

    fn sftp_tab(&mut self, node: Node<'_, '_>) -> io::Result<Option<PaneLayout>> {
        let local = value(node, "sftpLocalPath")?.map(PathBuf::from);
        let remote = value(node, "sftpRemotePath")?.filter(|path| *path != "~");
        let minutes = number::<i32>(node, "sftpAutoCloseTimeout")?.filter(|minutes| *minutes > 0);
        let tool = RemoteProjectTool::Sftp {
            local: local
                .clone()
                .filter(|path| LocalFileTool::valid_directory(path)),
            remote: remote
                .filter(|path| RemoteProjectTool::valid_remote_path(path, true))
                .map(str::to_owned),
            idle_minutes: minutes
                .and_then(|minutes| u16::try_from(minutes).ok())
                .filter(|minutes| *minutes <= RemoteProjectTool::MAX_IDLE_MINUTES),
        };
        if matches!(&tool, RemoteProjectTool::Sftp { local: supported_local, remote: supported_remote, idle_minutes }
            if local.is_some() != supported_local.is_some() || remote.is_some() != supported_remote.is_some() || minutes.is_some() != idle_minutes.is_some())
        {
            self.note(Note::SessionData);
        }
        self.remote_tool_tab(node, tool, true)
    }

    fn remote_tool_tab(
        &mut self,
        node: Node<'_, '_>,
        tool: RemoteProjectTool,
        legacy_name: bool,
    ) -> io::Result<Option<PaneLayout>> {
        let Some(id) = text_value(node, "connectionId")?.filter(|id| !id.trim().is_empty()) else {
            self.note(Note::MissingConnection);
            return Ok(None);
        };
        let mut connection = self.connections.get(id).copied();
        if connection.is_none() && legacy_name {
            for candidate in self.connections.values().copied() {
                if text_value(candidate, "name")? == Some(id) {
                    if connection.is_some() {
                        self.note(Note::MissingConnection);
                        return Ok(None);
                    }
                    connection = Some(candidate);
                }
            }
        }
        let Some(connection) = connection else {
            self.note(Note::MissingConnection);
            return Ok(None);
        };
        match routes::remote_tool(connection)? {
            Ok(target) => Ok(Some(PaneLayout::RemoteTool { target, tool })),
            Err(note) => {
                self.note(note);
                Ok(None)
            }
        }
    }

    fn terminal_effect(&mut self, node: Node<'_, '_>) -> io::Result<Option<Selection>> {
        let id = value(node, "terminalEffectPluginId")?;
        let raw_speed = value(node, "terminalEffectAnimationSpeed")?;
        let effect = id.and_then(|id| {
            Effect::ALL
                .into_iter()
                .find(|effect| effect_id(*effect) == id)
        });
        let speed = raw_speed.map_or(Some(Speed::DEFAULT), |text| {
            Speed::try_from(text.parse::<f64>().ok()?).ok()
        });
        let selection = effect
            .zip(speed)
            .map(|(effect, speed)| Selection { effect, speed });
        if selection.is_none() && (id.is_some() || raw_speed.is_some()) {
            self.note(Note::Appearance);
        }
        Ok(selection)
    }

    fn split(
        &mut self,
        node: Node<'_, '_>,
        tab: &routes::Tab<'_>,
        depth: usize,
        indices: &mut BTreeSet<usize>,
    ) -> io::Result<(Option<PaneLayout>, bool)> {
        if depth > MAX_SPLIT_DEPTH {
            return Err(invalid(
                "Java project split nesting exceeds the native limit",
            ));
        }
        if has_any(node, &["currentDirectory", "scrollbackRef"]) {
            self.note(Note::SessionData);
        }
        if let Some(index) = number::<usize>(node, "widgetIndex")? {
            if has_any(
                node,
                &["orientation", "dividerPosition", "leftChild", "rightChild"],
            ) || !indices.insert(index)
            {
                return Err(invalid("invalid or duplicate Java project split leaf"));
            }
            let pane = self.leaf(tab, value(node, "connectionId")?, index == 0)?;
            let focused = pane.is_some() && index == 0;
            return Ok((pane, focused));
        }
        let vertical = match value(node, "orientation")? {
            Some("HORIZONTAL") => true,
            Some("VERTICAL") => false,
            _ => return Err(invalid("invalid Java project split orientation")),
        };
        let ratio = number::<f32>(node, "dividerPosition")?.unwrap_or(0.5);
        if !(0.0..=1.0).contains(&ratio) {
            return Err(invalid("invalid Java project divider position"));
        }
        let (first, first_focused) =
            self.split(required_child(node, "leftChild")?, tab, depth + 1, indices)?;
        let (second, second_focused) =
            self.split(required_child(node, "rightChild")?, tab, depth + 1, indices)?;
        let pane = match (first, second) {
            (Some(first), Some(second)) => Some(PaneLayout::Split {
                vertical,
                ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (first, second) => first.or(second),
        };
        Ok((pane, first_focused || second_focused))
    }

    fn leaf(
        &mut self,
        tab: &routes::Tab<'_>,
        own: Option<&str>,
        focused: bool,
    ) -> io::Result<Option<PaneLayout>> {
        self.panes += 1;
        if self.panes > MAX_TOTAL_PANES {
            return Err(invalid("Java project exceeds the native pane count limit"));
        }
        let own = own.filter(|id| Some(*id) != tab.connection);
        if own.is_none() && tab.local {
            return Ok(Some(tab.local_pane(focused)));
        }
        let Some(connection) = own
            .or(tab.connection)
            .and_then(|id| self.connections.get(id))
            .copied()
        else {
            self.note(Note::MissingConnection);
            return Ok(None);
        };
        match routes::convert(tab, connection, focused)? {
            Ok((mut pane, appearance_omitted)) => {
                if let PaneLayout::Terminal { initial_size, .. }
                | PaneLayout::Ssh { initial_size, .. } = &mut pane
                {
                    match routes::catalogue_initial_size(connection) {
                        Ok(size) => *initial_size = size,
                        Err(_) => self.note(Note::InitialSize),
                    }
                }
                if appearance_omitted {
                    self.note(Note::Appearance);
                }
                Ok(Some(pane))
            }
            Err(note) => {
                self.note(note);
                Ok(None)
            }
        }
    }
}

fn leaf_count(pane: &PaneLayout) -> usize {
    match pane {
        PaneLayout::Split { first, second, .. } => leaf_count(first) + leaf_count(second),
        _ => 1,
    }
}

fn effect_id(effect: Effect) -> &'static str {
    match effect {
        Effect::AmberCrt90 => "amber-crt-90",
        Effect::CommodoreHeritage => "commodore-blue",
        Effect::DeepSpaceRadar => "deep-space-radar",
        Effect::DigitalRain => "digital-rain",
        Effect::HologramHud => "hologram-hud",
        Effect::Mother => "mother",
        Effect::NeonCity => "neon-city",
        Effect::Poltergeist => "poltergeist",
        Effect::SynthwaveHorizon => "synthwave-horizon",
        Effect::TypewriterNoir => "typewriter-noir",
        Effect::Vhs1987 => "vhs-1987",
    }
}

fn focus_first(pane: &mut PaneLayout) {
    match pane {
        PaneLayout::Split { first, .. } => focus_first(first),
        PaneLayout::Terminal { focused, .. }
        | PaneLayout::Ssh { focused, .. }
        | PaneLayout::Serial { focused, .. }
        | PaneLayout::Ibm { focused, .. } => *focused = true,
        PaneLayout::LocalFile { .. } | PaneLayout::RemoteTool { .. } => {}
    }
}

fn tag(node: Node<'_, '_>, name: &str) -> bool {
    node.is_element() && node.tag_name().namespace().is_none() && node.tag_name().name() == name
}

fn has_any(node: Node<'_, '_>, names: &[&str]) -> bool {
    node.children()
        .any(|child| names.iter().any(|name| tag(child, name)))
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> io::Result<Option<Node<'a, 'input>>> {
    let mut children = node.children().filter(|node| tag(*node, name));
    let value = children.next();
    if children.next().is_some() {
        return Err(invalid(format!("duplicate Java XML element {name}")));
    }
    Ok(value)
}

fn required_child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> io::Result<Node<'a, 'input>> {
    child(node, name)?.ok_or_else(|| invalid(format!("missing Java XML element {name}")))
}

fn value<'a>(node: Node<'a, '_>, name: &str) -> io::Result<Option<&'a str>> {
    Ok(text_value(node, name)?
        .map(str::trim)
        .filter(|text| !text.is_empty()))
}

fn text_value<'a>(node: Node<'a, '_>, name: &str) -> io::Result<Option<&'a str>> {
    let Some(field) = child(node, name)? else {
        return Ok(None);
    };
    if field.children().any(|child| child.is_element()) {
        return Err(invalid(format!("expected text in Java XML element {name}")));
    }
    let mut text = field.children().filter(Node::is_text);
    let value = text.next().and_then(|node| node.text());
    if text.next().is_some() {
        return Err(invalid(format!(
            "ambiguous text in Java XML element {name}"
        )));
    }
    Ok(value)
}

fn number<T: std::str::FromStr>(node: Node<'_, '_>, name: &str) -> io::Result<Option<T>> {
    value(node, name)?
        .map(|text| {
            text.parse()
                .map_err(|_| invalid(format!("invalid number in Java XML element {name}")))
        })
        .transpose()
}

fn boolean(node: Node<'_, '_>, name: &str) -> io::Result<Option<bool>> {
    value(node, name)?
        .map(|text| match text {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(invalid(format!(
                "invalid boolean in Java XML element {name}"
            ))),
        })
        .transpose()
}

fn flag(node: Node<'_, '_>, name: &str) -> io::Result<bool> {
    Ok(boolean(node, name)?.unwrap_or(false))
}

#[cfg(test)]
mod tests;
