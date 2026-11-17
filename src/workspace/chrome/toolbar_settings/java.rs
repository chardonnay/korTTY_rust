//! Reads Java 2.26 toolbar references without guessing their native destinations.

use std::io;

use roxmltree::Node;

use crate::connection_store::Store;
use crate::workspace::key_bindings;

use super::{
    Action, DEFAULT_ICON, Group, Item, JavaTarget, Layout, MAX_ICON, MIN_ICON, Section,
    decode_profile, encode_profile, settings_window,
};

pub(crate) const FILE: &str = "toolbar.json";

pub(crate) struct Source(Layout);

pub(crate) struct Staged {
    pub(crate) contents: Vec<u8>,
    pub(crate) bound: bool,
    pub(crate) unavailable: usize,
}

impl Source {
    pub(crate) fn count(&self) -> usize {
        self.0.item_count()
    }

    pub(crate) fn uses_profiles(&self) -> bool {
        self.0.items(Group::Connections).iter().any(|item| {
            matches!(
                item,
                Item::Unavailable(JavaTarget::Connection(_) | JavaTarget::Folder(_))
            )
        })
    }

    pub(crate) fn stage(
        mut self,
        resolve_profile: impl Fn(&str) -> Option<u64>,
        resolve_group: impl Fn(&str) -> Option<String>,
    ) -> io::Result<Staged> {
        let mut unavailable = 0;
        let mut bound = false;
        for group in Group::ALL {
            for item in &mut self.0.section_mut(group).items {
                let Item::Unavailable(target) = item else {
                    continue;
                };
                let replacement = match target {
                    JavaTarget::Action(_) => None,
                    JavaTarget::Connection(id) => resolve_profile(id).map(Item::Connection),
                    JavaTarget::Folder(path) => resolve_group(path).map(Item::Folder),
                };
                if let Some(replacement) = replacement.filter(|item| item.valid(group)) {
                    *item = replacement;
                    bound = true;
                } else {
                    unavailable += 1;
                }
            }
        }
        Ok(Staged {
            contents: encode_profile(self.0)?,
            bound,
            unavailable,
        })
    }
}

pub(crate) fn icon_size(text: &str) -> Option<u16> {
    text.parse()
        .ok()
        .filter(|size| (MIN_ICON..=MAX_ICON).contains(size))
}

pub(crate) fn read(
    root: Node<'_, '_>,
    icon_size: Option<u16>,
    omitted: &mut usize,
) -> Option<Source> {
    let mut elements = root
        .children()
        .filter(|node| node.has_tag_name("iconToolbar") && node.tag_name().namespace().is_none());
    let Some(element) = elements.next() else {
        return icon_size.map(|icon_size| {
            Source(Layout {
                icon_size,
                ..Layout::default()
            })
        });
    };
    if elements.next().is_some() {
        *omitted += 1;
        return None;
    }
    let flag = |name, fallback, omitted: &mut usize| match element.attribute(name).map(str::trim) {
        Some("true" | "1") => true,
        Some("false" | "0") => false,
        None => fallback,
        _ => {
            *omitted += 1;
            fallback
        }
    };
    let actions_visible = flag("visible", true, omitted);
    let mut layout = Layout {
        icon_size: icon_size.unwrap_or(DEFAULT_ICON),
        actions_visible,
        terminal: Section::new(Vec::new()),
        tools: Section::new(Vec::new()),
        settings: Section::new(Vec::new()),
        connections: Section {
            visible: flag("connectionsVisible", actions_visible, omitted),
            items: Vec::new(),
        },
    };
    for entry in element.children().filter(Node::is_element) {
        if read_entry(entry)
            .and_then(|(group, item)| layout.add(group, item))
            .is_none()
        {
            *omitted += 1;
        }
    }
    if !flag("logfileManagerMigrated", false, omitted) {
        let items = layout.items(Group::Tools);
        let index = items
            .iter()
            .position(|item| *item == Item::Action(Action::SessionJournals))
            .map_or(items.len(), |index| index + 1);
        add_default(
            &mut layout,
            Group::Tools,
            index,
            Action::LogfileManager,
            omitted,
        );
    }
    if !flag("connectionManagerMigrated", false, omitted) {
        add_default(
            &mut layout,
            Group::Tools,
            0,
            Action::ManageConnections,
            omitted,
        );
    }
    if !flag("windowsShellsMigrated", false, omitted) && cfg!(windows) {
        let items = layout.items(Group::Terminal);
        let mut index = items
            .iter()
            .position(|item| *item == Item::Action(Action::LocalShell))
            .map_or(items.len(), |index| index + 1);
        for action in [Action::PowerShell, Action::CommandPrompt] {
            index += usize::from(add_default(
                &mut layout,
                Group::Terminal,
                index,
                action,
                omitted,
            ));
        }
    }
    if encode_profile(layout.clone()).is_err() {
        *omitted += layout.item_count().max(1);
        return None;
    }
    Some(Source(layout))
}

fn add_default(
    layout: &mut Layout,
    group: Group,
    index: usize,
    action: Action,
    omitted: &mut usize,
) -> bool {
    let item = Item::Action(action);
    if Group::ACTIONS
        .into_iter()
        .any(|group| layout.items(group).contains(&item))
    {
        return false;
    }
    let inserted = layout.insert(group, index, item).is_some();
    *omitted += usize::from(!inserted);
    inserted
}

fn read_entry(node: Node<'_, '_>) -> Option<(Group, Item)> {
    if !node.has_tag_name("entry")
        || node.tag_name().namespace().is_some()
        || node.attribute("id")?.trim().is_empty()
    {
        return None;
    }
    let group = match node.attribute("section")? {
        "TERMINAL" => Group::Terminal,
        "TOOLS" => Group::Tools,
        "SETTINGS" => Group::Settings,
        "CONNECTIONS" => Group::Connections,
        _ => return None,
    };
    let target = || node.attribute("target");
    let item = match node.attribute("kind")? {
        "SEPARATOR" => Item::Separator,
        "CONNECTION" => Item::Unavailable(JavaTarget::Connection(target()?.to_owned())),
        "GROUP" => Item::Unavailable(JavaTarget::Folder(target()?.to_owned())),
        "ACTION" => read_action(target()?),
        _ => return None,
    };
    Some((group, item))
}

fn read_action(id: &str) -> Item {
    use Action::*;
    use settings_window::Section as SettingsSection;

    Item::Action(match id {
        "toolbar.localShell" => LocalShell,
        "toolbar.powershell" => PowerShell,
        "toolbar.cmd" => CommandPrompt,
        "toolbar.settings.font" => FontSettings,
        "toolbar.settings.colors" => ColorSettings,
        "toolbar.settings.themes" => ThemeSettings,
        "toolbar.settings.appearance" => AppearanceSettings,
        "toolbar.settings.terminal" => TerminalSettings,
        "toolbar.settings.keyboard" => KeyboardSettings,
        "toolbar.settings.logging" => LoggingSettings,
        "toolbar.settings.window" => WindowSettings,
        "toolbar.settings.messaging" => return Item::Settings(SettingsSection::Messaging),
        "toolbar.settings.video" => return Item::Settings(SettingsSection::Video),
        "toolbar.settings.backup" => return Item::Settings(SettingsSection::Backup),
        "toolbar.settings.export" => return Item::Settings(SettingsSection::Export),
        "toolbar.settings.updates" => return Item::Settings(SettingsSection::Updates),
        "toolbar.settings.sftp" => return Item::Settings(SettingsSection::Sftp),
        "toolbar.settings.editor" => return Item::Settings(SettingsSection::Editor),
        "toolbar.settings.snippetEditor" => return Item::Settings(SettingsSection::SnippetEditor),
        _ => {
            let Some(action) = key_bindings::java::action(id) else {
                return Item::Unavailable(JavaTarget::Action(id.to_owned()));
            };
            action
        }
    })
}

pub(crate) fn validate_bindings(bytes: &[u8], catalogue: Option<&Store>) -> io::Result<()> {
    let profile = decode_profile(bytes)?;
    for item in profile.layout.items(Group::Connections) {
        let valid = match item {
            Item::Connection(id) => catalogue.is_some_and(|store| {
                store.get(*id).is_some()
                    || store.local(*id).is_some()
                    || store.serial(*id).is_some()
            }),
            Item::Folder(path) => {
                catalogue.is_some_and(|store| store.direct_group_ids(path).next().is_some())
            }
            _ => true,
        };
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Java toolbar buttons require their reviewed connection catalogue.",
            ));
        }
    }
    Ok(())
}
