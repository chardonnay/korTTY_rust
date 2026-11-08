//! Writes the Java JAXB project model and references a separately chosen Java catalogue.
use std::collections::BTreeMap;
use std::fmt::{Display, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use kortty_terminal::TerminalProfile;

use crate::profile::projects::Name;
use crate::profile::{
    PaneLayout, WindowLayout, WorkspaceLayout, invalid, local_profile, save_new_atomic,
};
use crate::session_security::IsolationLevel;
use crate::ssh_dialog::unsafe_text;
use crate::terminal_effects::Selection;

use super::{
    CONNECTION_BYTES, PROJECT_BYTES, connection_nodes, document, effect_id, leaf_count, read_xml,
    routes,
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
    bindings: BTreeMap<String, PaneLayout>,
}

impl Export {
    pub(crate) fn publish(&self) -> io::Result<()> {
        if let Some(path) = &self.catalogue {
            let current = Catalogue::read(path)?;
            for (id, pane) in &self.bindings {
                if current.unique(pane) != Ok(id.as_str()) {
                    return Err(invalid(
                        "the Java connection mappings changed after the export preview",
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
        && layout
            .windows
            .iter()
            .flat_map(|window| &window.tabs)
            .any(needs_catalogue)
    {
        return Err(invalid(
            "choose the Java connections.xml that matches this project's SSH and serial panes",
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
    let mut bindings = BTreeMap::new();
    let mut notes = Vec::new();
    let mut tabs = 0;
    for (index, window) in layout.windows.iter().enumerate() {
        tabs += write_window(
            &mut output,
            index + 1,
            window,
            &available,
            &mut bindings,
            &mut notes,
        )?;
    }
    output.close("windows");
    output.close("project");
    if tabs == 0 {
        let detail = notes.first().map_or("", String::as_str);
        return Err(invalid(format!(
            "this project has no exportable Java tabs. {detail}"
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
struct Catalogue(BTreeMap<String, PaneLayout>);

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
                matches.insert(id.to_owned(), pane);
            }
        }
        Ok(Self(matches))
    }

    fn unique(&self, source: &PaneLayout) -> Result<&str, &'static str> {
        let mut candidates = self
            .0
            .iter()
            .filter(|(_, candidate)| equivalent(source, candidate));
        let Some((id, _)) = candidates.next() else {
            return Err("no eligible Java connection matches the pane exactly");
        };
        if candidates.next().is_some() {
            return Err("multiple Java connections match the pane; its ID is ambiguous");
        }
        Ok(id)
    }
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

fn local_identity(pane: &PaneLayout) -> Option<(TerminalProfile, IsolationLevel)> {
    let PaneLayout::Terminal {
        vt100,
        term,
        security,
        ..
    } = pane
    else {
        return None;
    };
    let isolation = security
        .as_ref()
        .map_or(IsolationLevel::None, |settings| settings.isolation);
    Some((local_profile(term.as_deref(), *vt100)?, isolation))
}

fn needs_catalogue(pane: &PaneLayout) -> bool {
    match pane {
        PaneLayout::Ssh { .. } | PaneLayout::Serial { .. } => true,
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
        } if !settings.shell.is_default() => {
            Err("a selected Windows shell cannot be encoded in a portable Java project")
        }
        PaneLayout::Terminal { .. }
            if local_identity(pane) == Some((TerminalProfile::Xterm, IsolationLevel::None))
                && can_default =>
        {
            Ok(Link::DefaultShell)
        }
        PaneLayout::Terminal { .. }
            if local_identity(pane)
                .is_some_and(|(_, isolation)| isolation == IsolationLevel::Sandbox) =>
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
    bindings: &mut BTreeMap<String, PaneLayout>,
    notes: &mut Vec<String>,
) -> io::Result<usize> {
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
        match Tab::write(output.depth, pane, catalogue, &id, (title, group), effect) {
            Ok(draft) => {
                output.text.push_str(&draft.xml.text);
                bindings.extend(draft.bindings);
                kept.push((tab, id));
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
            Err(reason) => notes.push(format!(
                "Window {index}, tab {} omitted ({reason}).",
                tab + 1
            )),
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
    Ok(kept.len())
}

struct Tab {
    xml: Xml,
    bindings: BTreeMap<String, PaneLayout>,
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
    ) -> Result<Self, &'static str> {
        let first = first_leaf(pane);
        let primary = link(first, catalogue, true)?;
        let (font, focused) = presentation(first);
        let mut tab = Self {
            xml: Xml {
                text: String::new(),
                depth,
            },
            bindings: BTreeMap::new(),
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
                tab.xml.field("connectionId", connection);
                tab.bindings.insert(connection.into(), first.clone());
            }
        }
        tab.xml.field("fontSizeOverride", font);
        if let Some(selection) = effect {
            tab.xml
                .field("terminalEffectPluginId", effect_id(selection.effect));
            if selection.speed != 1 {
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
                if connection != primary
                    && let Link::Catalogue(id) = connection
                {
                    self.xml.field("connectionId", id);
                    self.bindings.insert(id.into(), leaf.clone());
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
        PaneLayout::Split { .. } => unreachable!("presentation is taken from a leaf"),
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
