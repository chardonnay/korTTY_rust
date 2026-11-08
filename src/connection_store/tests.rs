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
    assert!(store.toggle_favorites(&selected).unwrap());
    assert!(selected.iter().all(|id| store.is_favorite(*id)));
    assert!(!store.toggle_favorites(&selected).unwrap());
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
fn terminal_effect_round_trips_and_invalid_speed_cannot_be_saved() {
    use crate::terminal_effects::Effect;

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
        speed: 99,
    };
    profile.terminal_effect = Some(selection);
    let id = store.save(profile).unwrap();
    let loaded = dir.store().get(id).unwrap().clone();
    assert_eq!(loaded.terminal_effect, Some(selection));
    for speed in [0, 100] {
        let mut invalid = loaded.clone();
        invalid.terminal_effect = Some(Selection { speed, ..selection });
        assert_eq!(
            store.save(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
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
    store.set_favorite(member, true).unwrap();
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
    let mut old = connection();
    let old_document = serde_json::to_value(&old).unwrap();
    assert!(old_document.get("startup").is_none());
    let legacy: Connection = serde_json::from_value(old_document.clone()).unwrap();
    assert_eq!(legacy.startup, ConnectOptions::default());

    let dir = TestDir::new();
    let mut store = dir.store();
    old.startup = ConnectOptions::new(300, 20).unwrap();
    let id = store.save(old).unwrap();
    assert_eq!(
        dir.store().get(id).unwrap().startup,
        ConnectOptions::new(300, 20).unwrap()
    );

    for (seconds, attempts) in [(0, 1), (301, 1), (20, 21)] {
        let mut invalid = old_document.clone();
        invalid["startup"] = serde_json::json!({"timeout_seconds": seconds, "attempts": attempts});
        assert!(serde_json::from_value::<Connection>(invalid).is_err());
    }
}
