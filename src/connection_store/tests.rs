use super::*;

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("kortty-profiles-{suffix}"));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn store(&self) -> Store {
        Store::open(self.0.join("connections.json")).unwrap()
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn connection() -> Connection {
    Connection {
        id: 0,
        name: "Production test".into(),
        tag: "test".into(),
        login: SavedLogin {
            endpoint: LoginTarget {
                host: "server.test.".into(),
                port: 2222,
                username: "operator".into(),
            },
            auth: Auth::VaultPassword("password-reference".into()),
            keyboard_interactive: false,
        },
        jump: None,
        emulation: TerminalProfile::Xterm.term_name().into(),
        incognito: false,
        isolation: IsolationLevel::None,
        startup: Default::default(),
        logging: Default::default(),
        terminal_effect: None,
        command_timestamps: None,
        tab_color: None,
        paste_warning: None,
        window_geometry: None,
        terminal: Default::default(),
        tunnels: vec![TunnelConfig {
            enabled: true,
            description: "Local service".into(),
            rule: TunnelRule {
                bind: "127.0.0.1:15432".parse().unwrap(),
                target: TunnelTarget::Local {
                    host: "db.test".into(),
                    port: 5432,
                },
            },
        }],
    }
}

#[test]
fn saved_window_geometry_round_trips_private_store_and_validates_bounds() {
    let directory = TestDir::new();
    let geometry = WindowGeometry {
        width: 907,
        maximized: true,
        ..WindowGeometry::default()
    };
    let mut profile = connection();
    profile.window_geometry = Some(geometry);
    let id = directory.store().save(profile).unwrap();
    let store = directory.store();
    let mut profile = store.get(id).unwrap().clone();
    assert_eq!(profile.window_geometry, Some(geometry));
    profile.window_geometry.as_mut().unwrap().height = 0;
    assert!(profile.validate().is_err());
    assert_eq!(
        directory.store().get(id).unwrap().window_geometry,
        Some(geometry)
    );
}

#[test]
fn serial_profiles_share_store_ids_and_merge_usage_without_touching_ssh() {
    use crate::session::{SerialFlowControl, SerialLineEnding, SerialSettings};
    use kortty_terminal::TextEncoding;

    let directory = TestDir::new();
    let mut store = directory.store();
    let ssh = store.save(connection()).unwrap();
    let connection = SerialConnection {
        // A profile copied from a different OS must not prevent the library from opening.
        device: "COM7".into(),
        settings: SerialSettings {
            baud_rate: 115200,
            data_bits: 7,
            parity: SerialParity::Even,
            stop_bits: SerialStopBits::Two,
            flow_control: SerialFlowControl::XonXoff,
            assert_dtr: false,
            assert_rts: false,
        },
        line_ending: SerialLineEnding::CrLf,
        local_echo: true,
        encoding: Some(TextEncoding::Latin9),
    };
    let serial = store
        .save_serial(SerialProfile::new(
            "",
            connection.clone(),
            TerminalProfile::Vt220,
        ))
        .unwrap();
    assert_ne!(ssh, serial);
    assert!(store.get(serial).is_none());
    let mut tracker = directory.store();
    let saved = tracker.serial(serial).unwrap().clone();
    assert!(saved.matches(&connection, TerminalProfile::Vt220));
    assert_eq!(saved.display_name(), "COM7 @ 115200");
    assert_eq!(saved.line(), "115200 7E2");
    assert_eq!(tracker.search_serial("com*").next(), Some(&saved));
    tracker.record_serial_used_at(&saved, 12).unwrap();

    let mut updated = saved.clone();
    updated.name = "Console".into();
    updated.connection.settings.baud_rate = 57600;
    store.save_serial(updated.clone()).unwrap();
    assert_eq!(store.recent_serial_since(0).next(), Some((&updated, 12)));
    tracker.record_serial_used_at(&saved, 20).unwrap();
    assert_eq!(tracker.recent_serial_since(13).count(), 0);

    tracker.delete_connections(&[serial]).unwrap();
    let reopened = directory.store();
    assert!(reopened.serial(serial).is_none());
    assert!(reopened.recent_ids().next().is_none());
    assert!(reopened.get(ssh).is_some());
}

#[test]
fn batch_edits_commit_together_and_reject_stale_members() {
    let profile = TestDir::new();
    let mut store = profile.store();
    let first = store.save(connection()).unwrap();
    let mut second = connection();
    second.name = "Second".into();
    let second = store.save(second).unwrap();
    let mut third = connection();
    third.name = "Untouched".into();
    let third = store.save(third).unwrap();
    let selected = [first, second];
    assert!(store.toggle_connection_favorites(&selected, &[]).unwrap());
    assert!(selected.iter().all(|id| store.is_favorite(*id)));
    assert!(!store.toggle_connection_favorites(&selected, &[]).unwrap());
    store.set_connection_tags(&selected, "batch").unwrap();
    let badge = "custom:00000000-0000-4000-8000-000000000001";
    store.set_os_badges(&selected, Some(badge)).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    assert!(
        store
            .set_connection_tags(&[first, 9999], "partial")
            .is_err()
    );
    assert!(store.delete_connections(&[first, 9999]).is_err());
    assert!(store.set_os_badges(&selected, Some("../image")).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    assert_eq!(store.get(third).unwrap().tag, "test");
    store.delete_connections(&selected).unwrap();
    let reopened = profile.store();
    assert_eq!(reopened.connections().len(), 1);
    assert!(reopened.get(third).is_some());
    assert_eq!(reopened.os_badge_id(first), None);
}

#[test]
fn teamwork_favorites_are_local_to_the_source_and_toggle_with_local_connections_atomically() {
    let profile = TestDir::new();
    let mut store = profile.store();
    let local = store.save(connection()).unwrap();
    let external = local.to_string();
    let first = shared_identity(&profile.0.join("first.xml"), &external);
    let second = shared_identity(&profile.0.join("second.xml"), &external);
    assert_ne!(first, second);
    assert!(
        store
            .toggle_connection_favorites(&[local], std::slice::from_ref(&first))
            .unwrap()
    );
    let mut stale = profile.store();
    assert!(stale.is_favorite(local));
    assert!(stale.is_teamwork_favorite(&first));
    assert!(!stale.is_teamwork_favorite(&second));

    assert!(
        store
            .toggle_connection_favorites(&[local], &[first.clone(), second.clone()])
            .unwrap()
    );
    assert!(
        stale
            .toggle_connection_favorites(&[local], std::slice::from_ref(&first))
            .is_err()
    );
    let mut reopened = profile.store();
    assert!(reopened.is_favorite(local));
    assert!(reopened.is_teamwork_favorite(&first));
    assert!(reopened.is_teamwork_favorite(&second));
    assert!(
        !reopened
            .toggle_connection_favorites(&[local], &[first.clone(), second.clone()])
            .unwrap()
    );
    let mut reopened = profile.store();
    assert!(!reopened.is_favorite(local));
    assert!(!reopened.is_teamwork_favorite(&first));
    assert!(!reopened.is_teamwork_favorite(&second));
    assert!(reopened.toggle_connection_favorites(&[], &[]).is_err());
    assert!(
        reopened
            .toggle_connection_favorites(&[u64::MAX], std::slice::from_ref(&second))
            .is_err()
    );
    assert!(!profile.store().is_teamwork_favorite(&second));
}

#[test]
fn teamwork_favorites_do_not_travel_in_portable_profile_backups() {
    let profile = TestDir::new();
    let mut store = profile.store();
    let local = store.save(connection()).unwrap();
    store.toggle_connection_favorites(&[local], &[]).unwrap();
    let baseline = portable_backup(&fs::read(store.path()).unwrap()).unwrap();

    let shared = shared_identity(&profile.0.join("private-share.xml"), "shared");
    assert!(
        store
            .toggle_connection_favorites(&[], std::slice::from_ref(&shared))
            .unwrap()
    );
    let full = fs::read(store.path()).unwrap();
    assert!(validate_backup(&full).is_err());
    let portable = portable_backup(&full).unwrap();
    assert_eq!(portable, baseline);
    assert!(!String::from_utf8_lossy(&portable).contains("private-share.xml"));
    validate_backup(&portable).unwrap();
    assert!(profile.store().is_teamwork_favorite(&shared));
}

#[test]
fn hidden_teamwork_can_be_restored_offline_by_origin_without_removing_favorites() {
    let profile = TestDir::new();
    let mut store = profile.store();
    store.save(connection()).unwrap();
    let original = portable_backup(&fs::read(store.path()).unwrap()).unwrap();
    let path = profile.0.join("first.xml");
    let first_catalog = shared_catalog(&path, "same-id");
    let second_catalog = shared_catalog(&profile.0.join("second.xml"), "same-id");
    let first = teamwork::HiddenEntry::from(first_catalog.entries().next().unwrap());
    let second = teamwork::HiddenEntry::from(second_catalog.entries().next().unwrap());
    assert_ne!(first.identity(), second.identity());
    assert_ne!(first.identity().source(), second.identity().source());
    assert!(store.hidden_teamwork().next().is_none());
    store
        .toggle_connection_favorites(&[], std::slice::from_ref(first.identity()))
        .unwrap();
    store.hide_teamwork(std::slice::from_ref(&first)).unwrap();
    fs::remove_file(&path).unwrap();

    let mut stale = profile.store();
    assert_eq!(stale.hidden_teamwork().collect::<Vec<_>>(), [&first]);
    let offline = stale.hidden_teamwork().next().unwrap();
    assert_eq!(offline.name(), "Shared");
    assert_eq!(offline.host(), "shared.example");
    assert!(stale.is_teamwork_hidden(first.identity()));
    assert!(!stale.is_teamwork_hidden(second.identity()));
    assert!(stale.is_teamwork_favorite(first.identity()));

    store.hide_teamwork(std::slice::from_ref(&second)).unwrap();
    assert!(stale.restore_teamwork(first.identity()).is_err());
    let both = profile.store();
    assert_eq!(both.hidden_teamwork().count(), 2);
    let raw = fs::read(both.path()).unwrap();
    assert!(validate_backup(&raw).is_err());
    assert_eq!(portable_backup(&raw).unwrap(), original);

    store.restore_teamwork(first.identity()).unwrap();
    let mut reopened = profile.store();
    assert!(!reopened.is_teamwork_hidden(first.identity()));
    assert!(reopened.is_teamwork_hidden(second.identity()));
    assert!(reopened.is_teamwork_favorite(first.identity()));
    assert!(reopened.restore_teamwork(first.identity()).is_err());
    assert!(reopened.hide_teamwork(&[]).is_err());
    assert!(!path.exists());
}

#[test]
fn hidden_teamwork_metadata_is_bounded_and_batches_fail_without_writing() {
    use serde_json::{Value, json};

    let profile = TestDir::new();
    let mut store = profile.store();
    store.save(connection()).unwrap();
    let catalog = shared_catalog(&profile.0.join("shared.xml"), "first");
    let first = teamwork::HiddenEntry::from(catalog.entries().next().unwrap());
    let stored = serde_json::to_value(&first).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    let encode = |entries: Vec<Value>| {
        let mut document = original.clone();
        document["library"] = json!({ "teamwork_hidden": entries });
        serde_json::to_vec(&document).unwrap()
    };
    let decode = |entries| decode_document(&encode(entries));
    assert!(decode(vec![stored.clone()]).is_ok());
    assert!(validate_backup(&encode(vec![stored.clone()])).is_err());
    assert!(decode(vec![stored.clone(), stored.clone()]).is_err());
    for (field, value) in [
        ("name", json!("unsafe\u{202e} name")),
        ("name", json!("x".repeat(161))),
        ("host", json!("SHARED.EXAMPLE")),
        ("host", json!("shared.example:22")),
    ] {
        let mut tampered = stored.clone();
        tampered[field] = value;
        let invalid: teamwork::HiddenEntry = serde_json::from_value(tampered.clone()).unwrap();
        assert!(store.hide_teamwork(&[first.clone(), invalid]).is_err());
        assert!(decode(vec![tampered]).is_err());
    }
    let mut injected_route = stored.clone();
    injected_route["username"] = json!("must-not-load");
    assert!(decode(vec![injected_route]).is_err());
    let over_limit = (0..513)
        .map(|index| {
            let mut entry = stored.clone();
            entry["identity"]["id"] = json!(index.to_string());
            entry
        })
        .collect::<Vec<_>>();
    assert!(decode(over_limit.clone()).is_err());
    let over_limit = over_limit
        .into_iter()
        .map(|entry| serde_json::from_value(entry).unwrap())
        .collect::<Vec<_>>();
    assert!(store.hide_teamwork(&over_limit).is_err());
    assert_eq!(fs::read(store.path()).unwrap(), bytes);
    assert!(store.hidden_teamwork().next().is_none());
}

#[test]
fn teamwork_favorite_records_reject_invalid_ids_origins_duplicates_and_counts() {
    use serde_json::{Value, json};

    let item = |kind: &str, location: &str, id: &str| json!({"kind": kind, "location": location, "id": id});
    let decode = |favorites: Vec<Value>| {
        let bytes = serde_json::to_vec(&json!({
            "version": VERSION,
            "next_id": 1,
            "connections": [],
            "approvals": {},
            "library": { "teamwork_favorites": favorites },
        }))
        .unwrap();
        decode_document(&bytes)
    };
    let first = item("git", "https://git.example/team", "shared");
    assert!(decode(vec![first.clone()]).is_ok());
    assert!(
        decode(vec![
            first,
            item("git", "HTTPS://GIT.EXAMPLE:443/team", "shared")
        ])
        .is_err()
    );
    for invalid in [
        item("file", "relative.xml", "shared"),
        item("git", "https://user:password@git.example/team", "shared"),
        item("git", "https://git.example/team", "invalid id"),
        item("git", "https://git.example/team", ""),
        item("unknown", "https://git.example/team", "shared"),
    ] {
        assert!(decode(vec![invalid]).is_err());
    }
    assert!(
        decode(
            (0..513)
                .map(|id| item("git", "https://git.example/team", &id.to_string()))
                .collect()
        )
        .is_err()
    );
}

fn shared_identity(path: &Path, id: &str) -> teamwork::Identity {
    shared_catalog(path, id)
        .entries()
        .next()
        .unwrap()
        .identity()
        .clone()
}

fn shared_catalog(path: &Path, id: &str) -> teamwork::Catalog {
    fs::write(path, format!(r#"<connections><connection id="{id}"><name>Shared</name><host>shared.example</host><username>ops</username></connection></connections>"#)).unwrap();
    let settings = teamwork::Settings {
        sources: vec![teamwork::Source {
            kind: teamwork::SourceKind::File,
            location: path.to_str().unwrap().into(),
            enabled: true,
            check_interval_minutes: teamwork::DEFAULT_CHECK_INTERVAL_MINUTES,
        }],
        ..teamwork::Settings::default()
    };
    teamwork::Catalog::default().refreshed(&settings.active_sources(true, &[]).unwrap())
}

#[test]
fn saved_blank_usernames_and_independent_authentication_references_round_trip() {
    let profile = TestDir::new();
    let mut store = profile.store();
    let mut entry = connection();
    entry.login.endpoint.username.clear();
    entry.login.auth = Auth::Configured {
        method: AuthMethod::PublicKey,
        credential: Some("password-reference".into()),
        key: Some(12),
    };
    let id = store.save(entry).unwrap();
    let reopened = profile.store();
    let login = &reopened.get(id).unwrap().login;
    assert!(login.endpoint.validate().is_err());
    assert!(login.endpoint.validate_saved().is_ok());
    assert_eq!(login.auth.registered_key(), Some(12));
    assert_eq!(
        login.auth.credential_reference(),
        Some("password-reference")
    );
    assert!(matches!(login.auth.active(), ActiveAuth::StoredKey(12)));
    assert!(login.auth.has_vault_reference());
    for username in [" ", " bad", "bad\nname"] {
        let mut endpoint = login.endpoint.clone();
        endpoint.username = username.into();
        assert!(endpoint.validate_saved().is_err());
    }
}

#[test]
fn java_os_symbols_keep_their_ids_and_all_artwork_parses() {
    let expected = [
        "windows-xp",
        "windows-10",
        "windows-11",
        "windows-server",
        "macos",
        "linux",
        "redhat",
        "rocky",
        "almalinux",
        "centos",
        "fedora",
        "debian",
        "ubuntu",
        "linux-mint",
        "arch",
        "suse",
        "chromeos",
        "solaris",
        "aix",
        "hp-ux",
        "dec",
        "freebsd",
        "openbsd",
        "zos",
    ];
    assert!(OsBadge::all().map(OsBadge::id).eq(expected));
    for badge in OsBadge::all() {
        assert_eq!(OsBadge::find(badge.id()), Some(badge));
        assert!(!badge.name().is_empty());
        let tree = resvg::usvg::Tree::from_data(badge.svg(), &Default::default()).unwrap();
        assert_eq!((tree.size().width(), tree.size().height()), (24.0, 24.0));
        assert!(!tree.root().children().is_empty(), "{}", badge.id());
    }
    assert!(OsBadge::find("Windows 11").is_none());
}

#[test]
fn os_symbol_is_optional_and_round_trips_with_the_connection_library() {
    let directory = TestDir::new();
    let mut store = directory.store();
    let id = store.save(connection()).unwrap();
    assert_eq!(directory.store().os_badge(id), None);
    assert!(
        !fs::read_to_string(store.path())
            .unwrap()
            .contains("os_badges")
    );

    let fedora = OsBadge::find("fedora").unwrap();
    store.set_os_badge_id(id, Some(fedora.id())).unwrap();
    let mut store = directory.store();
    assert_eq!(store.os_badge(id), Some(fedora));
    let mut renamed = store.get(id).unwrap().clone();
    renamed.name = "Renamed".into();
    store.save(renamed).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    let portable = portable_backup(&bytes).unwrap();
    let document: Document = serde_json::from_slice(&portable).unwrap();
    assert_eq!(document.library.os_badges[&id], "fedora");

    let group = store.create_group(None, "Replicas").unwrap();
    let copy = store
        .save_in_group_with_badge(
            connection(),
            Some(&group),
            store.os_badge_id(id).map(str::to_owned).as_deref(),
        )
        .unwrap();
    assert_eq!(directory.store().os_badge(copy), Some(fedora));
    store.delete_group(&group).unwrap();
    assert_eq!(directory.store().os_badge(copy), None);
    store.set_os_badge_id(id, None).unwrap();
    assert_eq!(directory.store().os_badge(id), None);
    assert!(
        !fs::read_to_string(store.path())
            .unwrap()
            .contains("os_badges")
    );
}

#[test]
fn group_tab_color_inheritance_follows_rename_and_delete_atomically() {
    let directory = TestDir::new();
    let mut store = directory.store();
    let parent = store.create_group(None, "Production").unwrap();
    let child = store.create_group(Some(&parent), "Database").unwrap();
    let leaf = store.create_group(Some(&child), "Replica").unwrap();
    let neighbor = store.create_group(None, "Production-Other").unwrap();
    let id = store.save_in_group(connection(), Some(&leaf)).unwrap();
    let red = TabColor::PRESETS[0].0;
    let blue = TabColor::PRESETS[4].0;
    store.set_group_color(&parent, Some(red)).unwrap();
    store.set_group_color(&neighbor, Some(red)).unwrap();
    assert_eq!(
        store.inherited_group_color(&leaf),
        Some((parent.as_str(), red))
    );

    let mut stale = directory.store();
    store.set_group_color(&child, Some(blue)).unwrap();
    assert!(stale.set_group_color(&child, None).is_err());
    let mut store = directory.store();
    assert_eq!(
        store.effective_tab_color(store.get(id).unwrap()),
        Some((blue, Some(child.as_str())))
    );
    let mut direct = store.get(id).unwrap().clone();
    direct.tab_color = Some(red);
    assert_eq!(store.effective_tab_color(&direct), Some((red, None)));

    let renamed = store.rename_group(&parent, "Live").unwrap();
    let moved = format!("{renamed}/Database");
    assert_eq!(store.group_color(&parent), None);
    assert_eq!(store.group_color(&renamed), Some(red));
    assert_eq!(
        store.effective_tab_color(store.get(id).unwrap()),
        Some((blue, Some(moved.as_str())))
    );
    store.set_group_color(&moved, None).unwrap();
    assert_eq!(
        store.effective_tab_color(store.get(id).unwrap()),
        Some((red, Some(renamed.as_str())))
    );
    store.delete_group(&renamed).unwrap();
    store.create_group(None, &renamed).unwrap();
    let reopened = directory.store();
    assert_eq!(reopened.group_color(&renamed), None);
    assert_eq!(reopened.group_color(&moved), None);
    assert_eq!(reopened.group_color(&neighbor), Some(red));
    assert!(reopened.get(id).is_none());
}

#[test]
fn approval_survives_description_edits_and_is_invalidated_by_route_or_rule_changes() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let id = store.save(connection()).unwrap();
    let mut profile = store.get(id).unwrap().clone();
    let plan = profile.plan();
    assert!(!store.approved(id, &plan));
    store.approve(id, plan.clone()).unwrap();
    assert!(dir.store().approved(id, &plan));

    profile.tunnels[0].description = "Renamed".into();
    store.save(profile.clone()).unwrap();
    assert!(store.approved(id, &plan));

    profile.tunnels[0].rule.bind.set_port(15433);
    store.save(profile.clone()).unwrap();
    assert!(!store.approved(id, &plan));
    assert!(store.approve(id, plan).is_err());
    let changed = profile.plan();
    store.approve(id, changed.clone()).unwrap();
    profile.login.endpoint.port = 22;
    store.save(profile).unwrap();
    assert!(!dir.store().approved(id, &changed));
}

#[test]
fn terminal_effect_round_trips_and_invalid_speed_cannot_be_loaded() {
    use crate::terminal_effects::{Effect, Speed};

    let dir = TestDir::new();
    let mut store = dir.store();
    let mut profile = connection();
    let legacy = serde_json::to_value(&profile).unwrap();
    assert!(legacy.get("terminal_effect").is_none());
    assert!(
        serde_json::from_value::<Connection>(legacy)
            .unwrap()
            .terminal_effect
            .is_none()
    );

    let selection = Selection {
        effect: Effect::DigitalRain,
        speed: Speed::try_from(2.5000000000000004).unwrap(),
    };
    profile.terminal_effect = Some(selection);
    let id = store.save(profile).unwrap();
    let loaded = dir.store().get(id).unwrap().clone();
    assert_eq!(loaded.terminal_effect, Some(selection));
    let mut value = serde_json::to_value(&loaded).unwrap();
    value["terminal_effect"]["speed"] = serde_json::json!(99);
    let legacy = serde_json::from_value::<Connection>(value.clone()).unwrap();
    assert_eq!(
        legacy.terminal_effect.unwrap().speed,
        Speed::try_from(99.0).unwrap(),
    );
    assert_eq!(serde_json::to_value(&legacy).unwrap(), value);
    for speed in [0, 100] {
        value["terminal_effect"]["speed"] = serde_json::json!(speed);
        assert!(serde_json::from_value::<Connection>(value.clone()).is_err());
    }
    assert_eq!(
        dir.store().get(id).unwrap().terminal_effect,
        Some(selection)
    );
}

#[test]
fn command_timestamp_override_round_trips_and_can_return_to_global_default() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let mut profile = connection();
    let legacy = serde_json::to_value(&profile).unwrap();
    assert!(legacy.get("command_timestamps").is_none());
    assert_eq!(
        serde_json::from_value::<Connection>(legacy)
            .unwrap()
            .command_timestamps,
        None
    );

    for visibility in [Some(true), Some(false), None] {
        profile.command_timestamps = visibility;
        let id = store.save(profile).unwrap();
        profile = dir.store().get(id).unwrap().clone();
        assert_eq!(profile.command_timestamps, visibility);
    }
}

#[test]
fn conflicting_saves_preserve_the_newer_file_and_deleting_never_reuses_an_id() {
    let dir = TestDir::new();
    let mut first = dir.store();
    let mut stale = dir.store();
    let id = first.save(connection()).unwrap();
    assert!(stale.save(connection()).is_err());
    assert_eq!(dir.store().connections().len(), 1);
    first.delete(id).unwrap();
    assert_ne!(first.save(connection()).unwrap(), id);
}

#[test]
fn keyboard_interactive_profiles_round_trip_without_changing_older_logins() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let old = connection();
    let old_json = serde_json::to_value(&old).unwrap();
    assert!(old_json["login"].get("keyboard_interactive").is_none());
    assert!(
        !serde_json::from_value::<Connection>(old_json)
            .unwrap()
            .login
            .keyboard_interactive
    );

    let mut profile = old;
    profile.login.auth = Auth::KeyboardInteractive;
    let mut jump = connection().login;
    jump.endpoint.host = "jump.test.".into();
    jump.keyboard_interactive = true;
    profile.jump = Some(jump);
    let id = store.save(profile).unwrap();
    let reloaded = dir.store();
    let login = reloaded.get(id).unwrap();
    assert_eq!(login.login.auth, Auth::KeyboardInteractive);
    assert!(login.jump.as_ref().unwrap().keyboard_interactive);
    let mut invalid = login.clone();
    invalid.login.keyboard_interactive = true;
    assert!(store.save(invalid).is_err());
}

#[test]
fn library_subtrees_and_metadata_round_trip_without_changing_profile_ids_or_routes() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let root = store.save(connection()).unwrap();
    let bytes = fs::read(store.path()).unwrap();
    assert!(
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap()
            .get("library")
            .is_none()
    );

    let folder = store.create_group(None, " Production ").unwrap();
    let nested = store.create_group(Some(&folder), "Workers").unwrap();
    let empty = store.create_group(Some(&nested), "Empty").unwrap();
    let sibling = store.create_group(None, "Production-old").unwrap();
    assert!(store.create_group(Some(&folder), "Workers").is_err());
    assert!(store.create_group(None, "../outside").is_err());
    assert!(store.create_group(Some("missing"), "sub").is_err());
    let member = store.save_in_group(connection(), Some(&nested)).unwrap();
    let adjacent = store.save_in_group(connection(), Some(&sibling)).unwrap();
    let plan = store.get(member).unwrap().plan();
    store.approve(member, plan.clone()).unwrap();
    assert!(store.toggle_connection_favorites(&[member], &[]).unwrap());
    store.record_used(member).unwrap();

    let renamed = store.rename_group(&folder, "Live").unwrap();
    assert_eq!(renamed, "Live");
    assert_eq!(store.group_of(member), Some("Live/Workers"));
    assert_eq!(store.group_of(adjacent), Some(sibling.as_str()));
    assert!(store.has_group("Live/Workers/Empty"));
    assert!(!store.has_group(&empty));
    assert_eq!(
        store
            .in_group("Live")
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
        [member]
    );
    assert!(store.approved(member, &plan));
    assert!(store.is_favorite(member));

    let mut reopened = dir.store();
    assert_eq!(
        reopened.recent().map(|entry| entry.id).collect::<Vec<_>>(),
        [member]
    );
    assert_eq!(reopened.group_of(root), None);
    reopened.delete_group("Live").unwrap();
    assert!(reopened.get(member).is_none());
    assert!(reopened.get(adjacent).is_some());
    assert!(!reopened.is_favorite(member));
    assert!(reopened.recent().next().is_none());
    assert!(!reopened.approved(member, &plan));
    assert!(reopened.save(connection()).unwrap() > adjacent);
}

#[test]
fn open_recent_connection_times_filter_without_erasing_quick_connect_history() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let first = store.save(connection()).unwrap();
    let second = store.save(connection()).unwrap();
    store.record_used_at(first, 10).unwrap();
    store.record_used_at(second, 20).unwrap();
    store.record_used_at(second, 40).unwrap();
    store.record_used_at(first, 15).unwrap();
    store.record_used_at(first, 12).unwrap();

    let reopened = dir.store();
    assert_eq!(
        reopened.recent().map(|entry| entry.id).collect::<Vec<_>>(),
        [second, first]
    );
    assert_eq!(
        reopened
            .recent_since(25)
            .map(|(entry, time)| (entry.id, time))
            .collect::<Vec<_>>(),
        [(second, 40)]
    );
    assert_eq!(reopened.recent_since(40).count(), 0);

    let mut legacy = reopened.document.clone();
    legacy.library.used_at.clear();
    reopened.write(&legacy).unwrap();
    let legacy = dir.store();
    assert_eq!(
        legacy
            .recent_since(0)
            .map(|(entry, time)| (entry.id, time))
            .collect::<Vec<_>>(),
        [(second, 0), (first, 0)]
    );
    assert_eq!(legacy.recent_since(1).count(), 0);
    assert_eq!(legacy.recent().count(), 2);
}

#[test]
fn concurrent_recent_updates_merge_and_never_record_a_changed_route_or_incognito_profile() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let ids: Vec<_> = (0..17).map(|_| store.save(connection()).unwrap()).collect();
    let mut editor = dir.store();
    for &id in &ids {
        store.record_used(id).unwrap();
    }
    let mut edited = editor.get(ids[0]).unwrap().clone();
    edited.name = "Renamed while recent activity was recorded".into();
    editor.save(edited).unwrap();
    assert_eq!(
        editor.recent().map(|entry| entry.id).collect::<Vec<_>>(),
        ids[2..].iter().rev().copied().collect::<Vec<_>>()
    );
    assert_eq!(
        editor.recent_since(0).filter(|(_, time)| *time > 0).count(),
        15
    );

    let mut stale_route = dir.store();
    let mut edited = editor.get(ids[0]).unwrap().clone();
    edited.login.endpoint.port = 22;
    editor.save(edited).unwrap();
    stale_route.record_used(ids[0]).unwrap();
    assert!(!stale_route.recent().any(|entry| entry.id == ids[0]));

    let mut stale_privacy = dir.store();
    let mut edited = editor.get(ids[1]).unwrap().clone();
    edited.incognito = true;
    editor.save(edited).unwrap();
    stale_privacy.record_used(ids[1]).unwrap();
    assert!(!stale_privacy.recent().any(|entry| entry.id == ids[1]));
    assert!(stale_privacy.get(ids[1]).unwrap().incognito);
}

#[test]
fn shared_search_follows_anchored_literal_star_and_quick_connection_fields() {
    let dir = TestDir::new();
    let mut store = dir.store();
    let mut first = connection();
    first.name = "Web-[01]".into();
    first.login.endpoint.host = "192.0.2.10".into();
    let first_id = store.save(first).unwrap();
    let mut second = connection();
    second.name = "Old-Web-[01]".into();
    let second_id = store.save(second).unwrap();
    let ids = |query| {
        store
            .search(query)
            .map(|entry| entry.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids("web-[*]"), [first_id]);
    assert_eq!(ids("web"), [first_id, second_id]);
    assert_eq!(ids("192.0.*"), [first_id]);
    assert!(ids("192x0.*").is_empty());
    assert_eq!(ids("operator@192.0.*"), [first_id]);
    assert_eq!(ids("operator"), [first_id, second_id]);
    assert!(!Search::new("operator").manager(store.get(first_id).unwrap()));
    assert_eq!(ids("   "), [first_id, second_id]);
}

#[test]
fn ssh_startup_policy_round_trips_and_old_profiles_keep_original_bounds() {
    use kortty_ssh::KeepAlive;

    let mut old = connection();
    let old_document = serde_json::to_value(&old).unwrap();
    assert!(old_document.get("startup").is_none());
    let legacy: Connection = serde_json::from_value(old_document.clone()).unwrap();
    assert_eq!(legacy.startup, ConnectOptions::default());

    let dir = TestDir::new();
    let mut store = dir.store();
    let startup = ConnectOptions::new(300, 20)
        .unwrap()
        .with_keepalive(Some(KeepAlive::new(false, 600).unwrap()));
    old.startup = startup;
    let id = store.save(old).unwrap();
    assert_eq!(dir.store().get(id).unwrap().startup, startup,);

    for (seconds, attempts) in [(0, 1), (301, 1), (20, 21)] {
        let mut invalid = old_document.clone();
        invalid["startup"] = serde_json::json!({"timeout_seconds": seconds, "attempts": attempts});
        assert!(serde_json::from_value::<Connection>(invalid).is_err());
    }
    for seconds in [4, 601] {
        let mut invalid = old_document.clone();
        invalid["startup"] = serde_json::json!({
            "timeout_seconds": 20, "attempts": 1, "keepalive": {"enabled": true, "seconds": seconds},
        });
        assert!(serde_json::from_value::<Connection>(invalid).is_err());
    }
}
