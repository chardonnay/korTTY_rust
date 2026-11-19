use serde_json::{Value, json};
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::connection_store::import::Preview as ConnectionPreview;
use crate::workspace::backup_data::java_toolbar::FILE;

const SETTINGS: &[u8] = br#"<globalSettings>
    <toolbarIconSize>41</toolbarIconSize>
    <iconToolbar visible="false" connectionsVisible="true" logfileManagerMigrated="true"
                 connectionManagerMigrated="true" windowsShellsMigrated="true">
        <entry id="git-bash" kind="ACTION" section="TERMINAL" target="toolbar.gitBash"/>
        <entry id="1" kind="ACTION" section="TOOLS" target="menu.tools.aiManager"/>
        <entry id="2" kind="SEPARATOR" section="TOOLS"/>
        <entry id="3" kind="ACTION" section="SETTINGS" target="toolbar.settings.video"/>
        <entry id="4" kind="ACTION" section="SETTINGS" target="toolbar.settings.language"/>
        <entry id="5" kind="CONNECTION" section="CONNECTIONS" target="ssh-profile"/>
        <entry id="6" kind="CONNECTION" section="CONNECTIONS" target="local-profile"/>
        <entry id="7" kind="CONNECTION" section="CONNECTIONS" target="serial-profile"/>
        <entry id="8" kind="GROUP" section="CONNECTIONS" target="Ops"/>
        <entry id="9" kind="GROUP" section="CONNECTIONS" target="Occupied"/>
        <entry id="10" kind="GROUP" section="CONNECTIONS" target="Aliases"/>
        <entry id="11" kind="CONNECTION" section="CONNECTIONS" target="1"/>
        <entry id="12" kind="CONNECTION" section="CONNECTIONS" target="placeholder"/>
        <entry id="13" kind="CONNECTION" section="CONNECTIONS" target="duplicate"/>
    </iconToolbar>
</globalSettings>"#;

const JAVA_CONNECTIONS: &[u8] = br#"<connections>
    <connection id="ssh-profile"><name>Imported SSH</name><host>ssh.example</host><username>user</username><group>Ops</group></connection>
    <connection id="local-profile"><name>Imported shell</name><protocol>LOCAL_SHELL</protocol><group>Ops</group></connection>
    <connection id="serial-profile"><name>Imported serial</name><protocol>SERIAL</protocol><serial><portName>COM19</portName></serial><group>Ops</group></connection>
    <connection id="child"><name>Child</name><host>child.example</host><username>user</username><group>Ops/Child</group></connection>
    <connection id="prefix"><name>Prefix</name><host>prefix.example</host><username>user</username><group>Ops extra</group></connection>
    <connection id="occupied"><name>New group member</name><host>another.example</host><username>user</username><group>Occupied</group></connection>
    <connection id="exact-alias"><name>Exact folder</name><host>exact.example</host><username>user</username><group>Aliases</group></connection>
    <connection id="alias"><name>Alias folder</name><host>alias.example</host><username>user</username><group> Aliases / </group></connection>
    <connection id="placeholder"><name>(Ordner: Ops)</name><host>placeholder</host><group>Ops</group></connection>
    <connection id="duplicate"><name>First duplicate</name><host>first.example</host><username>user</username></connection>
    <connection id="duplicate"><name>Second duplicate</name><host>second.example</host><username>user</username></connection>
</connections>"#;

const NATIVE_CONNECTIONS: &[u8] = br#"<connections>
    <connection><name>Native connection</name><host>native.example</host><username>user</username><group>Occupied</group></connection>
</connections>"#;

fn inspect_source(
    parent: &Path,
    settings: &[u8],
    connections: Option<&[u8]>,
) -> super::super::super::Preview {
    let archive_path = parent.join("toolbar.zip");
    let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
    let options = SimpleFileOptions::default()
        .with_aes_encryption_bytes(AesMode::Aes256, b"toolbar-password");
    for (name, contents) in [
        ("global-settings.xml", Some(settings)),
        ("connections.xml", connections),
    ] {
        if let Some(contents) = contents {
            archive.start_file(name, options).unwrap();
            archive.write_all(contents).unwrap();
        }
    }
    archive.finish().unwrap();
    super::super::super::inspect_java_password(
        &archive_path,
        &Secret::new("toolbar-password".into()),
    )
    .unwrap()
}

fn native_connections() -> Vec<u8> {
    ConnectionPreview::java_backup(NATIVE_CONNECTIONS)
        .unwrap()
        .portable_java_backup()
        .unwrap()
        .0
}

fn queue_at(root: &Path, preview: super::super::super::Preview) -> QueuedRestore {
    super::super::super::queue_restore_at(root, preview, RestoreMode::JavaAdditive).unwrap()
}

fn read_layout(root: &Path) -> Value {
    serde_json::from_slice::<Value>(&fs::read(root.join(FILE)).unwrap()).unwrap()["layout"].clone()
}

#[test]
fn java_toolbar_maps_exact_new_profiles_and_preserves_unavailable_buttons() {
    let source = Scratch::new();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    directory
        .publish_copy(CONNECTIONS, &native_connections())
        .unwrap();
    let existing = Store::open(local.0.join(CONNECTIONS)).unwrap();
    assert_eq!(existing.connections()[0].id, 1);

    let preview = inspect_source(&source.0, SETTINGS, Some(JAVA_CONNECTIONS));
    assert!(
        preview
            .java_review()
            .unwrap()
            .1
            .iter()
            .any(|entry| entry.contains("Java toolbar"))
    );
    let queued = queue_at(&local.0, preview);
    assert!(
        queued
            .scheduler_notes
            .iter()
            .any(|note| note.contains("6 Java toolbar buttons"))
    );
    assert!(pending_java_cancellation(&local.0).unwrap().is_some());
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    let layout = read_layout(&local.0);
    assert_eq!(layout["icon_size"], 41);
    assert_eq!(layout["actions_visible"], false);
    assert_eq!(layout["connections"]["visible"], true);
    assert_eq!(layout["terminal"]["items"], json!([{"action": "git-bash"}]));
    assert_eq!(
        layout["tools"]["items"],
        json!([{"action": "ai-manager"}, "separator"])
    );
    assert_eq!(
        layout["settings"]["items"],
        json!([
            {"settings": "video"},
            {"unavailable": {"action": "toolbar.settings.language"}},
        ])
    );
    let store = Store::open(local.0.join(CONNECTIONS)).unwrap();
    let ssh_id = store
        .connections()
        .iter()
        .find(|profile| profile.name == "Imported SSH")
        .unwrap()
        .id;
    let shell_id = store.local_profiles()[0].id;
    let serial_id = store.serial_profiles()[0].id;
    assert_eq!(
        store.direct_group_ids("Ops").collect::<Vec<_>>(),
        [ssh_id, shell_id, serial_id],
    );
    assert_eq!(
        layout["connections"]["items"],
        json!([
            {"connection": ssh_id},
            {"connection": shell_id},
            {"connection": serial_id},
            {"folder": "Ops"},
            {"unavailable": {"folder": "Occupied"}},
            {"unavailable": {"folder": "Aliases"}},
            {"unavailable": {"connection": "1"}},
            {"unavailable": {"connection": "placeholder"}},
            {"unavailable": {"connection": "duplicate"}},
        ])
    );
    assert!(!directory.has_entry("vault").unwrap());
    assert!(!directory.has_entry("ssh").unwrap());

    let bytes = fs::read(local.0.join(FILE)).unwrap();
    let second = queue_at(
        &local.0,
        inspect_source(&source.0, SETTINGS, Some(JAVA_CONNECTIONS)),
    );
    assert!(
        second
            .scheduler_notes
            .iter()
            .any(|note| note.contains("existing toolbar"))
    );
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(local.0.join(FILE)).unwrap(), bytes);
}

#[test]
fn java_toolbar_legacy_defaults_and_intentionally_empty_layouts_remain_distinct() {
    let source = Scratch::new();
    let legacy = Scratch::new();
    let old_settings = br#"<globalSettings>
        <toolbarIconSize>90</toolbarIconSize>
        <iconToolbar visible="0">
            <entry id="1" kind="ACTION" section="TERMINAL" target="toolbar.localShell"/>
            <entry id="2" kind="SEPARATOR" section="TERMINAL"/>
            <entry id="3" kind="ACTION" section="TOOLS" target="menu.tools.sessionJournals"/>
            <entry id="4" kind="ACTION" section="TOOLS" target="menu.tools.aiManager"/>
        </iconToolbar>
    </globalSettings>"#;
    queue_at(&legacy.0, inspect_source(&source.0, old_settings, None));
    drop(ProfileLease::acquire(&legacy.0, || false).unwrap());
    let layout = read_layout(&legacy.0);
    assert_eq!(layout["icon_size"], 24);
    assert_eq!(layout["actions_visible"], false);
    assert_eq!(layout["connections"]["visible"], false);
    let mut terminal = vec![json!({"action": "local-shell"})];
    if cfg!(windows) {
        terminal.extend([
            json!({"action": "power-shell"}),
            json!({"action": "command-prompt"}),
        ]);
    }
    terminal.push(json!("separator"));
    assert_eq!(layout["terminal"]["items"], json!(terminal));
    assert_eq!(
        layout["tools"]["items"],
        json!([
            {"action": "manage-connections"},
            {"action": "session-journals"},
            {"action": "logfile-manager"},
            {"action": "ai-manager"},
        ])
    );

    let empty = Scratch::new();
    let settings = br#"<globalSettings><toolbarIconSize>64</toolbarIconSize>
        <iconToolbar logfileManagerMigrated="true" connectionManagerMigrated="true"
            windowsShellsMigrated="true"/>
    </globalSettings>"#;
    queue_at(&empty.0, inspect_source(&source.0, settings, None));
    drop(ProfileLease::acquire(&empty.0, || false).unwrap());
    let layout = read_layout(&empty.0);
    assert_eq!(layout["icon_size"], 64);
    for group in ["terminal", "tools", "settings", "connections"] {
        assert_eq!(layout[group]["items"], json!([]));
    }
}

#[test]
fn java_toolbar_can_bind_an_unchanged_catalogue_of_unique_existing_profiles() {
    let source = Scratch::new();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let existing = br#"<connections>
        <connection><name>Saved SSH</name><host>saved.example</host><username>user</username><group>Ops</group></connection>
        <connection><name>Saved shell</name><protocol>LOCAL_SHELL</protocol><group>Ops</group></connection>
        <connection><name>Saved serial</name><protocol>SERIAL</protocol><serial><portName>COM19</portName></serial><group>Ops</group></connection>
        <connection><name>Compare route</name><host>direct.example</host><username>user</username></connection>
        <connection><name>Ambiguous alias</name><host>alias.example</host><username>user</username></connection>
    </connections>"#;
    let mut current = ConnectionPreview::java_backup(existing)
        .unwrap()
        .portable_java_backup()
        .unwrap()
        .0;
    current.extend_from_slice(b"\n\n");
    directory.publish_copy(CONNECTIONS, &current).unwrap();
    let store = Store::open(local.0.join(CONNECTIONS)).unwrap();
    let ids = store.direct_group_ids("Ops").collect::<Vec<_>>();
    assert_eq!(ids.len(), 3);
    let archive = br#"<connections>
        <connection id="ssh"><name>Saved SSH</name><host>saved.example</host><username>user</username><group>Ops</group></connection>
        <connection id="local"><name>Saved shell</name><protocol>LOCAL_SHELL</protocol><group>Ops</group></connection>
        <connection id="serial"><name>Saved serial</name><protocol>SERIAL</protocol><serial><portName>COM19</portName></serial><group>Ops</group></connection>
        <connection id="different-route"><name>Compare route</name><host>direct.example</host><username>user</username><jumpServer><enabled>true</enabled><host>jump.example</host><username>user</username></jumpServer></connection>
        <connection id="alias-a"><name>Ambiguous alias</name><host>alias.example</host><username>user</username></connection>
        <connection id="alias-b"><name>Ambiguous alias</name><host>alias.example</host><username>user</username></connection>
    </connections>"#;
    let settings = br#"<globalSettings><iconToolbar>
        <entry id="1" kind="CONNECTION" section="CONNECTIONS" target="ssh"/>
        <entry id="2" kind="CONNECTION" section="CONNECTIONS" target="local"/>
        <entry id="3" kind="CONNECTION" section="CONNECTIONS" target="serial"/>
        <entry id="4" kind="GROUP" section="CONNECTIONS" target="Ops"/>
        <entry id="5" kind="CONNECTION" section="CONNECTIONS" target="different-route"/>
        <entry id="6" kind="CONNECTION" section="CONNECTIONS" target="alias-a"/>
        <entry id="7" kind="CONNECTION" section="CONNECTIONS" target="alias-b"/>
    </iconToolbar></globalSettings>"#;
    let inspect = || inspect_source(&source.0, settings, Some(archive));
    queue_at(&local.0, inspect());
    let changed = [current.as_slice(), b" "].concat();
    directory.replace_file(CONNECTIONS, &changed).unwrap();
    assert!(
        ProfileLease::acquire(&local.0, || false)
            .is_err_and(|error| error.to_string().contains("connections changed"))
    );
    discard_pending_java(&pending_java_cancellation(&local.0).unwrap().unwrap()).unwrap();
    directory.replace_file(CONNECTIONS, &current).unwrap();
    queue_at(&local.0, inspect());
    drop(ProfileLease::acquire(&local.0, || false).unwrap());
    assert_eq!(fs::read(local.0.join(CONNECTIONS)).unwrap(), current);
    assert_eq!(
        read_layout(&local.0)["connections"]["items"],
        json!([
            {"connection": ids[0]}, {"connection": ids[1]}, {"connection": ids[2]},
            {"folder": "Ops"}, {"unavailable": {"connection": "different-route"}},
            {"unavailable": {"connection": "alias-a"}}, {"unavailable": {"connection": "alias-b"}},
        ])
    );
}

#[test]
fn java_toolbar_requires_the_reviewed_layout_and_catalogue_on_restart() {
    let source = Scratch::new();
    let local = Scratch::new();
    let directory = Directory::open(&local.0, false).unwrap();
    let inspect = || inspect_source(&source.0, SETTINGS, Some(JAVA_CONNECTIONS));
    queue_at(&local.0, inspect());

    let mut pending = fs::read(local.0.join(PENDING)).unwrap();
    assert_eq!(pending[PENDING_MAGIC_WITH_MODE.len()], JAVA_TOOLBAR);
    let start = PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN;
    let plan_size = u32::from_le_bytes(pending[start..start + 4].try_into().unwrap()) as usize;
    let payload = start + 4 + plan_size;
    let mut snapshot = archive::decode_snapshot(&pending[payload..]).unwrap();
    snapshot.get_mut(FILE).unwrap().push(b' ');
    pending.truncate(payload);
    pending.extend_from_slice(&archive::encode_snapshot(&snapshot).unwrap());
    directory.replace_file(PENDING, &pending).unwrap();
    let result = ProfileLease::acquire(&local.0, || false);
    assert!(result.is_err_and(|error| {
        error
            .to_string()
            .contains("reviewed Java toolbar layout changed")
    }));
    assert!(!directory.has_entry(CONNECTIONS).unwrap());
    assert!(!directory.has_entry(FILE).unwrap());
    discard_pending_java(&pending_java_cancellation(&local.0).unwrap().unwrap()).unwrap();

    queue_at(&local.0, inspect());
    directory
        .publish_copy(CONNECTIONS, &native_connections())
        .unwrap();
    let result = ProfileLease::acquire(&local.0, || false);
    assert!(result.is_err_and(|error| error.to_string().contains("connections changed")));
    assert!(!directory.has_entry(FILE).unwrap());
    assert_eq!(
        fs::read(local.0.join(CONNECTIONS)).unwrap(),
        native_connections()
    );
    discard_pending_java(&pending_java_cancellation(&local.0).unwrap().unwrap()).unwrap();
}
