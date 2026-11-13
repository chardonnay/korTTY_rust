use std::time::{Duration, SystemTime};

use kortty_jobs::{
    DependencyMode, Job, JobGraph, JobId, RunOutcome, RunRecord, Scheduler, SchedulerStore,
};
use kortty_vault::Secret;
use zip::{AesMode, ZipWriter, write::SimpleFileOptions};

use super::*;
use crate::connection_store::{Store, import::Preview as ConnectionPreview};
use crate::scheduler_window::{ConnectionRef, SshCommand};

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
const SNIPPET_XML: &[u8] =
    include_bytes!("../../snippets/archive/fixtures/java-private-library-2.26.0.xml");
const COMMAND: &str = "java-3f9bab16184b4e13bf71e37331b8843a";
const SNIPPET: &str = "java-0bd3734742024c5a912d4fe43c09f755";

struct Fixture {
    _scratch: crate::snippets::tests::Profile,
    root: PathBuf,
    archive: PathBuf,
    password: Secret,
}

impl Fixture {
    fn new() -> Self {
        let scratch = crate::snippets::tests::Profile::new();
        let archive = scratch.0.join("java-scheduler.zip");
        let root = scratch.0.join("profile");
        let password = Secret::new("fixture backup password".into());
        let mut zip = ZipWriter::new(File::create(&archive).unwrap());
        let options = SimpleFileOptions::default()
            .with_aes_encryption_bytes(AesMode::Aes256, password.as_str().as_bytes());
        let scheduler = std::str::from_utf8(SCHEDULER).unwrap().replace(
            "3ee66545-2eaf-4ac7-b352-d9a1a51d75ed",
            "00000000-0000-0000-0000-000000000001",
        );
        for (name, bytes) in [
            ("connections.xml", CONNECTION_XML),
            ("snippets.xml", SNIPPET_XML),
            ("job-scheduler.xml", scheduler.as_bytes()),
        ] {
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

    fn queue(&self) {
        let preview = super::super::inspect_java_password(&self.archive, &self.password).unwrap();
        super::super::queue_restore_at(&self.root, preview, RestoreMode::JavaAdditive).unwrap();
    }

    fn apply(&self) {
        drop(ProfileLease::acquire(&self.root, || false).unwrap());
        assert!(!self.root.join(PENDING).exists());
    }

    fn seed_local(&self, id: &str) -> (ConnectionRef, RunRecord) {
        let directory = Directory::open(&self.root, true).unwrap();
        let (connections, count) = ConnectionPreview::java_backup(LOCAL_CONNECTION)
            .unwrap()
            .portable_java_backup()
            .unwrap();
        assert_eq!(count, 1);
        directory.publish_copy(CONNECTIONS, &connections).unwrap();
        let store = Store::open(self.root.join(CONNECTIONS)).unwrap();
        let local = ConnectionRef::from(&store.connections()[0]);
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
    assert_eq!(graph.jobs().len(), 3);
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
        [primary, secondary]
    );
    let link = snippet.action().snippet.as_ref().unwrap();
    assert_eq!(link.arguments, ["literal space", "a'b"]);
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
    let local_jobs = fs::read(fixture.root.join(JOBS)).unwrap();
    fixture.queue();
    fixture.apply();
    assert_eq!(fs::read(fixture.root.join(JOBS)).unwrap(), local_jobs);
    let store = SchedulerStore::open(fixture.root.join(JOBS)).unwrap();
    assert_eq!(store.definitions().len(), 1);
    assert_eq!(store.definitions()[0].name, "Existing job");
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
