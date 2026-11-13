//! Parses portable job definitions from Java KorTTY's Scheduler repository.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::num::NonZeroU32;
use std::path::PathBuf;

use kortty_jobs::{
    Date, DependencyMode, Job, JobDefinition, JobGraph, JobId, JobSchedule, MAX_DEPENDENCIES,
    MAX_JOBS, SchedulerStore, Time, Weekday,
};
use roxmltree::Node;
use zeroize::Zeroizing;

use crate::scheduler_window::{ConnectionRef, MAX_TARGETS, SnippetLink, SshCommand, SudoMode};
use crate::snippets::SnippetId;

use super::java_secrets::parse_xml;

const MAX_XML_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_ID_BYTES: usize = 128;
const MAX_ARGUMENTS: usize = 64;
const MAX_TIMES: usize = 24 * 60;

#[derive(Default)]
pub(super) struct Review {
    pub(super) jobs: Vec<ImportedJob>,
    pub(super) omitted: BTreeMap<Omission, usize>,
    pub(super) unsupported_sections: usize,
}

pub(super) struct BoundReview {
    pub(super) jobs: JobGraph<SshCommand>,
    pub(super) omitted: BTreeMap<Omission, usize>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Omission {
    InvalidDefinition,
    UnsupportedAction,
    UnsupportedExecutionOptions,
    GroupTargets,
    InvalidTargets,
    MissingSnippet,
    InvalidDependency,
    InvalidGraph,
}

impl Omission {
    pub(super) fn description(self) -> &'static str {
        match self {
            Self::InvalidDefinition => "invalid Java Scheduler definitions omitted",
            Self::UnsupportedAction => "unsupported Java Scheduler actions omitted",
            Self::UnsupportedExecutionOptions => {
                "Java Scheduler jobs with unsupported execution options omitted"
            }
            Self::GroupTargets => "Java Scheduler jobs with connection group targets omitted",
            Self::InvalidTargets => {
                "Java Scheduler jobs with missing or invalid target references omitted"
            }
            Self::MissingSnippet => {
                "Java Scheduler jobs with missing or conflicting snippets omitted"
            }
            Self::InvalidDependency => "Java Scheduler jobs with missing prerequisites omitted",
            Self::InvalidGraph => "Java Scheduler jobs in an invalid dependency graph omitted",
        }
    }
}

pub(super) struct ImportedJob {
    pub(super) definition: JobDefinition,
    pub(super) source_targets: Vec<String>,
    pub(super) action: Action,
    pub(super) working_directory: Option<String>,
}

pub(super) enum Action {
    Command(String),
    Snippet {
        id: SnippetId,
        arguments: Vec<String>,
    },
}

impl ImportedJob {
    /// The caller resolves source IDs only against the reviewed restored routes.
    fn bind(
        self,
        mut connection: impl FnMut(&str) -> Option<ConnectionRef>,
        has_snippet: impl FnOnce(SnippetId) -> bool,
    ) -> ParseResult<Job<SshCommand>> {
        let mut targets = self
            .source_targets
            .iter()
            .map(|id| connection(id).ok_or(Omission::InvalidTargets));
        let primary = targets
            .next()
            .transpose()?
            .ok_or(Omission::InvalidTargets)?;
        let additional_connections = targets.collect::<ParseResult<Vec<_>>>()?;
        let (command, snippet) = match self.action {
            Action::Command(command) => (command, None),
            Action::Snippet { id, arguments } if has_snippet(id) => {
                (String::new(), Some(SnippetLink { id, arguments }))
            }
            Action::Snippet { .. } => return Err(Omission::MissingSnippet),
        };
        let action = SshCommand {
            host: String::new(),
            port: 0,
            username: String::new(),
            key_file: PathBuf::new(),
            connection: Some(primary),
            additional_connections,
            target_groups: None,
            command,
            working_directory: self.working_directory,
            script: None,
            snippet,
            remote: None,
            agent: None,
            sudo: SudoMode::Disabled,
            virtual_terminal: None,
            automation_journal: None,
            desktop_notices: Default::default(),
            webhook_targets: Vec::new(),
        }
        .validated()
        .map_err(|_| Omission::InvalidDefinition)?;
        self.definition
            .with_action(action)
            .map_err(|_| Omission::InvalidDefinition)
    }
}

pub(super) fn inspect(bytes: &[u8]) -> io::Result<Review> {
    let doc = parse_xml(bytes, MAX_XML_BYTES, "jobScheduler")?;
    if doc.root().children().any(|node| node.is_pi()) {
        return Err(invalid("Invalid Java Scheduler processing instruction."));
    }
    let root = doc.root_element();
    let fields = Fields::new(root).map_err(|_| invalid("Invalid Java Scheduler document."))?;
    let mut review = Review::default();
    for section in [
        "sudoCredentials",
        "pinnedHostKeys",
        "journal",
        "webhookTargets",
    ] {
        if fields
            .get(section)
            .is_some_and(|node| node.children().any(|child| child.is_element()))
        {
            review.unsupported_sections += 1;
        }
    }
    let jobs = fields
        .list("jobs", "job", MAX_JOBS)
        .map_err(|_| invalid("Invalid Java Scheduler list or job limit exceeded."))?;
    let mut ids = BTreeSet::new();
    for job in jobs {
        let Ok(fields) = Fields::new(job) else {
            *review
                .omitted
                .entry(Omission::InvalidDefinition)
                .or_default() += 1;
            continue;
        };
        let id = fields.text("id").and_then(job_id);
        if let Ok(ref id) = id
            && !ids.insert(id.clone())
        {
            return Err(invalid("Duplicate Java Scheduler job identity."));
        }
        match id.and_then(|id| parse_job(&fields, id)) {
            Ok(job) => review.jobs.push(job),
            Err(reason) => *review.omitted.entry(reason).or_default() += 1,
        }
    }
    let missing = prune_missing_dependencies(
        &mut review.jobs,
        |job| &job.definition.id,
        |job| &job.definition.dependencies,
    );
    if missing != 0 {
        *review
            .omitted
            .entry(Omission::InvalidDependency)
            .or_default() += missing;
    }
    let graph = review
        .jobs
        .iter()
        .map(|job| job.definition.clone().with_action(()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if JobGraph::new(graph).is_err() {
        *review.omitted.entry(Omission::InvalidGraph).or_default() += review.jobs.len();
        review.jobs.clear();
    }
    Ok(review)
}

impl Review {
    pub(super) fn bind_all(
        self,
        mut connection: impl FnMut(&str) -> Option<ConnectionRef>,
        mut has_snippet: impl FnMut(SnippetId) -> bool,
    ) -> io::Result<BoundReview> {
        let mut jobs = Vec::with_capacity(self.jobs.len());
        let mut omitted = self.omitted;
        for incoming in self.jobs {
            match incoming.bind(&mut connection, &mut has_snippet) {
                Ok(job) => jobs.push(job),
                Err(reason) => *omitted.entry(reason).or_default() += 1,
            }
        }
        let missing = prune_missing_dependencies(&mut jobs, Job::id, Job::dependencies);
        if missing != 0 {
            *omitted.entry(Omission::InvalidDependency).or_default() += missing;
        }
        Ok(BoundReview {
            jobs: JobGraph::new(jobs).map_err(io::Error::other)?,
            omitted,
        })
    }
}

fn prune_missing_dependencies<T>(
    jobs: &mut Vec<T>,
    id: impl Fn(&T) -> &JobId,
    dependencies: impl Fn(&T) -> &[JobId],
) -> usize {
    let mut ids: BTreeSet<_> = jobs.iter().map(|job| id(job).clone()).collect();
    let original = jobs.len();
    loop {
        let before = ids.len();
        for job in jobs.iter() {
            if dependencies(job)
                .iter()
                .any(|dependency| !ids.contains(dependency))
            {
                ids.remove(id(job));
            }
        }
        if ids.len() == before {
            jobs.retain(|job| ids.contains(id(job)));
            return original - jobs.len();
        }
    }
}

/// Encodes only reviewed disabled jobs, with native portable Scheduler JSON.
pub(super) fn encode(graph: &JobGraph<SshCommand>) -> io::Result<Zeroizing<Vec<u8>>> {
    check_bound_graph(graph)?;
    SchedulerStore::encode_backup(graph)
        .map(Zeroizing::new)
        .map_err(io::Error::other)
}

/// Revalidates the exact allowed job/action shape before applying a pending restore.
pub(super) fn decode(bytes: &[u8]) -> io::Result<JobGraph<SshCommand>> {
    let graph = SchedulerStore::decode_backup(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    check_bound_graph(&graph)?;
    Ok(graph)
}

fn check_bound_graph(graph: &JobGraph<SshCommand>) -> io::Result<()> {
    if graph.jobs().is_empty() || !graph.jobs().iter().all(valid_bound_job) {
        return Err(invalid("Invalid reviewed Java Scheduler jobs."));
    }
    Ok(())
}

fn valid_bound_job(job: &Job<SshCommand>) -> bool {
    let action = job.action();
    !job.enabled()
        && job.id().as_str().strip_prefix("java-").is_some_and(|id| {
            id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
        && action.connection.is_some()
        && action.target_groups.is_none()
        && action.script.is_none()
        && action.remote.is_none()
        && action.agent.is_none()
        && action.sudo == SudoMode::Disabled
        && action.virtual_terminal.is_none()
        && action.automation_journal.is_none()
        && action.desktop_notices == Default::default()
        && action.webhook_targets.is_empty()
        && action
            .working_directory
            .as_deref()
            .is_none_or(|value| bounded(value, 4096, false).is_ok())
        && match &action.snippet {
            Some(link) => {
                action.command.is_empty()
                    && link
                        .arguments
                        .iter()
                        .all(|value| bounded(value, 4096, false).is_ok())
            }
            None => bounded(&action.command, 16 * 1024, true).is_ok(),
        }
        && action.clone().validated().is_ok()
}

type ParseResult<T> = Result<T, Omission>;

fn parse_job(fields: &Fields<'_, '_>, id: JobId) -> ParseResult<ImportedJob> {
    if fields.boolean("hostKeyVerificationDisabled", false)? {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    if !fields
        .text_list("targetGroupNames", "groupName", MAX_TARGETS)?
        .is_empty()
    {
        return Err(Omission::GroupTargets);
    }
    let mut source_targets =
        fields.text_list("targetConnectionIds", "connectionId", MAX_TARGETS)?;
    if source_targets.is_empty() {
        source_targets.extend(fields.optional_text("connectionId")?.map(str::to_owned));
    }
    let mut unique = BTreeSet::new();
    source_targets.retain(|id| unique.insert(id.clone()));
    if source_targets.is_empty() || source_targets.iter().any(|id| !source_id(id)) {
        return Err(Omission::InvalidTargets);
    }

    let name = fields.text("name")?;
    if !name
        .chars()
        .all(crate::session_journal::is_display_character)
        || Job::new(id.clone(), name, ()).is_err()
    {
        return Err(Omission::InvalidDefinition);
    }
    let mut dependencies = fields
        .text_list("dependsOn", "jobId", MAX_DEPENDENCIES)?
        .iter()
        .map(|id| job_id(id))
        .collect::<ParseResult<Vec<_>>>()?;
    let mut unique = BTreeSet::new();
    dependencies.retain(|value| value != &id && unique.insert(value.clone()));
    let mode = match fields.optional_text("dependencyMode")? {
        None | Some("GATE") => DependencyMode::Gate,
        Some("CHAIN") => DependencyMode::Chain,
        _ => return Err(Omission::InvalidDependency),
    };
    let definition = JobDefinition {
        id,
        name: name.into(),
        enabled: false,
        mode,
        max_age_minutes: fields.positive_minutes("dependencyMaxAgeMinutes")?,
        dependencies,
        schedule: fields.get("schedule").map(parse_schedule).transpose()?,
    };
    let action = parse_action(fields.get("action").ok_or(Omission::InvalidDefinition)?)?;
    let working_directory = fields
        .optional_text("workingDirectory")?
        .map(|path| bounded(path, 4096, false).map(str::to_owned))
        .transpose()?;
    Ok(ImportedJob {
        definition,
        source_targets,
        action,
        working_directory,
    })
}

fn parse_schedule(node: Node<'_, '_>) -> ParseResult<JobSchedule> {
    let fields = Fields::new(node)?;
    let weekdays = fields
        .text_list("weekdays", "weekday", 7)?
        .iter()
        .map(|day| match day.to_ascii_uppercase().as_str() {
            "MONDAY" => Ok(Weekday::Monday),
            "TUESDAY" => Ok(Weekday::Tuesday),
            "WEDNESDAY" => Ok(Weekday::Wednesday),
            "THURSDAY" => Ok(Weekday::Thursday),
            "FRIDAY" => Ok(Weekday::Friday),
            "SATURDAY" => Ok(Weekday::Saturday),
            "SUNDAY" => Ok(Weekday::Sunday),
            _ => Err(Omission::InvalidDefinition),
        })
        .collect::<ParseResult<Vec<_>>>()?;
    let times = fields
        .text_list("fixedTimes", "time", MAX_TIMES)?
        .iter()
        .map(|time| parse_time(time))
        .collect::<ParseResult<Vec<_>>>()?;
    let start = fields
        .optional_text("windowStartTime")?
        .map(parse_time)
        .transpose()?
        .unwrap_or(Time::MIN);
    let end = fields
        .optional_text("windowEndTime")?
        .map(parse_time)
        .transpose()?
        .unwrap_or(Time::new(23, 59, 0, 0).expect("valid day end"));
    let from = fields
        .optional_text("activeFromDate")?
        .map(parse_date)
        .transpose()?;
    let until = fields
        .optional_text("activeUntilDate")?
        .map(parse_date)
        .transpose()?;
    JobSchedule::default()
        .with_enabled(fields.boolean("enabled", true)?)
        .with_weekdays(weekdays)
        .with_fixed_times(times)
        .with_window(start, end)
        .with_interval_minutes(fields.positive_minutes("intervalMinutes")?)
        .with_active_dates(from, until)
        .map_err(|_| Omission::InvalidDefinition)
}

fn parse_action(node: Node<'_, '_>) -> ParseResult<Action> {
    let fields = Fields::new(node)?;
    let kind = fields.optional_text("type")?.unwrap_or("COMMAND");
    if !matches!(kind, "COMMAND" | "SNIPPET_SCRIPT") {
        return Err(Omission::UnsupportedAction);
    }
    if fields.boolean("useSudo", false)?
        || fields.boolean("sudoStagingEnabled", false)?
        || fields.boolean("ptyEnabled", false)?
        || fields.boolean("runtimeLimitSuccess", false)?
        || fields.optional_text("runtimeLimitSeconds")?.is_some()
    {
        return Err(Omission::UnsupportedExecutionOptions);
    }
    match kind {
        "COMMAND" => {
            let command = fields.text("command")?;
            Ok(Action::Command(
                bounded(command, 16 * 1024, true)?.to_owned(),
            ))
        }
        "SNIPPET_SCRIPT" => {
            let id = fields.text("snippetId")?;
            let compact = compact_uuid(id).ok_or(Omission::InvalidDefinition)?;
            let id = SnippetId::try_from(compact).map_err(|_| Omission::InvalidDefinition)?;
            let arguments = fields
                .list("snippetArguments", "argument", MAX_ARGUMENTS)?
                .into_iter()
                .map(element_text)
                .collect::<ParseResult<Vec<_>>>()?
                .into_iter()
                .filter(|value| !value.trim().is_empty())
                .map(|value| bounded(value, 4096, false).map(str::to_owned))
                .collect::<ParseResult<Vec<_>>>()?;
            if arguments
                .iter()
                .map(|value| value.len() + 1)
                .sum::<usize>()
                .saturating_sub(1)
                > 8 * 1024
            {
                return Err(Omission::InvalidDefinition);
            }
            Ok(Action::Snippet { id, arguments })
        }
        _ => unreachable!(),
    }
}

fn parse_time(value: &str) -> ParseResult<Time> {
    value
        .parse::<Time>()
        .map_err(|_| Omission::InvalidDefinition)
}

fn parse_date(value: &str) -> ParseResult<Date> {
    value
        .parse::<Date>()
        .map_err(|_| Omission::InvalidDefinition)
}

fn job_id(source: &str) -> ParseResult<JobId> {
    let id = compact_uuid(source).ok_or(Omission::InvalidDefinition)?;
    JobId::new(format!("java-{id}")).map_err(|_| Omission::InvalidDefinition)
}

fn compact_uuid(source: &str) -> Option<String> {
    (source.len() == 36
        && source.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        }))
    .then(|| source.replace('-', "").to_ascii_lowercase())
}

fn source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SOURCE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

fn bounded(value: &str, limit: usize, multiline: bool) -> ParseResult<&str> {
    if value.trim().is_empty()
        || value.len() > limit
        || value.chars().any(|character| {
            !crate::session_journal::is_display_character(character)
                || (!multiline && matches!(character, '\n' | '\t'))
        })
    {
        return Err(Omission::InvalidDefinition);
    }
    Ok(value)
}

struct Fields<'a, 'input>(BTreeMap<&'input str, Node<'a, 'input>>);

impl<'a, 'input> Fields<'a, 'input> {
    fn new(node: Node<'a, 'input>) -> ParseResult<Self> {
        if node.attributes().len() != 0 {
            return Err(Omission::InvalidDefinition);
        }
        let mut fields = BTreeMap::new();
        for child in node.children().filter(|child| !ignorable(*child)) {
            let name = child.tag_name();
            if !child.is_element()
                || name.namespace().is_some()
                || fields.insert(name.name(), child).is_some()
            {
                return Err(Omission::InvalidDefinition);
            }
        }
        Ok(Self(fields))
    }

    fn get(&self, name: &str) -> Option<Node<'a, 'input>> {
        self.0.get(name).copied()
    }

    fn text(&self, name: &str) -> ParseResult<&'a str> {
        self.optional_text(name)?.ok_or(Omission::InvalidDefinition)
    }

    fn optional_text(&self, name: &str) -> ParseResult<Option<&'a str>> {
        self.get(name)
            .map(element_text)
            .transpose()
            .map(|value| value.map(str::trim).filter(|value| !value.is_empty()))
    }

    fn list(&self, name: &str, item: &str, limit: usize) -> ParseResult<Vec<Node<'a, 'input>>> {
        let Some(wrapper) = self.get(name) else {
            return Ok(Vec::new());
        };
        if wrapper.attributes().len() != 0 {
            return Err(Omission::InvalidDefinition);
        }
        let mut items = Vec::new();
        for child in wrapper.children().filter(|child| !ignorable(*child)) {
            if items.len() == limit
                || !child.is_element()
                || child.tag_name().namespace().is_some()
                || !child.has_tag_name(item)
            {
                return Err(Omission::InvalidDefinition);
            }
            items.push(child);
        }
        Ok(items)
    }

    fn text_list(&self, name: &str, item: &str, limit: usize) -> ParseResult<Vec<String>> {
        let mut values = Vec::new();
        for node in self.list(name, item, limit)? {
            let value = element_text(node)?.trim();
            if !value.is_empty() {
                values.push(value.to_owned());
            }
        }
        Ok(values)
    }

    fn boolean(&self, name: &str, fallback: bool) -> ParseResult<bool> {
        match self.optional_text(name)? {
            None => Ok(fallback),
            Some("true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            _ => Err(Omission::InvalidDefinition),
        }
    }

    fn positive_minutes(&self, name: &str) -> ParseResult<Option<NonZeroU32>> {
        self.optional_text(name)?
            .map(|value| {
                value
                    .parse::<i32>()
                    .map_err(|_| Omission::InvalidDefinition)
                    .map(|value| u32::try_from(value).ok().and_then(NonZeroU32::new))
            })
            .transpose()
            .map(Option::flatten)
    }
}

fn ignorable(node: Node<'_, '_>) -> bool {
    node.is_comment() || node.is_text() && node.text().is_none_or(|text| text.trim().is_empty())
}

fn element_text<'a>(node: Node<'a, '_>) -> ParseResult<&'a str> {
    if node.attributes().len() != 0 {
        return Err(Omission::InvalidDefinition);
    }
    let mut content = None;
    for child in node.children().filter(|child| !child.is_comment()) {
        if !child.is_text() || content.replace(child.text().unwrap_or_default()).is_some() {
            return Err(Omission::InvalidDefinition);
        }
    }
    Ok(content.unwrap_or_default())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_dialog::LoginTarget;

    const XML: &str = include_str!("java_scheduler/fixtures/java-2.26.0.xml");
    const PRIMARY: &str = "b04d413f-9ee5-4b56-bc12-c72da52d4b3e";
    const SECONDARY: &str = "9c0719d8-dfa7-4468-a1d9-4149cb8fa01e";

    fn connection(id: u64, host: &str) -> ConnectionRef {
        ConnectionRef {
            id,
            target: LoginTarget {
                host: host.into(),
                port: 22,
                username: "fixture".into(),
            },
            jump: None,
        }
    }

    #[test]
    fn java_scheduler_backup_reads_pinned_jaxb_jobs_and_binds_reviewed_targets() -> io::Result<()> {
        let review = inspect(XML.as_bytes())?;
        assert_eq!(review.unsupported_sections, 1);
        assert_eq!(
            review.omitted,
            BTreeMap::from([
                (Omission::UnsupportedAction, 1),
                (Omission::UnsupportedExecutionOptions, 1),
                (Omission::GroupTargets, 1),
                (Omission::InvalidDependency, 1),
            ])
        );
        let [command, snippet] = review
            .jobs
            .try_into()
            .unwrap_or_else(|_| panic!("expected a command and a snippet"));
        assert_eq!(
            command.definition.id.as_str(),
            "java-3f9bab16184b4e13bf71e37331b8843a"
        );
        assert_eq!(command.definition.name, "Inspect café & 東京");
        assert!(!command.definition.enabled && !snippet.definition.enabled);
        assert_eq!(command.source_targets, [PRIMARY]);
        assert_eq!(snippet.source_targets, [PRIMARY, SECONDARY]);
        assert_eq!(
            snippet.definition.dependencies.as_slice(),
            std::slice::from_ref(&command.definition.id)
        );
        assert_eq!(snippet.definition.mode, DependencyMode::Chain);
        assert_eq!(
            snippet.definition.max_age_minutes.map(NonZeroU32::get),
            Some(35)
        );
        let schedule = command.definition.schedule.as_ref().unwrap();
        assert!(schedule.enabled());
        assert_eq!(
            schedule.weekdays().collect::<Vec<_>>(),
            [Weekday::Monday, Weekday::Friday]
        );
        assert_eq!(
            schedule.fixed_times().collect::<Vec<_>>(),
            [
                Time::new(0, 15, 0, 0).unwrap(),
                Time::new(22, 15, 0, 0).unwrap()
            ]
        );
        assert_eq!(
            schedule.window(),
            (
                Time::new(22, 0, 0, 0).unwrap(),
                Time::new(2, 0, 0, 0).unwrap()
            )
        );
        assert_eq!(
            schedule.active_dates(),
            (
                Some(Date::new(2026, 11, 1).unwrap()),
                Some(Date::new(2027, 2, 1).unwrap())
            )
        );
        assert_eq!(schedule.interval_minutes().map(NonZeroU32::get), Some(30));

        let command = command
            .bind(
                |id| (id == PRIMARY).then(|| connection(17, "first.example")),
                |_| false,
            )
            .unwrap();
        assert_eq!(command.action().command, "printf '%s\\n' 'fixture & data'");
        assert_eq!(
            command.action().working_directory.as_deref(),
            Some("/srv/project files")
        );
        assert_eq!(command.action().connection.as_ref().unwrap().id, 17);
        assert_eq!(command.action().sudo, SudoMode::Disabled);
        assert!(command.action().webhook_targets.is_empty());
        let snippet = snippet
            .bind(
                |id| match id {
                    PRIMARY => Some(connection(17, "first.example")),
                    SECONDARY => Some(connection(33, "second.example")),
                    _ => None,
                },
                |id| String::from(id) == "3ee665452eaf4ac7b352d9a1a51d75ed",
            )
            .unwrap();
        assert_eq!(snippet.action().additional_connections[0].id, 33);
        assert_eq!(
            snippet.action().snippet.as_ref().unwrap().arguments,
            ["literal space", "a'b"]
        );
        assert!(JobGraph::new(vec![command, snippet]).is_ok());
        Ok(())
    }

    #[test]
    fn java_scheduler_backup_rejects_ambiguous_ids_unsafe_xml_and_missing_targets() -> io::Result<()>
    {
        let root = XML.split_once("<jobs>").unwrap().1;
        let first = root.split_once("</job>").unwrap().0;
        let duplicated =
            format!("<jobScheduler><jobs>{first}</job>{first}</job></jobs></jobScheduler>");
        assert!(inspect(duplicated.as_bytes()).is_err());
        let external =
            br#"<!DOCTYPE jobScheduler [<!ENTITY f SYSTEM "file:///secret">]><jobScheduler/>"#;
        assert!(inspect(external).is_err());
        let altered = XML.replace(
            "<hostKeyVerificationDisabled>false</hostKeyVerificationDisabled>",
            "<hostKeyVerificationDisabled>true</hostKeyVerificationDisabled>",
        );
        assert!(inspect(altered.as_bytes())?.jobs.is_empty());
        let mut jobs = inspect(XML.as_bytes())?.jobs.into_iter();
        assert!(jobs.next().unwrap().bind(|_| None, |_| false).is_err());
        assert!(
            jobs.next()
                .unwrap()
                .bind(
                    |id| Some(connection(
                        if id == PRIMARY { 17 } else { 33 },
                        "fixture.example"
                    )),
                    |_| false
                )
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn java_scheduler_backup_serializes_only_closed_disabled_jobs_without_authority()
    -> io::Result<()> {
        let lookup = |id: &str| match id {
            PRIMARY => Some(connection(17, "first.example")),
            SECONDARY => Some(connection(33, "second.example")),
            _ => None,
        };
        let reviewed = inspect(XML.as_bytes())?.bind_all(lookup, |_| true)?;
        assert_eq!(reviewed.jobs.jobs().len(), 2);
        let bytes = encode(&reviewed.jobs)?;
        let restored = decode(&bytes)?;
        for (expected, actual) in reviewed.jobs.jobs().iter().zip(restored.jobs()) {
            assert_eq!(expected.definition(), actual.definition());
            assert_eq!(expected.action(), actual.action());
        }

        let mut forged: serde_json::Value = serde_json::from_slice(&bytes)?;
        forged["definitions"][0]["enabled"] = serde_json::json!(true);
        assert!(decode(&serde_json::to_vec(&forged)?).is_err());
        forged["definitions"][0]["enabled"] = serde_json::json!(false);
        let command_id = reviewed.jobs.jobs()[0].id().as_str();
        forged["actions"][command_id]["sudo"] = serde_json::json!("passwordless");
        assert!(decode(&serde_json::to_vec(&forged)?).is_err());

        let missing_target = inspect(XML.as_bytes())?.bind_all(
            |id| (id == SECONDARY).then(|| connection(33, "second.example")),
            |_| true,
        )?;
        assert!(missing_target.jobs.jobs().is_empty());
        assert_eq!(missing_target.omitted[&Omission::InvalidTargets], 2);
        let missing_snippet = inspect(XML.as_bytes())?.bind_all(lookup, |_| false)?;
        assert_eq!(missing_snippet.jobs.jobs().len(), 1);
        assert_eq!(missing_snippet.omitted[&Omission::MissingSnippet], 1);

        let parent_unavailable = XML.replacen(
            "<connectionId>b04d413f-9ee5-4b56-bc12-c72da52d4b3e</connectionId>",
            "<connectionId>missing-parent-target</connectionId>",
            1,
        );
        let omitted = inspect(parent_unavailable.as_bytes())?.bind_all(lookup, |_| true)?;
        assert!(omitted.jobs.jobs().is_empty());
        assert_eq!(omitted.omitted[&Omission::InvalidTargets], 1);
        assert_eq!(omitted.omitted[&Omission::InvalidDependency], 2);
        Ok(())
    }
}
