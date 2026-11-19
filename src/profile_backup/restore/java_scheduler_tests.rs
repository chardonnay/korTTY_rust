use std::time::{Duration, SystemTime};

use kortty_jobs::{
    DependencyMode, Job, JobGraph, JobId, RunOutcome, RunRecord, Scheduler, SchedulerStore,
};
use kortty_vault::Secret;
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::connection_store::{Store, import::Preview as ConnectionPreview};
use crate::scheduler_window::{ConnectionRef, RemoteAction, SshCommand};

const SCHEDULER: &[u8] = include_bytes!("../java_scheduler/fixtures/java-2.26.0.xml");
const CONNECTION_XML: &[u8] = br#"<connections>
    <connection id="b04d413f-9ee5-4b56-bc12-c72da52d4b3e">
        <name>Primary</name><host>primary.example</host><port>2222</port><username>fixture</username>
        <jumpServer><enabled>true</enabled><host>jump.example</host><port>2200</port><username>jump-user</username></jumpServer>
    </connection>
    <connection id="9c0719d8-dfa7-4468-a1d9-4149cb8fa01e">
        <name>Secondary</name><host>secondary.example</host><username>other</username>
    </connection>
</connections>"#;
const LOCAL_CONNECTION: &[u8] = br#"<connections><connection id="unrelated"><name>Local</name><host>local.example</host><username>native</username></connection></connections>"#;
const GROUP_CONNECTIONS: &str = r#"<connections>
    <connection id="9c0719d8-dfa7-4468-a1d9-4149cb8fa01e">
        <name>Secondary</name><host>secondary.example</host><username>other</username>
        <group> / Ops / Hosts / Child / </group>
    </connection>
    <connection id="sibling">
        <name>Sibling</name><host>sibling.example</host><username>other</username>
        <group>Ops/Hosts-old</group>
    </connection>
    <connection id="b04d413f-9ee5-4b56-bc12-c72da52d4b3e">
        <name>Primary</name><host>primary.example</host><username>fixture</username>
        <group>Ops/Hosts</group>
    </connection>
</connections>"#;
const GROUP_JOB: &str = "java-7ec12ac55d6e496b947cdeb608c9f11d";
const SNIPPET_XML: &[u8] =
    include_bytes!("../../snippets/archive/fixtures/java-private-library-2.26.0.xml");
const GLOBAL_SETTINGS: &[u8] =
    include_bytes!("../../ai_manager/profiles/fixtures/java-global-settings.xml");
const COMMAND: &str = "java-3f9bab16184b4e13bf71e37331b8843a";
const SNIPPET: &str = "java-0bd3734742024c5a912d4fe43c09f755";
const PRIVILEGED: &str = "java-e02d8f33ccdb4fd8bd8c340688f038a3";
const PRIVILEGED_DEPENDENT: &str = "java-50dc55a9cac94798932908798863715b";
const AGENT: &str = "java-2bd70fbea5e44b29b621370fcce50b96";

struct Fixture {
    _scratch: crate::snippets::tests::Profile,
    root: PathBuf,
    archive: PathBuf,
    password: Secret,
}

impl Fixture {
    fn new() -> Self {
        let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
        Self::with_scheduler(scheduler)
    }

    fn with_scheduler(scheduler: &str) -> Self {
        Self::with_sources(scheduler, None)
    }

    fn with_sources(scheduler: &str, global_settings: Option<&[u8]>) -> Self {
        Self::with_connections(scheduler, CONNECTION_XML, global_settings)
    }

    fn with_connections(
        scheduler: &str,
        connections: &[u8],
        global_settings: Option<&[u8]>,
    ) -> Self {
        let scratch = crate::snippets::tests::Profile::new();
        let archive = scratch.0.join("java-scheduler.zip");
        let root = scratch.0.join("profile");
        let password = Secret::new("fixture backup password".into());
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        let options = SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
        let scheduler = scheduler.replace(
            "3ee66545-2eaf-4ac7-b352-d9a1a51d75ed",
            "00000000-0000-0000-0000-000000000001",
        );
        for (name, bytes) in [
            ("connections.xml", connections),
            ("snippets.xml", SNIPPET_XML),
            ("job-scheduler.xml", scheduler.as_bytes()),
        ]
        .into_iter()
        .chain(global_settings.map(|bytes| ("global-settings.xml", bytes)))
        {
            zip.start_file(name, options).unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        Self {
            _scratch: scratch,
            root,
            archive,
            password,
        }
    }

    fn queue(&self) -> QueuedRestore {
        let preview = super::super::inspect_java_password(&self.archive, &self.password).unwrap();
        super::super::queue_restore_at(&self.root, preview, RestoreMode::JavaAdditive).unwrap()
    }

    fn apply(&self) {
        drop(ProfileLease::acquire(&self.root, || false).unwrap());
        assert!(!self.root.join(PENDING).exists());
    }

    fn seed_local(&self, id: &str) -> (ConnectionRef, RunRecord) {
        let directory = Directory::open(&self.root, true).unwrap();
        let local = self.seed_connection(&directory, LOCAL_CONNECTION);
        let action: SshCommand = serde_json::from_value(serde_json::json!({
            "connection": local,
            "command": "printf local",
        }))
        .unwrap();
        let id = JobId::new(id).unwrap();
        let graph =
            JobGraph::new(vec![Job::new(id.clone(), "Existing job", action).unwrap()]).unwrap();
        let mut scheduler = Scheduler::new(graph);
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(3);
        let run = scheduler.start_manual(&id, start).unwrap().started[0].id;
        let finished = scheduler
            .complete(run, RunOutcome::Success, start + Duration::from_secs(1))
            .unwrap()
            .finished
            .remove(0);
        let mut jobs = SchedulerStore::open(self.root.join(JOBS)).unwrap();
        jobs.commit_with_actions(
            scheduler.graph(),
            std::slice::from_ref(&finished),
            &[(run, "Existing journal detail".into())],
        )
        .unwrap();
        (local, finished)
    }

    fn seed_connection(&self, directory: &Directory, bytes: &[u8]) -> ConnectionRef {
        let (connections, count) = ConnectionPreview::java_backup(bytes)
            .unwrap()
            .portable_java_backup()
            .unwrap();
        assert_eq!(count, 1);
        directory.publish_copy(CONNECTIONS, &connections).unwrap();
        let store = Store::open(self.root.join(CONNECTIONS)).unwrap();
        ConnectionRef::from(&store.connections()[0])
    }
}

#[test]
fn java_scheduler_restore_stages_shared_targets_with_exact_source_and_membership() {
    use crate::clipboard_policy::ClipboardPolicy;
    use crate::connection_store::teamwork::{Catalog, ManagedSource, Source, SourceKind};
    use crate::scheduler_window::{SharedAuth, SharedTargets};

    let ordered = r#"<connection id="z-order"><name>Ordered</name><host>ordered.example</host>
        <username>fixture</username><group>Ops/Hosts</group></connection>"#;
    let plain = GROUP_CONNECTIONS.replace("<connections>", &format!("<connections>{ordered}")).replace(
        "<group>Ops/Hosts</group>",
        "<group>Ops/Hosts</group><connectionSource>TEAMWORK</connectionSource><teamworkSourceId>fleet</teamworkSourceId>",
    );
    let test_profile = crate::snippets::tests::Profile::new();
    let file = test_profile.0.join("shared.xml");
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
    let globals = format!(
        "<globalSettings><teamworkSources><source><id>fleet</id><type>SHARED_FILE</type><location>{}</location></source></teamworkSources></globalSettings>",
        file.display()
    );
    let fixture = Fixture::with_connections(scheduler, plain.as_bytes(), Some(globals.as_bytes()));
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let source = preview.java.unwrap().scheduler.unwrap();
    let directory = Directory::open(&fixture.root, true).unwrap();
    let local = super::stage_java_connections(
        &directory,
        &source.connections,
        Default::default(),
        JavaTargetReuse {
            toolbar: false,
            scheduler: true,
        },
    )
    .unwrap()
    .unwrap();
    let store =
        crate::connection_store::import::java_connection_candidate(&local.contents).unwrap();
    let policy = ClipboardPolicy {
        custom_teamwork_sources_allowed: false,
        teamwork_sources: vec![ManagedSource {
            name: "Fleet".into(),
            source: Source {
                location: file.to_str().unwrap().into(),
                enabled: true,
                kind: SourceKind::File,
                check_interval_minutes: 15,
            },
        }],
        ..ClipboardPolicy::default()
    };
    let shared_row = r#"<connection id="b04d413f-9ee5-4b56-bc12-c72da52d4b3e">
        <name>Primary</name><host>primary.example</host><username>fixture</username><group>Ops/Hosts</group>
    </connection>"#;
    let key = kortty_ssh::HostKey::from_openssh(
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX",
    )
    .unwrap();
    let trust = kortty_ssh::TrustStore::from_pins(
        ["primary.example", "ordered.example"]
            .into_iter()
            .map(|host| (kortty_ssh::Endpoint::new(host, 22).unwrap(), key.clone()))
            .collect(),
    )
    .unwrap();
    for (extra, expect_group) in [
        ("", true),
        (
            r#"<connection id="extra"><host>new.example</host><username>fixture</username><group>Ops/Hosts</group></connection>"#,
            false,
        ),
    ] {
        fs::write(
            &file,
            format!("<connections>{shared_row}{ordered}{extra}</connections>"),
        )
        .unwrap();
        let catalog = Catalog::load_for_scheduler(&fixture.root, &policy).unwrap();
        let shared = SharedTargets::new(&catalog, &store, &trust, |entry| {
            Some((
                entry.connection.login.endpoint.clone(),
                SharedAuth::Key {
                    id: 1,
                    password_preference: None,
                },
            ))
        });
        let mut notes = Vec::new();
        let backup =
            super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
        let bytes = super::java_jobs::stage(
            &directory,
            &backup.snapshot,
            Some(&local),
            backup.java.unwrap().scheduler.unwrap(),
            &shared,
            &policy,
            &mut notes,
        )
        .unwrap()
        .unwrap();
        let jobs = super::java_scheduler::decode(&bytes).unwrap();
        let command = jobs.job(&JobId::new(COMMAND).unwrap()).unwrap();
        assert!(!command.enabled());
        assert!(
            command
                .action()
                .connection
                .as_ref()
                .unwrap()
                .shared_identity()
                .is_some()
        );
        let blocked = crate::clipboard_policy::parse(
            "[meta]
schema-version = 1
[[rule]]
[rule.servers]
mode = 'deny'
hosts = ['primary.example:22']",
        )
        .unwrap();
        let isolated = ClipboardPolicy {
            minimum_isolation: crate::session_security::IsolationLevel::Process,
            ..ClipboardPolicy::default()
        };
        for denied in [&blocked, &isolated] {
            let backup =
                super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
            if let Some(bytes) = super::java_jobs::stage(
                &directory,
                &backup.snapshot,
                Some(&local),
                backup.java.unwrap().scheduler.unwrap(),
                &shared,
                denied,
                &mut Vec::new(),
            )
            .unwrap()
            {
                let remaining = super::java_scheduler::decode(&bytes).unwrap();
                for id in [COMMAND, GROUP_JOB] {
                    assert!(remaining.job(&JobId::new(id).unwrap()).is_none());
                }
            }
        }
        let grouped = jobs.job(&JobId::new(GROUP_JOB).unwrap());
        assert_eq!(grouped.is_some(), expect_group);
        if let Some(grouped) = grouped {
            assert!(!grouped.enabled());
            assert_eq!(
                grouped
                    .action()
                    .target_groups
                    .as_ref()
                    .unwrap()
                    .groups
                    .len(),
                2
            );
            assert!(
                grouped
                    .action()
                    .require_current_targets(&store, &shared)
                    .is_ok()
            );
            assert!(
                grouped
                    .action()
                    .targets()
                    .map(|route| route.target.host.as_str())
                    .eq(["ordered.example", "secondary.example", "primary.example"])
            );
        }
    }
}

#[test]
fn java_scheduler_restore_preserves_group_order_and_explicit_selection() {
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap().replacen(
        "<targetGroupNames/>",
        "<targetGroupNames><groupName> / Ops / Hosts / </groupName>\
         <groupName>Ops/Hosts/Child</groupName></targetGroupNames>",
        1,
    );
    let fixture = Fixture::with_connections(&scheduler, GROUP_CONNECTIONS.as_bytes(), None);
    fixture.queue();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    fixture.apply();

    let connections = Store::open(fixture.root.join(CONNECTIONS)).unwrap();
    assert_eq!(connections.connections().len(), 3);
    let members: Vec<_> = connections
        .in_group("Ops/Hosts")
        .map(ConnectionRef::from)
        .collect();
    let [secondary, primary] = members.as_slice() else {
        panic!("wrong Scheduler group membership: {members:?}");
    };
    assert_eq!(secondary.target.host, "secondary.example");
    assert_eq!(primary.target.host, "primary.example");

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let get = |id| scheduler.graph().job(&JobId::new(id).unwrap()).unwrap();
    for (id, group_paths, explicit, ordered_targets) in [
        (
            COMMAND,
            &["Ops/Hosts", "Ops/Hosts/Child"][..],
            std::slice::from_ref(primary),
            [primary, secondary],
        ),
        (GROUP_JOB, &["Ops/Hosts"][..], &[][..], [secondary, primary]),
    ] {
        let job = get(id);
        assert!(!job.enabled());
        let action = job.action();
        let groups = action.target_groups.as_ref().unwrap();
        assert!(
            groups.groups.iter().cloned().eq(group_paths
                .iter()
                .copied()
                .map(crate::scheduler_window::ConnectionGroup::from))
        );
        assert_eq!(groups.explicit.as_slice(), explicit);
        assert!(action.targets().eq(ordered_targets));
        assert!(action.require_current_target_groups(&connections).is_ok());
    }
    drop(store);

    let jobs = fs::read(fixture.root.join(JOBS)).unwrap();
    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_JOBS_COMMITTING;
    Directory::open(&fixture.root, false)
        .unwrap()
        .publish_copy(PENDING, &committing)
        .unwrap();
    fixture.apply();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), jobs);
}

#[test]
fn java_scheduler_manual_group_review_binds_a_renamed_group_with_exact_members() {
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
    let fixture = Fixture::with_connections(scheduler, GROUP_CONNECTIONS.as_bytes(), None);
    let directory = Directory::open(&fixture.root, true).unwrap();
    let saved_source = GROUP_CONNECTIONS
        .replace(" / Ops / Hosts / Child / ", "Saved/Hosts/Child")
        .replace("Ops/Hosts", "Saved/Hosts")
        .replace(
            "</connections>",
            r#"<connection><name>Primary alone</name>
            <host>primary.example</host><username>fixture</username><group>Incomplete</group>
            </connection></connections>"#,
        );
    let (bytes, count) = ConnectionPreview::java_backup(saved_source.as_bytes())
        .unwrap()
        .portable_java_backup()
        .unwrap();
    assert_eq!(count, 4);
    directory.publish_copy(CONNECTIONS, &bytes).unwrap();
    let saved = Store::open(fixture.root.join(CONNECTIONS)).unwrap();
    let expected: Vec<_> = saved
        .in_group("Saved/Hosts")
        .map(ConnectionRef::from)
        .collect();
    let mut preview =
        super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    preview
        .prepare_java_scheduler_review_at(&fixture.root)
        .unwrap();
    let review = preview.java_scheduler_review_mut().unwrap();
    let index = (0..review.len())
        .find(|&index| {
            review
                .group_row(index)
                .is_some_and(|(path, _, _)| path == "Ops/Hosts")
        })
        .unwrap();
    let (_, choices, _) = review.group_row(index).unwrap();
    assert_eq!(
        choices
            .iter()
            .map(|choice| choice.0.as_deref())
            .collect::<Vec<_>>(),
        [None, Some("Saved/Hosts")]
    );
    assert!(
        review
            .select_group(index, Some("Incomplete".into()))
            .is_err()
    );
    review
        .select_group(index, Some("Saved/Hosts".into()))
        .unwrap();
    super::super::queue_restore_at(&fixture.root, preview, RestoreMode::JavaAdditive).unwrap();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let group = scheduler
        .graph()
        .job(&JobId::new(GROUP_JOB).unwrap())
        .unwrap();
    assert!(!group.enabled());
    assert!(group.action().targets().eq(&expected));
    assert_eq!(
        group.action().target_groups.as_ref().unwrap().groups,
        [crate::scheduler_window::ConnectionGroup::from(
            "Saved/Hosts"
        )]
    );
    let direct = scheduler
        .graph()
        .job(&JobId::new(COMMAND).unwrap())
        .unwrap();
    assert_eq!(
        direct.action().connection.as_ref(),
        expected
            .iter()
            .find(|member| member.target.host == "primary.example")
    );
    assert!(store.journal().is_empty());
}

#[test]
fn java_scheduler_manual_target_review_binds_only_the_same_route() {
    let fixture = Fixture::new();
    let directory = Directory::open(&fixture.root, true).unwrap();
    let native_source = std::str::from_utf8(CONNECTION_XML).unwrap().replace(
        "<name>Primary</name>",
        "<name>Primary</name><tag>Local settings</tag>",
    );
    let (bytes, count) = ConnectionPreview::java_backup(native_source.as_bytes())
        .unwrap()
        .portable_java_backup()
        .unwrap();
    assert_eq!(count, 2);
    directory.publish_copy(CONNECTIONS, &bytes).unwrap();
    let current = Store::open(fixture.root.join(CONNECTIONS)).unwrap();
    let primary = &current.connections()[0];
    let secondary = &current.connections()[1];
    let preview = || {
        let mut preview =
            super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
        preview
            .prepare_java_scheduler_review_at(&fixture.root)
            .unwrap();
        let review = preview.java_scheduler_review_mut().unwrap();
        assert_eq!(review.len(), 1);
        let (_, choices, selected) = review.row(0);
        assert!(selected.is_some_and(|choice| choice.id.is_none()));
        assert_eq!(
            choices.iter().map(|choice| choice.id).collect::<Vec<_>>(),
            [None, Some(primary.id)]
        );
        assert!(review.select(0, Some(secondary.id)).is_err());
        review.select(0, Some(primary.id)).unwrap();
        preview
    };
    let inspected = preview();
    directory
        .replace_file(CONNECTIONS, &[bytes.as_slice(), b" "].concat())
        .unwrap();
    assert!(
        super::super::queue_restore_at(&fixture.root, inspected, RestoreMode::JavaAdditive)
            .is_err_and(|error| error.to_string().contains("Saved connections changed"))
    );
    assert!(!directory.has_entry(PENDING).unwrap());
    directory.replace_file(CONNECTIONS, &bytes).unwrap();
    super::super::queue_restore_at(&fixture.root, preview(), RestoreMode::JavaAdditive).unwrap();
    fixture.apply();
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let command = scheduler
        .graph()
        .job(&JobId::new(COMMAND).unwrap())
        .unwrap();
    assert!(!command.enabled());
    assert_eq!(
        command.action().connection,
        Some(ConnectionRef::from(primary))
    );
    assert!(store.journal().is_empty());
    assert_eq!(fs::read(fixture.root.join(CONNECTIONS)).unwrap(), bytes);
}

#[test]
fn java_scheduler_reuses_exact_saved_targets_and_keeps_archive_group_order() {
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
    let fixture = Fixture::with_connections(scheduler, GROUP_CONNECTIONS.as_bytes(), None);
    let directory = Directory::open(&fixture.root, true).unwrap();
    let document = roxmltree::Document::parse(GROUP_CONNECTIONS).unwrap();
    let reversed: Vec<_> = document
        .root_element()
        .children()
        .filter(roxmltree::Node::is_element)
        .map(|node| &GROUP_CONNECTIONS[node.range()])
        .collect();
    let reordered = format!(
        "<connections>{}{}{}</connections>",
        reversed[2], reversed[1], reversed[0]
    );
    let (mut current, count) = ConnectionPreview::java_backup(reordered.as_bytes())
        .unwrap()
        .portable_java_backup()
        .unwrap();
    assert_eq!(count, 3);
    current.extend_from_slice(b"\n\n");
    directory.publish_copy(CONNECTIONS, &current).unwrap();
    let connections = Store::open(fixture.root.join(CONNECTIONS)).unwrap();
    let group_members = connections
        .in_group("Ops/Hosts")
        .map(ConnectionRef::from)
        .collect::<Vec<_>>();
    assert!(
        group_members
            .iter()
            .map(|member| member.target.host.as_str())
            .eq(["primary.example", "secondary.example"])
    );

    fixture.queue();
    assert_eq!(
        fs::read(fixture.root.join(PENDING)).unwrap()[PENDING_MAGIC_WITH_MODE.len()],
        JAVA_JOBS
    );
    directory
        .replace_file(CONNECTIONS, &[current.as_slice(), b" "].concat())
        .unwrap();
    assert!(
        ProfileLease::acquire(&fixture.root, || false)
            .is_err_and(|error| error.to_string().contains("connections changed"))
    );
    assert!(!directory.has_entry(JOBS).unwrap());
    discard_pending_java(&pending_java_cancellation(&fixture.root).unwrap().unwrap()).unwrap();
    directory.replace_file(CONNECTIONS, &current).unwrap();

    fixture.queue();
    fixture.apply();
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let group = scheduler
        .graph()
        .job(&JobId::new(GROUP_JOB).unwrap())
        .unwrap();
    assert!(!group.enabled());
    let expected = [&group_members[1], &group_members[0]];
    assert!(group.action().targets().eq(expected));
    let targets = group.action().target_groups.as_ref().unwrap();
    assert!(
        targets
            .group_order
            .iter()
            .copied()
            .eq(expected.map(|member| member.id))
    );
    group
        .action()
        .require_current_target_groups(&connections)
        .unwrap();
    assert!(store.journal().is_empty());
    drop(store);
    let jobs = fs::read(fixture.root.join(JOBS)).unwrap();

    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), jobs);
    assert_eq!(fs::read(fixture.root.join(CONNECTIONS)).unwrap(), current);
}

#[test]
fn java_scheduler_restore_omits_groups_with_extra_local_or_unmapped_source_members() {
    const UNMAPPED: &str = "<connection><name>Unmapped</name><host>unknown.example</host>\
        <username>fixture</username><group>Ops/Hosts/Child</group></connection>";
    let local = std::str::from_utf8(LOCAL_CONNECTION).unwrap().replace(
        "</connection>",
        "<group>Ops/Hosts/Local</group></connection>",
    );
    let with_unmapped =
        GROUP_CONNECTIONS.replace("</connections>", &format!("{UNMAPPED}</connections>"));
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
    for (source, local) in [
        (GROUP_CONNECTIONS, Some(local.as_bytes())),
        (&with_unmapped, None),
    ] {
        let fixture = Fixture::with_connections(scheduler, source.as_bytes(), None);
        if let Some(local) = local {
            let directory = Directory::open(&fixture.root, true).unwrap();
            fixture.seed_connection(&directory, local);
        }
        let queued = fixture.queue();
        let reason = java_scheduler::Omission::GroupTargets.description();
        assert!(queued.scheduler_notes.contains(&format!("{reason} (1)")));
        fixture.apply();

        let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
        assert!(
            store
                .definitions()
                .iter()
                .any(|job| job.id.as_str() == COMMAND)
        );
        assert!(
            store
                .definitions()
                .iter()
                .all(|job| job.id.as_str() != GROUP_JOB)
        );
    }
}

#[test]
fn java_scheduler_restore_rechecks_group_targets_in_the_pending_catalogue() {
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap();
    let fixture = Fixture::with_connections(scheduler, GROUP_CONNECTIONS.as_bytes(), None);
    fixture.queue();

    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    let (marker, payload) = pending.split_at(PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN);
    let plan_size = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let (header, snapshot) = payload.split_at(4 + plan_size);
    let mut plan: JavaCredentialPlan = serde_json::from_slice(&header[4..]).unwrap();
    let mut snapshot = archive::decode_snapshot(snapshot).unwrap();
    let catalogue = snapshot.get_mut(CONNECTIONS).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(catalogue).unwrap();
    let membership = changed["library"]["membership"].as_object_mut().unwrap();
    let group = membership
        .values_mut()
        .find(|path| path.as_str() == Some("Ops/Hosts/Child"))
        .unwrap();
    *group = serde_json::json!("Ops/Hosts-old");
    **catalogue = serde_json::to_vec(&changed).unwrap();
    // Update the digest to exercise the independent startup target check.
    plan.connections.as_mut().unwrap().after = java_credential_restore::digest(catalogue);
    let plan = serde_json::to_vec(&plan).unwrap();
    let mut altered = marker.to_vec();
    altered.extend_from_slice(&(plan.len() as u32).to_le_bytes());
    altered.extend_from_slice(&plan);
    altered.extend_from_slice(&archive::encode_snapshot(&snapshot).unwrap());
    Directory::open(&fixture.root, false)
        .unwrap()
        .replace_file(PENDING, &altered)
        .unwrap();

    let error = ProfileLease::acquire(&fixture.root, || false)
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("Saved connection group targets changed")
    );
    assert!(!fixture.root.join(CONNECTIONS).exists());
    assert!(!fixture.root.join(JOBS).exists());
    assert!(!fixture.root.join(snippets::FILE_NAME).exists());
    let receipt = pending_java_cancellation(&fixture.root).unwrap().unwrap();
    discard_pending_java(&receipt).unwrap();
}

#[test]
fn java_scheduler_restore_keeps_disabled_command_and_snippet_virtual_terminals() {
    use crate::job_executor::virtual_terminal::Options;

    let controls = r#"<ptyEnabled>true</ptyEnabled>
                <ptyColumns>150</ptyColumns><ptyRows>50</ptyRows>
                <screenshotIntervalSeconds>0</screenshotIntervalSeconds>
                <screenshotOnChange>false</screenshotOnChange><maxScreenshots>21</maxScreenshots>
                <runtimeLimitSeconds>74</runtimeLimitSeconds>
                <runtimeLimitSuccess>true</runtimeLimitSuccess>"#;
    let inactive = "<ptyEnabled>false</ptyEnabled>\n                <runtimeLimitSuccess>false</runtimeLimitSuccess>";
    let scheduler = std::str::from_utf8(SCHEDULER)
        .unwrap()
        .replace("\r\n", "\n")
        .replacen(
            inactive,
            "<ptyEnabled>true</ptyEnabled>",
            1,
        )
        .replacen(inactive, controls, 1)
        .replacen(
            "<connectionId>9c0719d8-dfa7-4468-a1d9-4149cb8fa01e</connectionId>",
            "",
            1,
        )
        .replace(
            inactive,
            "<ptyEnabled>false</ptyEnabled><runtimeLimitSeconds>0</runtimeLimitSeconds><runtimeLimitSuccess>false</runtimeLimitSuccess>",
        );
    let fixture = Fixture::with_scheduler(&scheduler);
    fixture.queue();
    assert!(!fixture.root.join(JOBS).exists());
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    assert!(store.journal().is_empty());
    assert_eq!(store.journal_recordings().count(), 0);
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let graph = scheduler.graph();
    let get = |id| graph.job(&JobId::new(id).unwrap()).unwrap();
    let command = get(COMMAND);
    let snippet = get(SNIPPET);
    let agent = get(AGENT);
    assert!(!command.enabled() && !snippet.enabled() && !agent.enabled());
    assert_eq!(command.action().virtual_terminal, Some(Options::default()));
    assert_eq!(
        snippet.action().virtual_terminal,
        Some(Options {
            columns: 150,
            rows: 50,
            interval_seconds: 0,
            capture_changes: false,
            max_frames: 21,
            limit_seconds: 74,
            limit_is_success: true,
        }),
    );
    assert_eq!(command.action().connection, snippet.action().connection);
    assert!(snippet.action().additional_connections.is_empty());
    assert!(snippet.action().snippet.is_some());
    assert_eq!(snippet.dependencies(), [JobId::new(COMMAND).unwrap()]);
    assert!(agent.action().virtual_terminal.is_none());
    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_keeps_a_disabled_journal_job_with_an_unresolved_profile() {
    use crate::session_journal::automation::{Closing, Keep, Retention};

    let scheduler = std::str::from_utf8(SCHEDULER).unwrap().replacen(
        "<schedule>",
        r#"<sessionJournal>
            <enabled>true</enabled>
            <aiMode>ON_FAILURE</aiMode>
            <aiProfileId>legacy-profile</aiProfileId>
            <keepMode>ONLY_ON_FAILURE</keepMode>
            <retentionDays>37</retentionDays>
        </sessionJournal>
        <schedule>"#,
        1,
    );
    let fixture = Fixture::with_scheduler(&scheduler);
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, omissions) = preview.java_review().unwrap();
    assert!(omissions.iter().any(|entry| {
        entry == "Imported Scheduler journal profiles that require selection in JobScheduler (1)"
    }));
    super::super::queue_restore_at(&fixture.root, preview, RestoreMode::JavaAdditive).unwrap();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let command = scheduler
        .graph()
        .job(&JobId::new(COMMAND).unwrap())
        .unwrap();
    assert!(!command.enabled());
    let journal = command.action().automation_journal.as_ref().unwrap();
    assert!(journal.enabled);
    assert!(journal.capture_input);
    assert_eq!(journal.closing, Closing::Failures);
    assert_eq!(journal.keep, Keep::Failures);
    assert_eq!(journal.retention, Retention::Days { days: 37 });
    assert!(journal.summary_profile.is_none());
    assert_eq!(
        journal.unresolved_summary_profile.as_deref(),
        Some("legacy-profile")
    );
    assert!(journal.java_profile_source.is_none());
}

#[test]
fn java_scheduler_restore_carries_exact_profile_provenance_without_activating_it() {
    use crate::ai_manager::{Configuration, ProfileRef};

    const SOURCE: &str = "45d49b0d-764e-4fc8-9108-b831ba701f88";
    let scheduler = std::str::from_utf8(SCHEDULER).unwrap().replacen(
        "<schedule>",
        &format!(
            "<sessionJournal><enabled>true</enabled><aiMode>ALWAYS</aiMode>\
             <aiProfileId>{SOURCE}</aiProfileId></sessionJournal><schedule>"
        ),
        1,
    );

    for collision in [false, true] {
        let fixture = Fixture::with_sources(&scheduler, Some(GLOBAL_SETTINGS));
        let directory = Directory::open(&fixture.root, true).unwrap();
        let (name, model) = if collision {
            ("Café HTTPS", "fixture-other")
        } else {
            ("Existing local profile", "fixture-local")
        };
        let existing = serde_json::json!({
            "profiles": {"profiles": [{"id": 7, "name": name, "model": model,
                "endpoint": "http://127.0.0.1:11434"}], "default": 7, "last_id": 7}
        });
        directory
            .publish_copy(ai_backup::SETTINGS, &serde_json::to_vec(&existing).unwrap())
            .unwrap();

        fixture.queue();
        fixture.apply();
        let installed = fs::read(fixture.root.join(ai_backup::SETTINGS)).unwrap();
        let jobs = fs::read(fixture.root.join(JOBS)).unwrap();
        fixture.queue();
        fixture.apply();
        assert_eq!(
            fs::read(fixture.root.join(ai_backup::SETTINGS)).unwrap(),
            installed
        );
        assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), jobs);

        let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
        let loaded = store.restore_with_actions::<SshCommand>().unwrap();
        let job = loaded.graph().job(&JobId::new(COMMAND).unwrap()).unwrap();
        assert!(!job.enabled());
        let journal = job.action().automation_journal.as_ref().unwrap();
        assert_eq!(journal.unresolved_summary_profile.as_deref(), Some(SOURCE));
        assert!(journal.summary_profile.is_none());
        let source = journal.java_profile_source.as_ref().unwrap();
        assert_eq!(source.id(), SOURCE);

        let ai = Configuration::load_from(&fixture.root).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&installed).unwrap();
        assert_eq!(saved["profiles"]["default"], 7);
        assert!(ai.java_profile_source(&ProfileRef::Numeric(7)).is_none());
        let first_inserted = ai.java_profile_source(&ProfileRef::Numeric(8)).unwrap();
        assert_eq!(first_inserted == source, !collision);
        assert!(ai.terminal_chat_transport(8).is_err());
        assert!(ai.reviewed_agent_cli_profiles().next().is_none());
    }
}

#[test]
fn java_scheduler_ai_drafts_restore_disabled_with_matching_profile_and_dependencies() {
    use crate::ai_manager::{Configuration, ProfileRef};
    use crate::scheduler_window::{IMPORTED_AI_REVIEW_REQUIRED, agent::Probe};

    const SOURCE: &str = "45d49b0d-764e-4fc8-9108-b831ba701f88";
    const SWARM: &str = "java-a1111111111141118111111111111111";
    let xml = std::str::from_utf8(SCHEDULER).unwrap();
    let agent_start = xml[..xml.find("<name>Agent task</name>").unwrap()]
        .rfind("<job>")
        .unwrap();
    let agent_end = agent_start + xml[agent_start..].find("</job>").unwrap() + "</job>".len();
    let agent = xml[agent_start..agent_end].replace(
        "<aiPrompt>Inspect the fixture</aiPrompt>",
        &format!("<aiPrompt>Inspect the fixture\nSummarize failures</aiPrompt><aiProfileId>{SOURCE}</aiProfileId>"),
    );
    let swarm = agent
        .replace("2bd70fbe-a5e4-4b29-b621-370fcce50b96", "a1111111-1111-4111-8111-111111111111")
        .replace("Agent task", "Swarm task")
        .replace("<type>AI_AGENT</type>", "<type>AI_SWARM</type>")
        .replace("<dependsOn/>", "<dependsOn><jobId>3f9bab16-184b-4e13-bf71-e37331b8843a</jobId></dependsOn><dependencyMode>CHAIN</dependencyMode>")
        .replace("<swarmReadOnly>true</swarmReadOnly>", "<swarmReadOnly>false</swarmReadOnly><swarmMaxParallelism>9</swarmMaxParallelism>");
    let xml = format!("{}{agent}{swarm}{}", &xml[..agent_start], &xml[agent_end..]);
    let fixture = Fixture::with_sources(&xml, Some(GLOBAL_SETTINGS));
    fixture.queue();
    fixture.apply();
    let first = fs::read(fixture.root.join(JOBS)).unwrap();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), first);

    let ai = Configuration::load_from(&fixture.root).unwrap();
    let source = ai.java_profile_source(&ProfileRef::Numeric(1)).unwrap();
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    for (id, probe, parallelism) in [
        (AGENT, Probe::PerFiring, 1),
        (SWARM, Probe::SwarmIterative, 9),
    ] {
        let job = scheduler.graph().job(&JobId::new(id).unwrap()).unwrap();
        assert!(!job.enabled());
        assert!(job.action().agent.is_none());
        assert_eq!(
            job.action().require_executable(),
            Err(IMPORTED_AI_REVIEW_REQUIRED)
        );
        let draft = job.action().imported_ai.as_ref().unwrap();
        assert_eq!(draft.goal, "Inspect the fixture\nSummarize failures");
        assert_eq!(draft.profile_source.as_ref(), Some(source));
        assert_eq!(draft.probe(), probe);
        assert_eq!(draft.parallelism, parallelism);
        if id == SWARM {
            assert_eq!(job.mode(), DependencyMode::Chain);
            assert_eq!(job.dependencies(), [JobId::new(COMMAND).unwrap()]);
        }
    }
    assert!(store.journal().is_empty());

    let bytes = super::super::data::portable_jobs(&first).unwrap();
    let portable = SchedulerStore::decode_backup::<SshCommand>(&bytes).unwrap();
    for id in [AGENT, SWARM] {
        let id = JobId::new(id).unwrap();
        let job = portable.job(&id).unwrap();
        assert_eq!(job.action(), scheduler.graph().job(&id).unwrap().action());
        assert!(SshCommand::validate_definition(job).is_ok());
    }
    let enabled_draft = portable
        .jobs()
        .iter()
        .map(|job| {
            let mut definition = job.definition();
            definition.enabled = job.id().as_str() == AGENT;
            definition.with_action(job.action().clone()).unwrap()
        })
        .collect();
    let forged = SchedulerStore::encode_backup(&JobGraph::new(enabled_draft).unwrap()).unwrap();
    let mut snapshot = super::super::Snapshot::new();
    snapshot.insert(JOBS.into(), zeroize::Zeroizing::new(bytes));
    assert!(super::super::data::validate(&snapshot).is_ok());
    snapshot.insert(JOBS.into(), zeroize::Zeroizing::new(forged));
    assert!(super::super::data::validate(&snapshot).is_err());
}

#[test]
fn java_scheduler_restore_keeps_remote_paths_and_requires_deletion_confirmation() {
    use std::fmt::Write as _;

    use serde_json::json;

    let actions = [
        (
            "SFTP_MKDIR",
            "<remotePath>/work/新しい folder</remotePath>",
            Some(json!({"kind":"mkdir", "path":"/work/新しい folder"})),
        ),
        (
            "SFTP_RENAME",
            "<remoteSourcePath>/work/old</remoteSourcePath><newName>/work/new</newName>",
            Some(json!({"kind":"rename", "from":"/work/old", "to":"/work/new"})),
        ),
        (
            "SFTP_CHMOD",
            "<remotePath>/work/regular</remotePath><permissions>0640</permissions>",
            Some(json!({"kind":"chmod", "path":"/work/regular", "mode":0o640})),
        ),
        (
            "SFTP_CHMOD",
            "<remotePath>/work/symbolic</remotePath><permissions>u+rw,o-w</permissions>",
            Some(json!({"kind":"chmod_symbolic", "path":"/work/symbolic", "mode":"u+rw,o-w"})),
        ),
        (
            "SFTP_CHOWN",
            "<remotePath>/work/owned</remotePath><owner>1200</owner><group>1400</group>",
            Some(json!({"kind":"chown", "path":"/work/owned", "uid":1200, "gid":1400})),
        ),
        (
            "SFTP_COPY_REMOTE",
            "<remoteSourcePath>/work/orig</remoteSourcePath><remoteDestinationPath>/work/copy</remoteDestinationPath>",
            Some(json!({"kind":"copy_remote", "from":"/work/orig", "to":"/work/copy"})),
        ),
        (
            "SFTP_DELETE",
            "<remotePath>/work/retired</remotePath>",
            Some(
                json!({"kind":"delete", "path":"/work/retired", "confirmed_path":"", "recursive":true}),
            ),
        ),
        (
            "SFTP_MKDIR",
            "<remotePath>/work/sudo</remotePath><useSudo>true</useSudo><sudoStagingEnabled>true</sudoStagingEnabled>",
            None,
        ),
        (
            "SFTP_DELETE",
            "<remotePath>/work/staged</remotePath><sudoStagingEnabled>true</sudoStagingEnabled>",
            None,
        ),
        ("SFTP_MKDIR", "<remotePath>relative/path</remotePath>", None),
        (
            "SFTP_RENAME",
            "<remoteSourcePath>/work/old</remoteSourcePath><newName>child</newName>",
            None,
        ),
        (
            "SFTP_CHMOD",
            "<remotePath>/work/umask</remotePath><permissions>+x</permissions>",
            None,
        ),
        (
            "SFTP_CHOWN",
            "<remotePath>/work/named</remotePath><owner>fixture</owner>",
            None,
        ),
        (
            "SFTP_ARCHIVE",
            "<archivePath>/work/generated.zip</archivePath>",
            None,
        ),
        ("UNKNOWN_ACTION", "", None),
    ];
    let mut xml = String::from("<jobScheduler><jobs>");
    for (index, (kind, fields, _)) in actions.iter().enumerate() {
        write!(
            xml,
            "<job><id>00000000-0000-4000-8000-{index:012x}</id><name>Remote job {index}</name>\
             <enabled>true</enabled><connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>\
             <workingDirectory>/unused</workingDirectory><journalDetailMode>FULL</journalDetailMode>\
             <action><type>{kind}</type>{fields}</action></job>"
        ).unwrap();
    }
    xml.push_str("</jobs></jobScheduler>");
    let fixture = Fixture::with_scheduler(&xml);
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, notes) = preview.java_review().unwrap();
    for note in [
        "Imported recursive Scheduler deletions require exact-path confirmation in JobScheduler (1)",
        "Imported Scheduler copies use guarded SFTP at a new destination with native permissions (1)",
        "Java Scheduler jobs with unsupported execution options omitted (4)",
        "unsupported Java Scheduler actions omitted (1)",
    ] {
        assert!(notes.iter().any(|actual| actual == note), "{notes:?}");
    }
    fixture.queue();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let restored = scheduler.graph();
    assert_eq!(restored.jobs().len(), 7);
    for (index, (_, _, expected)) in actions.iter().enumerate() {
        let id = JobId::new(format!("java-00000000000040008000{index:012x}")).unwrap();
        let Some(expected) = expected else {
            assert!(restored.job(&id).is_none());
            continue;
        };
        let job = restored.job(&id).unwrap();
        let action = job.action();
        assert!(!job.enabled());
        assert_eq!(
            action.connection.as_ref().unwrap().target.host,
            "primary.example"
        );
        assert!(action.working_directory.is_none());
        assert!(action.command.is_empty());
        assert_eq!(&serde_json::to_value(&action.remote).unwrap(), expected);
    }
    let deletion = restored
        .jobs()
        .iter()
        .find(|job| matches!(&job.action().remote, Some(RemoteAction::Delete { .. })))
        .unwrap();
    let confirmation = "Repeat the exact remote path to authorize deletion";
    assert_eq!(deletion.action().require_executable(), Err(confirmation));
    assert!(matches!(deletion.action().remote_action(), Err(error) if error == confirmation));
    assert!(SshCommand::validate_definition(deletion).is_ok());
    let enabled = deletion
        .definition()
        .with_action(deletion.action().clone())
        .unwrap()
        .with_enabled(true);
    assert!(SshCommand::validate_definition(&enabled).is_err());

    let mut forged = deletion.action().clone();
    let Some(RemoteAction::Delete {
        path,
        confirmed_path,
        ..
    }) = &mut forged.remote
    else {
        unreachable!()
    };
    *confirmed_path = path.as_str().to_owned();
    assert!(forged.require_executable().is_ok());
    let forged = deletion.definition().with_action(forged).unwrap();
    let forged = SchedulerStore::encode_backup(&JobGraph::new(vec![forged]).unwrap()).unwrap();
    assert!(java_scheduler::decode(&forged).is_err());

    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_JOBS_COMMITTING;
    Directory::open(&fixture.root, false)
        .unwrap()
        .publish_copy(PENDING, &committing)
        .unwrap();
    fixture.apply();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_keeps_transfer_intent_inert_and_replays_it() {
    use std::fmt::Write as _;

    let actions = [
        ("SFTP_UPLOAD", "", "upload"),
        ("SFTP_DOWNLOAD", "", "download"),
        ("SFTP_SYNC", "", "sync_upload"),
        (
            "SFTP_SYNC",
            "<syncDirection>DOWNLOAD</syncDirection>",
            "sync_download",
        ),
        ("SFTP_UPLOAD", "<useSudo>true</useSudo>", "upload"),
        (
            "SFTP_DOWNLOAD",
            "<sudoStagingEnabled>true</sudoStagingEnabled>",
            "download",
        ),
    ];
    let mut xml = String::from("<jobScheduler><jobs>");
    for (index, (kind, options, _)) in actions.iter().enumerate() {
        write!(
            xml,
            r"<job><id>00000000-0000-4000-9000-{index:012x}</id><name>Transfer {index}</name>
             <enabled>true</enabled><connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>
             <action><type>{kind}</type>{options}<localPath>C:\Java Koppie\東京</localPath>
             <remotePath>/projects/東京</remotePath></action></job>"
        ).unwrap();
    }
    xml.push_str("</jobs></jobScheduler>");
    let fixture = Fixture::with_scheduler(&xml);
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, notes) = preview.java_review().unwrap();
    assert!(notes.iter().any(|note| note.contains("Imported Scheduler SFTP transfers require a local path and transfer review in JobScheduler (6)")));
    fixture.queue();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    assert_eq!(scheduler.graph().jobs().len(), actions.len());
    for (index, (_, _, kind)) in actions.iter().enumerate() {
        let id = JobId::new(format!("java-00000000000040009000{index:012x}")).unwrap();
        let job = scheduler.graph().job(&id).unwrap();
        assert!(!job.enabled());
        let action = job.action();
        assert_eq!(
            action.targets().next().unwrap().target.host,
            "primary.example"
        );
        assert!(action.remote.is_none());
        assert!(action.require_executable().is_err());
        assert!(action.command_for_exec(None).is_err());
        assert!(SshCommand::validate_definition(job).is_ok());
        let enabled = job
            .definition()
            .with_action(job.action().clone())
            .unwrap()
            .with_enabled(true);
        assert!(SshCommand::validate_definition(&enabled).is_err());
        let exported = serde_json::to_value(action).unwrap();
        assert_eq!(
            exported["imported_transfer"],
            serde_json::json!({
                "kind": kind,
                "local_hint": r"C:\Java Koppie\東京",
                "remote": "/projects/東京",
            })
        );
    }

    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_JOBS_COMMITTING;
    Directory::open(&fixture.root, false)
        .unwrap()
        .publish_copy(PENDING, &committing)
        .unwrap();
    fixture.apply();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_binds_sudo_staging_drafts_and_expands_dependencies() {
    use std::fmt::Write as _;

    use crate::scheduler_window::{SudoMode, imported_transfer};

    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SYNC: &str = "55be1473-abc2-450d-8000-000000000003";
    const AFTER: &str = "55be1473-abc2-450d-8000-000000000004";
    const SECRET: &str = "fixture-opaque-sudo-password";
    let primary = format!("<connectionId>{PRIMARY}</connectionId>");
    let mut xml = String::from("<jobScheduler><jobs>");
    for (index, (kind, direction)) in [
        ("SFTP_UPLOAD", "UPLOAD"),
        ("SFTP_DOWNLOAD", "DOWNLOAD"),
        ("SFTP_SYNC", "UPLOAD"),
        ("SFTP_SYNC", "DOWNLOAD"),
    ]
    .into_iter()
    .enumerate()
    {
        let group = if index == 3 {
            "<targetGroupNames><groupName>Ops/Hosts</groupName></targetGroupNames>"
        } else {
            ""
        };
        write!(
            xml,
            r"<job><id>55be1473-abc2-450d-8000-{index:012x}</id><name>Stage {index}</name>
               <enabled>true</enabled>{primary}{group}
               <schedule><enabled>true</enabled><fixedTimes><time>08:15</time></fixedTimes></schedule>
               <action><type>{kind}</type><syncDirection>{direction}</syncDirection>
               <useSudo>true</useSudo><sudoStagingEnabled>true</sudoStagingEnabled>
               <localPath>C:\Java\東京</localPath><remotePath>relative/東京</remotePath></action></job>"
        )
        .unwrap();
    }
    write!(
        xml,
        "<job><id>{AFTER}</id><name>After</name>{primary}
         <dependsOn><jobId>{SYNC}</jobId></dependsOn><dependencyMode>CHAIN</dependencyMode>
         <action><type>COMMAND</type><command>printf after</command></action></job>
         </jobs><sudoCredentials><credential><encryptedPassword>{SECRET}</encryptedPassword>
         </credential></sudoCredentials></jobScheduler>"
    )
    .unwrap();

    let fixture = Fixture::with_connections(&xml, GROUP_CONNECTIONS.as_bytes(), None);
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, notes) = preview.java_review().unwrap();
    assert!(
        notes.iter().any(|note| {
            note.starts_with(
                "Imported Scheduler sudo staging transfers are disabled per saved SSH route",
            ) && note.ends_with("(4)")
        }),
        "{notes:?}"
    );
    fixture.queue();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let graph = scheduler.graph();
    assert_eq!(graph.jobs().len(), 6);
    let mut split = Vec::new();
    for job in graph.jobs().iter().filter(|job| job.name() != "After") {
        let action = job.action();
        let Some(imported_transfer::Draft::StagedSftp(draft)) = action.imported_transfer.as_deref()
        else {
            panic!("missing staged transfer intent");
        };
        let expected = match job.name() {
            "Stage 0" => imported_transfer::Kind::Upload,
            "Stage 1" => imported_transfer::Kind::Download,
            "Stage 2" => imported_transfer::Kind::SyncUpload,
            _ => {
                assert!(job.id().as_str().starts_with("java-staging-"));
                split.push(job.id().clone());
                imported_transfer::Kind::SyncDownload
            }
        };
        assert!(draft.settings.kind == expected);
        assert_eq!(draft.settings.local_hint, r"C:\Java\東京");
        assert_eq!(draft.settings.remote_hint, "relative/東京");
        assert!(!job.enabled());
        assert!(job.schedule().unwrap().enabled());
        assert_eq!(action.sudo, SudoMode::Disabled);
        assert!(action.remote.is_none());
        assert_eq!(action.connection.as_ref(), Some(&draft.route));
        assert!(action.additional_connections.is_empty());
        assert!(action.target_groups.is_none());
        assert!(action.require_executable().is_err());
        assert!(SshCommand::validate_definition(job).is_ok());
        let mut changed = action.clone();
        changed.connection.as_mut().unwrap().target.host = "changed.example".into();
        assert!(changed.validated().is_err());
    }
    assert_eq!(split.len(), 2);
    let after = graph
        .job(&JobId::new(format!("java-{}", AFTER.replace('-', ""))).unwrap())
        .unwrap();
    assert_eq!(after.mode(), DependencyMode::Chain);
    assert_eq!(after.dependencies(), split);

    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    assert!(!String::from_utf8_lossy(&installed).contains(SECRET));
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_keeps_rsync_routes_dependencies_and_options_inert() {
    use serde_json::json;

    let xml = r#"<jobScheduler><jobs>
      <job><id>00000000-0000-4000-9001-000000000001</id><name>Before rsync</name>
        <connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>
        <action><type>COMMAND</type><command>printf prepare</command></action></job>
      <job><id>00000000-0000-4000-9001-000000000002</id><name>Group rsync</name><enabled>true</enabled>
        <connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>
        <targetGroupNames><groupName>Ops/Hosts</groupName></targetGroupNames>
        <dependsOn><jobId>00000000-0000-4000-9001-000000000001</jobId></dependsOn>
        <action><type>RSYNC_SYNC</type><rsyncDirection>DOWNLOAD</rsyncDirection>
          <rsyncSources><path> /work/first </path><path>/work/second</path></rsyncSources>
          <rsyncTargetRoot>C:\Java copy</rsyncTargetRoot></action></job>
      <job><id>00000000-0000-4000-9001-000000000003</id><name>After rsync</name>
        <connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>
        <dependsOn><jobId>00000000-0000-4000-9001-000000000002</jobId></dependsOn>
        <action><type>RSYNC_SYNC</type><rsyncSources><path>/old/tree/</path></rsyncSources>
          <rsyncTargetRoot>/work/merged</rsyncTargetRoot><rsyncDeleteEnabled>true</rsyncDeleteEnabled>
          <useSudo>true</useSudo><encryptedArchivePassword>fixture-unused-secret</encryptedArchivePassword>
        </action></job>
      </jobs></jobScheduler>"#;
    let globals = b"<globalSettings><jobSchedulerRsyncBinaryPath>/java-only/rsync</jobSchedulerRsyncBinaryPath></globalSettings>";
    let connections = GROUP_CONNECTIONS
        .replace("<name>Primary</name>", "<name>  Primary /  Büro__  </name>")
        .replace("<name>Secondary</name>", "");
    let fixture = Fixture::with_connections(xml, connections.as_bytes(), Some(globals));
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, notes) = preview.java_review().unwrap();
    assert!(notes.iter().any(|note| note.contains("Imported Scheduler rsync drafts require a native executable, destinations, and fresh transfer review in JobScheduler (2)")));
    fixture.queue();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let get = |index| JobId::new(format!("java-00000000000040009001{index:012x}")).unwrap();
    let first = get(1);
    let group_id = get(2);
    let grouped = scheduler.graph().job(&group_id).unwrap();
    let upload = scheduler.graph().job(&get(3)).unwrap();
    assert_eq!(grouped.dependencies(), &[first]);
    assert_eq!(upload.dependencies(), &[group_id]);
    let mapped = grouped.action();
    assert_eq!(
        mapped
            .targets()
            .map(|route| route.target.host.as_str())
            .collect::<Vec<_>>(),
        ["primary.example", "secondary.example"]
    );
    assert_eq!(
        mapped.target_groups.as_ref().unwrap().groups,
        [crate::scheduler_window::ConnectionGroup::from("Ops/Hosts")]
    );
    let mut routes = mapped.targets();
    let primary_id = routes.next().unwrap().id.to_string();
    let secondary_id = routes.next().unwrap().id.to_string();
    for (job, draft) in [
        (
            grouped,
            json!({"direction":"download", "java_sources":["/work/first","/work/second"], "java_target_root":r"C:\Java copy", "java_delete":false, "java_sudo":false,
                "java_target_folders": { (primary_id): "_Primary_B_ro_-b04d413f", (secondary_id): "other_secondary.example-9c0719d8" }}),
        ),
        (
            upload,
            json!({"direction":"upload", "java_sources":["/old/tree/"], "java_target_root":"/work/merged", "java_delete":true, "java_sudo":true}),
        ),
    ] {
        assert!(!job.enabled());
        assert!(job.action().remote.is_none());
        assert!(job.action().require_executable().is_err());
        assert!(SshCommand::validate_definition(job).is_ok());
        assert_eq!(
            serde_json::to_value(job.action()).unwrap()["imported_transfer"],
            draft
        );
    }
    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    let serialized = std::str::from_utf8(&installed).unwrap();
    assert!(
        !serialized.contains("fixture-unused-secret") && !serialized.contains("/java-only/rsync")
    );
    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_JOBS_COMMITTING;
    Directory::open(&fixture.root, false)
        .unwrap()
        .publish_copy(PENDING, &committing)
        .unwrap();
    fixture.apply();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_keeps_archive_drafts_inert_without_vault_or_local_path_authority() {
    use kortty_server_archive::protocol::ArchiveMemberLayout;
    use serde_json::json;
    use std::fmt::Write as _;

    const JAVA_SECRET: &str = "fixture-opaque-java-archive-envelope";
    const JAVA_SUDO_SECRET: &str = "fixture-opaque-java-sudo-envelope";
    let mut xml = String::from("<jobScheduler><jobs>");
    for (index, (format, options, targets, suffix)) in [
        ("ZIP", "", "", "zip"),
        ("ZIP_PASSWORD", r"<archiveDownloadAfterCreate>true</archiveDownloadAfterCreate><archiveDownloadLocalPath>C:\Java only\copy.zip</archiveDownloadLocalPath>", "", "zip"),
        ("TAR", "", "", "tar"),
        ("TAR_BZ2", "", "", "tar.bz2"),
        ("ZIP", "<useSudo>true</useSudo>", "", "zip"),
        ("ZIP", "<useSudo>false</useSudo><sudoStagingEnabled>true</sudoStagingEnabled>", "", "zip"),
    ].into_iter().enumerate() {
        let second_source = if matches!(index, 0 | 3) { "/etc/東京" } else { "/etc/hosts" };
        write!(xml, "<job><id>00000000-0000-4000-a000-{index:012x}</id><name>Archive {index}</name>\
            <enabled>true</enabled><connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>{targets}\
            <action><type>SFTP_ARCHIVE</type><archiveFormat>{format}</archiveFormat>\
            <archiveSources><path> /srv/東京 </path><path>{second_source}</path></archiveSources>\
            <archiveExcludes><pattern> *.log </pattern><pattern>var/*/tmp</pattern></archiveExcludes>\
            <archivePath>/copies/output-{index}.{suffix}</archivePath><archiveCompressionLevel>7</archiveCompressionLevel>\
            <encryptedArchivePassword>{JAVA_SECRET}</encryptedArchivePassword>{options}</action></job>").unwrap();
    }
    write!(xml, "</jobs><sudoCredentials><sudoCredential><scope>SERVER</scope>\
        <serverConnectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</serverConnectionId>\
        <encryptedPassword>{JAVA_SUDO_SECRET}</encryptedPassword></sudoCredential></sudoCredentials>\
        </jobScheduler>").unwrap();
    let fixture = Fixture::with_scheduler(&xml);
    let preview = super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
    let (_, _, notes) = preview.java_review().unwrap();
    for expected in [
        "Imported Scheduler archives require archive and saved route review in JobScheduler (6)",
        "Imported Scheduler password ZIPs require a Rust vault selection. Java archive secrets were omitted (1)",
        "Imported privileged Scheduler archives require a fresh sudo review and the Rust passwordless helper. Java sudo passwords were omitted (1)",
    ] {
        assert!(notes.iter().any(|note| note == expected), "{notes:?}");
    }
    fixture.queue();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    let pending = String::from_utf8_lossy(&pending);
    assert!(!pending.contains(JAVA_SECRET));
    assert!(!pending.contains(JAVA_SUDO_SECRET));
    fixture.apply();

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    assert_eq!(scheduler.graph().jobs().len(), 6);
    for (index, format) in ["zip", "zip_password", "tar", "tar_bz2", "zip", "zip"]
        .into_iter()
        .enumerate()
    {
        let id = JobId::new(format!("java-0000000000004000a000{index:012x}")).unwrap();
        let job = scheduler.graph().job(&id).unwrap();
        let action = job.action();
        let Some(RemoteAction::ImportedArchive(archive)) = &action.remote else {
            panic!("expected inert archive")
        };
        assert_eq!(archive.route, *action.connection.as_ref().unwrap());
        assert_eq!(archive.route.target.host, "primary.example");
        assert_eq!(archive.route.jump.as_ref().unwrap().host, "jump.example");
        let settings = serde_json::to_value(&archive.settings).unwrap();
        let second_source = if matches!(index, 0 | 3) {
            "/etc/東京"
        } else {
            "/etc/hosts"
        };
        assert_eq!(settings["sources"], json!(["/srv/東京", second_source]));
        assert_eq!(
            archive.settings.member_layout(),
            ArchiveMemberLayout::ServerPath
        );
        assert!(settings.get("reuse_output").is_none());
        assert_eq!(settings["java_excludes"], json!(["*.log", "var/*/tmp"]));
        assert_eq!(settings["format"], format);
        assert_eq!(settings["level"], if format == "tar" { 6 } else { 7 });
        assert_eq!(archive.settings.java_sudo, index == 4);
        assert_eq!(
            settings.get("java_sudo"),
            (index == 4).then_some(&json!(true))
        );
        assert_eq!(action.sudo, crate::scheduler_window::SudoMode::Disabled);
        assert_eq!(
            archive.settings.local_hint.as_deref(),
            (index == 1).then_some(r"C:\Java only\copy.zip")
        );
        assert!(!job.enabled());
        assert!(action.require_executable().is_err() && action.remote_action().is_err());
        assert!(SshCommand::validate_definition(job).is_ok());
        let enabled = job
            .definition()
            .with_action(action.clone())
            .unwrap()
            .with_enabled(true);
        assert!(SshCommand::validate_definition(&enabled).is_err());
        let mut changed = action.clone();
        changed.connection.as_mut().unwrap().target.port = 2223;
        assert!(changed.validated().is_err());
    }
    drop(store);
    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    assert!(!String::from_utf8_lossy(&installed).contains(JAVA_SECRET));
    assert!(!String::from_utf8_lossy(&installed).contains(JAVA_SUDO_SECRET));
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_splits_archive_routes_and_preserves_all_prerequisites() {
    use std::collections::BTreeMap;

    const BEFORE: &str = "45be1473-abc2-450d-8000-000000000001";
    const ARCHIVE: &str = "45be1473-abc2-450d-8000-000000000002";
    const AFTER: &str = "45be1473-abc2-450d-8000-000000000003";
    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SECONDARY: &str = "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e";
    const ORIGINAL_DOWNLOAD: &str = "C:/Java-only/archive.zip";
    const JAVA_SECRET: &str = "fixture-opaque-archive-password";

    let primary = format!("<connectionId>{PRIMARY}</connectionId>");
    let before = JobId::new(format!("java-{}", BEFORE.replace('-', ""))).unwrap();
    let after = JobId::new(format!("java-{}", AFTER.replace('-', ""))).unwrap();
    let connections = GROUP_CONNECTIONS.replace(
        "<group>Ops/Hosts</group>",
        "<group>Ops/Hosts</group><jumpServer><enabled>true</enabled><host>jump.example</host>\
            <port>2200</port><username>jump-user</username></jumpServer>",
    );
    let mut known_routes = None;
    for (targets, use_sudo) in [
        (
            format!(
                "<targetConnectionIds>{primary}<connectionId>{SECONDARY}</connectionId></targetConnectionIds>"
            ),
            false,
        ),
        (
            format!(
                "{primary}<targetGroupNames><groupName>Ops/Hosts</groupName></targetGroupNames>"
            ),
            true,
        ),
    ] {
        let xml = format!(
            "<jobScheduler><jobs>
            <job><id>{BEFORE}</id><name>Before</name>{primary}
            <action><type>COMMAND</type><command>printf before</command></action></job>
            <job><id>{ARCHIVE}</id><name>Grouped archive</name>{targets}<enabled>true</enabled>
            <dependsOn><jobId>{BEFORE}</jobId></dependsOn><dependencyMaxAgeMinutes>30</dependencyMaxAgeMinutes>
            <schedule><enabled>true</enabled><fixedTimes><time>08:15</time></fixedTimes></schedule>
            <action><type>SFTP_ARCHIVE</type><archiveFormat>ZIP_PASSWORD</archiveFormat>
            <useSudo>{use_sudo}</useSudo>
            <archiveSources><path>/etc/hosts</path></archiveSources>
            <archivePath>/copies/archive.zip</archivePath>
            <archiveDownloadAfterCreate>true</archiveDownloadAfterCreate>
            <archiveDownloadLocalPath>{ORIGINAL_DOWNLOAD}</archiveDownloadLocalPath>
            <encryptedArchivePassword>{JAVA_SECRET}</encryptedArchivePassword></action></job>
            <job><id>{AFTER}</id><name>After</name>{primary}
            <dependsOn><jobId>{ARCHIVE}</jobId></dependsOn><dependencyMode>CHAIN</dependencyMode>
            <action><type>COMMAND</type><command>printf after</command></action></job>
            </jobs></jobScheduler>"
        );
        let fixture = Fixture::with_connections(&xml, connections.as_bytes(), None);
        let preview =
            super::super::inspect_java_password(&fixture.archive, &fixture.password).unwrap();
        let (_, _, notes) = preview.java_review().unwrap();
        assert!(notes.iter().any(|note| {
            note == "Multi-target Scheduler archives are restored as disabled jobs per saved route. Choose a separate destination for each local copy (1)"
        }), "{notes:?}");
        fixture.queue();
        assert!(
            !String::from_utf8_lossy(&fs::read(fixture.root.join(PENDING)).unwrap())
                .contains(JAVA_SECRET)
        );
        fixture.apply();

        let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
        let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
        let graph = scheduler.graph();
        assert_eq!(graph.jobs().len(), 4);
        let mut routes = BTreeMap::new();
        let mut archives = Vec::new();
        for job in graph.jobs() {
            let action = job.action();
            let Some(RemoteAction::ImportedArchive(draft)) = &action.remote else {
                continue;
            };
            assert!(!job.enabled());
            assert_eq!(job.dependencies(), std::slice::from_ref(&before));
            assert_eq!(job.max_age_minutes().map(|minutes| minutes.get()), Some(30));
            assert!(job.schedule().is_some());
            assert_eq!(action.connection.as_ref(), Some(&draft.route));
            assert!(action.additional_connections.is_empty());
            assert!(action.target_groups.is_none());
            assert!(action.require_executable().is_err());
            assert!(SshCommand::validate_definition(job).is_ok());
            assert!(job.id().as_str().starts_with("java-archive-"));
            assert_eq!(draft.settings.target.as_str(), "/copies/archive.zip");
            assert_eq!(draft.settings.java_sudo, use_sudo);
            assert_eq!(
                draft.settings.local_hint.as_deref(),
                Some(ORIGINAL_DOWNLOAD)
            );
            if draft.route.target.host == "primary.example" {
                assert_eq!(draft.route.jump.as_ref().unwrap().host, "jump.example");
            } else {
                assert_eq!(draft.route.target.host, "secondary.example");
                assert!(draft.route.jump.is_none());
            }
            routes.insert(draft.route.target.host.clone(), job.id().clone());
            archives.push(job.id().clone());
        }
        assert_eq!(routes.len(), 2);
        let dependent = graph.job(&after).unwrap();
        assert_eq!(dependent.mode(), DependencyMode::Chain);
        assert_eq!(dependent.dependencies(), archives);
        if let Some(previous) = known_routes.replace(routes.clone()) {
            assert_eq!(previous, routes);
        }
        drop(store);
        let installed = fs::read(fixture.root.join(JOBS)).unwrap();
        assert!(!String::from_utf8_lossy(&installed).contains(JAVA_SECRET));
        fixture.queue();
        fixture.apply();
        assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
    }
}

#[test]
fn java_scheduler_restore_omits_archive_fanout_when_any_member_is_unavailable() {
    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SECONDARY: &str = "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e";
    let primary = format!("<connectionId>{PRIMARY}</connectionId>");
    let connections = GROUP_CONNECTIONS.replace(
        "<name>Secondary</name><host>secondary.example</host><username>other</username>",
        "<name>Unavailable target</name><protocol>LOCAL_SHELL</protocol>",
    );
    for (targets, reason) in [
        (
            format!(
                "<targetConnectionIds>{primary}<connectionId>{SECONDARY}</connectionId></targetConnectionIds>"
            ),
            java_scheduler::Omission::InvalidTargets,
        ),
        (
            format!(
                "{primary}<targetGroupNames><groupName>Ops/Hosts</groupName></targetGroupNames>"
            ),
            java_scheduler::Omission::GroupTargets,
        ),
    ] {
        let xml = format!(
            "<jobScheduler><jobs>
            <job><id>45be1473-abc2-450d-8000-000000000001</id><name>Independent job</name>{primary}
            <action><type>COMMAND</type><command>printf independent</command></action></job>
            <job><id>45be1473-abc2-450d-8000-000000000002</id><name>Grouped archive</name>{targets}
            <action><type>SFTP_ARCHIVE</type><archiveSources><path>/etc/hosts</path></archiveSources>
            <archivePath>/copies/archive.zip</archivePath></action></job>
            <job><id>45be1473-abc2-450d-8000-000000000003</id><name>After</name>{primary}
            <dependsOn><jobId>45be1473-abc2-450d-8000-000000000002</jobId></dependsOn>
            <action><type>COMMAND</type><command>printf after</command></action></job>
            </jobs></jobScheduler>"
        );
        let fixture = Fixture::with_connections(&xml, connections.as_bytes(), None);
        let queued = fixture.queue();
        for omitted in [reason, java_scheduler::Omission::InvalidDependency] {
            let expected = format!("{} (1)", omitted.description());
            assert!(queued.scheduler_notes.contains(&expected), "{expected}");
        }
        fixture.apply();
        let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
        let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
        assert_eq!(scheduler.graph().jobs().len(), 1);
        assert_eq!(scheduler.graph().jobs()[0].name(), "Independent job");
    }
}

#[test]
fn java_scheduler_restore_keeps_privileged_actions_inert_with_exact_routes_and_dependencies() {
    use std::fmt::Write as _;

    use serde_json::json;

    use crate::scheduler_window::{JournalDetail, SudoMode, imported_privileged};

    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SECONDARY: &str = "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e";
    let actions = [
        (
            "COMMAND",
            "<command>printf '%s' 'root &amp; data'</command>",
            json!({"kind": "command", "text": "printf '%s' 'root & data'",
                "options": {"working_directory": "/srv/project files"}}),
        ),
        (
            "SNIPPET_SCRIPT",
            "<snippetId>3ee66545-2eaf-4ac7-b352-d9a1a51d75ed</snippetId>\
             <snippetArguments><argument>space &amp; 東京</argument></snippetArguments>\
             <ptyEnabled>true</ptyEnabled><ptyColumns>132</ptyColumns>",
            json!({"kind": "snippet"}),
        ),
        (
            "SFTP_MKDIR",
            "<remotePath>relative/directory</remotePath>",
            json!({"operation": "mkdir", "path": "relative/directory"}),
        ),
        (
            "SFTP_RENAME",
            "<remoteSourcePath>/srv/old</remoteSourcePath><newName>new name</newName>",
            json!({"operation": "rename", "from": "/srv/old", "to": "new name"}),
        ),
        (
            "SFTP_CHMOD",
            "<remotePath>/srv/tool</remotePath><permissions>u=rwX,g=rX,o=</permissions>",
            json!({"operation": "chmod", "path": "/srv/tool", "permissions": "u=rwX,g=rX,o="}),
        ),
        (
            "SFTP_CHOWN",
            "<remotePath>/srv/input</remotePath><owner>backup-user</owner><group>ops</group>",
            json!({"operation": "chown", "path": "/srv/input", "owner": "backup-user", "group": "ops"}),
        ),
        (
            "SFTP_COPY_REMOTE",
            "<remoteSourcePath>/srv/from</remoteSourcePath><remoteDestinationPath>/srv/to</remoteDestinationPath>",
            json!({"operation": "copy", "from": "/srv/from", "to": "/srv/to"}),
        ),
        (
            "SFTP_DELETE",
            "<remotePath>/srv/retired</remotePath>",
            json!({"operation": "delete", "path": "/srv/retired"}),
        ),
    ];
    let uuid = |index| format!("00000000-0000-4000-8000-{index:012x}");
    let mut xml = String::from("<jobScheduler><jobs>");
    for (index, (kind, fields, _)) in actions.iter().enumerate() {
        let secondary = if matches!(index, 0 | 1 | 7) {
            format!("<connectionId>{SECONDARY}</connectionId>")
        } else {
            String::new()
        };
        write!(
            xml,
            "<job><id>{}</id><name>{kind}</name><enabled>true</enabled>\
             <targetConnectionIds><connectionId>{PRIMARY}</connectionId>{secondary}</targetConnectionIds>\
             <workingDirectory>/srv/project files</workingDirectory><journalDetailMode>FULL</journalDetailMode>\
             <action><type>{kind}</type><useSudo>true</useSudo>{fields}</action></job>",
            uuid(index),
        )
        .unwrap();
    }
    let successor = actions.len();
    write!(
        xml,
        "<job><id>{}</id><name>After all deletions</name><connectionId>{PRIMARY}</connectionId>\
         <dependsOn><jobId>{}</jobId></dependsOn><dependencyMode>CHAIN</dependencyMode>\
         <action><type>COMMAND</type><command>true</command></action></job></jobs></jobScheduler>",
        uuid(successor),
        uuid(7),
    )
    .unwrap();
    let fixture = Fixture::with_scheduler(&xml);
    fixture.queue();
    fixture.apply();
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let graph = scheduler.graph();
    assert_eq!(graph.jobs().len(), actions.len() + 3);
    let mut deletes = Vec::new();
    for (index, (kind, _, expected)) in actions.iter().enumerate() {
        let jobs: Vec<_> = graph
            .jobs()
            .iter()
            .filter(|job| {
                job.name() == *kind || job.name().starts_with(&format!("{kind} (target "))
            })
            .collect();
        assert_eq!(jobs.len(), if matches!(index, 1 | 7) { 2 } else { 1 });
        for (target_index, job) in jobs.into_iter().enumerate() {
            let action = job.action();
            assert!(!job.enabled());
            assert_eq!(action.sudo, SudoMode::Disabled);
            assert!(action.command.is_empty());
            assert!(action.snippet.is_none());
            assert!(action.working_directory.is_none());
            assert!(action.virtual_terminal.is_none());
            assert!(action.remote.is_none());
            assert_eq!(
                action.require_executable(),
                Err(imported_privileged::REVIEW_REQUIRED)
            );
            let enabled = job
                .definition()
                .with_action(action.clone())
                .unwrap()
                .with_enabled(true);
            assert!(SshCommand::validate_definition(&enabled).is_err());
            let draft = action.imported_privileged.as_ref().unwrap();
            assert!(draft.routes.iter().eq(action.targets()));
            assert_eq!(
                draft.routes[0].target.host,
                ["primary.example", "secondary.example"][target_index]
            );
            let settings = serde_json::to_value(&draft.settings).unwrap();
            match &draft.settings {
                imported_privileged::Settings::Command { .. } => {
                    assert_eq!(&settings, expected);
                    assert_eq!(draft.routes[1].target.host, "secondary.example");
                    assert_eq!(action.journal_detail, JournalDetail::Full);
                }
                imported_privileged::Settings::Snippet { link, options } => {
                    assert_eq!(settings["kind"], expected["kind"]);
                    assert_eq!(link.arguments, ["space & 東京"]);
                    assert_eq!(options.virtual_terminal.unwrap().columns, 132);
                    assert_eq!(
                        options.working_directory.as_deref(),
                        Some("/srv/project files")
                    );
                    assert_eq!(action.journal_detail, JournalDetail::Full);
                    assert!(job.id().as_str().starts_with("java-privileged-"));
                }
                imported_privileged::Settings::File { action: file } => {
                    assert_eq!(&settings["action"], expected);
                    assert_eq!(action.journal_detail, JournalDetail::Off);
                    if matches!(file, imported_privileged::FileAction::Delete { .. }) {
                        assert!(job.id().as_str().starts_with("java-privileged-"));
                        deletes.push(job.id().clone());
                    }
                }
            }
        }
    }
    let dependent = graph
        .jobs()
        .iter()
        .find(|job| job.name() == "After all deletions")
        .unwrap();
    assert!(!dependent.enabled());
    assert_eq!(dependent.mode(), DependencyMode::Chain);
    assert_eq!(dependent.dependencies(), deletes);
    assert!(store.journal().is_empty());
    drop(store);

    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_rebinds_exact_targets_and_preserves_local_history_on_replay() {
    let fixture = Fixture::new();
    let (local, existing_run) = fixture.seed_local("local");
    let old_jobs = fs::read(fixture.root.join(JOBS)).unwrap();
    fixture.queue();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), old_jobs);
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    assert!(
        !pending
            .windows(b"synthetic-sudo-ciphertext".len())
            .any(|bytes| bytes == b"synthetic-sudo-ciphertext")
    );
    fixture.apply();

    let routes = Store::open(fixture.root.join(CONNECTIONS)).unwrap();
    let find_route = |host| {
        let route = routes
            .connections()
            .iter()
            .find(|route| route.login.endpoint.host == host)
            .unwrap();
        ConnectionRef::from(route)
    };
    let primary = find_route("primary.example");
    let secondary = find_route("secondary.example");
    assert_ne!(primary.id, local.id);
    assert_ne!(primary.id, secondary.id);
    assert_eq!(primary.target.port, 2222);
    assert_eq!(primary.target.username, "fixture");
    let jump = primary.jump.as_ref().unwrap();
    assert_eq!(
        (jump.host.as_str(), jump.port, jump.username.as_str()),
        ("jump.example", 2200, "jump-user")
    );

    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    assert_eq!(store.journal(), [&existing_run]);
    assert_eq!(
        store.journal_detail(existing_run.id),
        Some("Existing journal detail")
    );
    let scheduler = store.restore_with_actions::<SshCommand>().unwrap();
    let graph = scheduler.graph();
    let get = |id| graph.job(&JobId::new(id).unwrap()).unwrap();
    assert_eq!(graph.jobs().len(), 6);
    assert_eq!(get("local").action().connection.as_ref(), Some(&local));
    assert!(get("local").enabled());
    let command = get(COMMAND);
    assert!(!command.enabled());
    assert_eq!(command.action().connection.as_ref(), Some(&primary));
    assert_eq!(command.action().command, "printf '%s\\n' 'fixture & data'");
    assert!(command.schedule().unwrap().enabled());
    let snippet = get(SNIPPET);
    assert!(!snippet.enabled());
    assert_eq!(snippet.mode(), DependencyMode::Chain);
    assert_eq!(snippet.dependencies(), [JobId::new(COMMAND).unwrap()]);
    assert_eq!(
        snippet.action().targets().cloned().collect::<Vec<_>>(),
        [primary.clone(), secondary]
    );
    let link = snippet.action().snippet.as_ref().unwrap();
    assert_eq!(link.arguments, ["literal space", "a'b"]);
    let privileged = get(PRIVILEGED);
    assert!(!privileged.enabled());
    assert_eq!(privileged.action().connection.as_ref(), Some(&primary));
    assert!(privileged.action().imported_privileged.is_some());
    assert_eq!(
        get(PRIVILEGED_DEPENDENT).dependencies(),
        std::slice::from_ref(privileged.id())
    );
    let agent = get(AGENT);
    assert!(!agent.enabled());
    assert_eq!(agent.action().connection.as_ref(), Some(&primary));
    assert!(agent.action().agent.is_none());
    assert_eq!(
        agent.action().imported_ai.as_ref().unwrap().goal,
        "Inspect the fixture"
    );
    let snippets = snippets::Library::open(fixture.root.join(snippets::FILE_NAME)).unwrap();
    let script = snippets.get(link.id).unwrap();
    assert_eq!(script.name(), "Deploy α");
    assert_eq!(script.language(), "bash");
    assert!(script.content().contains("printf '%s' '東京 & <db>'"));
    drop(store);

    let installed = fs::read(fixture.root.join(JOBS)).unwrap();
    let mut committing = pending;
    committing[PENDING_MAGIC_WITH_MODE.len()] = JAVA_JOBS_COMMITTING;
    let directory = Directory::open(&fixture.root, false).unwrap();
    directory.publish_copy(PENDING, &committing).unwrap();
    assert!(pending_java_cancellation(&fixture.root).unwrap().is_none());
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);

    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), installed);
}

#[test]
fn java_scheduler_restore_preserves_conflicting_job_and_omits_its_dependent() {
    let fixture = Fixture::new();
    let (_, existing_run) = fixture.seed_local(COMMAND);
    fixture.queue();
    fixture.apply();
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    assert_eq!(store.definitions().len(), 4);
    assert_eq!(store.definitions()[0].name, "Existing job");
    assert!(
        store
            .definitions()
            .iter()
            .any(|job| job.id.as_str() == AGENT && !job.enabled)
    );
    assert!(
        store
            .definitions()
            .iter()
            .all(|job| job.id.as_str() != SNIPPET)
    );
    assert_eq!(store.journal(), [&existing_run]);
}

#[test]
fn java_scheduler_restore_changed_connections_is_cancellable_before_publication() {
    let fixture = Fixture::new();
    fixture.seed_local("local");
    fixture.queue();

    let directory = Directory::open(&fixture.root, false).unwrap();
    let path = fixture.root.join(CONNECTIONS);
    let before_jobs = fs::read(fixture.root.join(JOBS)).unwrap();
    let mut changed = String::from_utf8(fs::read(&path).unwrap()).unwrap();
    assert!(changed.contains("local.example"));
    changed = changed.replace("local.example", "edited.example");
    directory
        .replace_file(CONNECTIONS, changed.as_bytes())
        .unwrap();
    assert!(ProfileLease::acquire(&fixture.root, || false).is_err());
    assert_eq!(fs::read(&path).unwrap(), changed.as_bytes());
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), before_jobs);
    assert!(!fixture.root.join(snippets::FILE_NAME).exists());
    let receipt = pending_java_cancellation(&fixture.root).unwrap().unwrap();
    discard_pending_java(&receipt).unwrap();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), before_jobs);
}

#[test]
fn java_scheduler_restore_rejects_a_changed_pending_job_digest_before_publication() {
    let fixture = Fixture::new();
    fixture.seed_local("local");
    fixture.queue();

    let directory = Directory::open(&fixture.root, false).unwrap();
    let connections = fs::read(fixture.root.join(CONNECTIONS)).unwrap();
    let jobs = fs::read(fixture.root.join(JOBS)).unwrap();
    let pending = fs::read(fixture.root.join(PENDING)).unwrap();
    let (marker, payload) = pending.split_at(PENDING_MAGIC_WITH_MODE.len() + 1 + NONCE_LEN);
    let plan_size = u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize;
    let (header, snapshot) = payload.split_at(4 + plan_size);
    let mut plan: serde_json::Value = serde_json::from_slice(&header[4..]).unwrap();
    let digest = plan["jobs"].as_array_mut().unwrap();
    assert_eq!(digest.len(), 32);
    let byte = digest[0].as_u64().unwrap();
    digest[0] = serde_json::json!(if byte == 0 { 1 } else { 0 });
    let plan = serde_json::to_vec(&plan).unwrap();
    let mut altered = marker.to_vec();
    altered.extend_from_slice(&(plan.len() as u32).to_le_bytes());
    altered.extend_from_slice(&plan);
    altered.extend_from_slice(snapshot);
    directory.replace_file(PENDING, &altered).unwrap();

    assert!(ProfileLease::acquire(&fixture.root, || false).is_err());
    assert_eq!(
        fs::read(fixture.root.join(CONNECTIONS)).unwrap(),
        connections
    );
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), jobs);
    assert!(!fixture.root.join(snippets::FILE_NAME).exists());
    let receipt = pending_java_cancellation(&fixture.root).unwrap().unwrap();
    discard_pending_java(&receipt).unwrap();
}
