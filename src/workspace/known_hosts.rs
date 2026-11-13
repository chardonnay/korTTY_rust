//! Native management of the same endpoint pins used by interactive and scheduled SSH.

use std::collections::BTreeSet;
use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::{Size, Task, window};
use kortty_ssh::{ImportReport, ImportReview, KnownHostsFile, TrustedHost};

use crate::clipboard_policy::ClipboardPolicy;
use crate::profile;
use crate::sftp_window::{Directory, list_local};
use crate::ssh_connection::trust_store;
use crate::workspace::dialog_placement::Kind as DialogKind;

use super::{Message as Desktop, Workspace, tool_tabs};

mod view;
pub(super) use view::{review_conflict_label, review_key_label};

pub(super) const TITLE: &str = "Known Hosts";
const PAGE_SIZE: usize = 96;

#[derive(Debug, Clone)]
pub enum Message {
    Open,
    Reload,
    Loaded(Result<Vec<TrustedHost>, String>),
    Search(String),
    Page(bool),
    Select(TrustedHost),
    Remove,
    ConfirmRemoval,
    Import,
    SourcePath(String),
    Browse(PathBuf),
    Browsed(Result<Arc<Directory>, String>),
    Preview,
    Reviewed(Result<Arc<ImportReview>, String>),
    ApplyImport,
    Mutated(Result<Mutation, String>),
    Back,
    Focus,
    Close,
}

#[derive(Debug, Clone)]
pub enum Mutation {
    Removed(bool),
    Imported(Arc<ImportReport>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Placement {
    Standalone { owner: window::Id },
    Hosted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    Load,
    Browse,
    Preview,
    Write,
}

pub(super) struct KnownHosts {
    id: window::Id,
    placement: Placement,
    entries: Vec<Row>,
    query: String,
    page: usize,
    selected: Option<usize>,
    mode: Mode,
    work: Option<Work>,
    reload_after_work: bool,
    load_error: Option<String>,
    status: Option<String>,
}

struct Row {
    pin: TrustedHost,
    search: String,
    algorithm: String,
    fingerprint: String,
    date: String,
}

enum Mode {
    Table,
    Remove(TrustedHost),
    Source {
        path: String,
        browse: Option<Arc<Directory>>,
    },
    Review(Arc<ImportReview>),
    Summary(Arc<ImportReport>),
}

impl KnownHosts {
    fn new(id: window::Id, placement: Placement) -> Self {
        Self {
            id,
            placement,
            entries: Vec::new(),
            query: String::new(),
            page: 0,
            selected: None,
            mode: Mode::Table,
            work: None,
            reload_after_work: false,
            load_error: None,
            status: None,
        }
    }

    fn load(&mut self) -> Task<Desktop> {
        self.work = Some(Work::Load);
        self.reload_after_work = false;
        let id = self.id;
        Task::future(async move {
            let result = trust_store().and_then(|store| store.list());
            wrap(
                id,
                Message::Loaded(result.map_err(|error| error.to_string())),
            )
        })
    }

    fn request_reload(&mut self) -> Task<Desktop> {
        if self.busy() {
            self.reload_after_work = true;
            Task::none()
        } else {
            self.load()
        }
    }

    fn reload_if_pending(&mut self) -> Task<Desktop> {
        if self.reload_after_work && !self.busy() {
            self.load()
        } else {
            Task::none()
        }
    }

    fn busy(&self) -> bool {
        self.work.is_some()
    }

    fn writing(&self) -> bool {
        self.work == Some(Work::Write)
    }

    fn ready(&self) -> bool {
        !self.busy() && self.load_error.is_none()
    }

    fn set_entries(&mut self, pins: Result<Vec<TrustedHost>, String>) {
        self.work = None;
        match pins {
            Ok(pins) => {
                let previous = self.selected.and_then(|index| self.entries.get(index));
                self.selected =
                    previous.and_then(|row| pins.iter().position(|pin| row.pin == *pin));
                let zone = jiff::tz::TimeZone::system();
                self.entries = pins.into_iter().map(|pin| Row::new(pin, &zone)).collect();
                self.load_error = None;
                self.page = self
                    .page
                    .min(self.visible_count().saturating_sub(1) / PAGE_SIZE);
            }
            Err(error) => {
                self.entries.clear();
                self.selected = None;
                self.load_error = Some(error);
            }
        }
    }

    fn visible_count(&self) -> usize {
        match &self.mode {
            Mode::Table => {
                let needle = self.query.trim().to_ascii_lowercase();
                self.entries
                    .iter()
                    .filter(|row| row.matches(&needle))
                    .count()
            }
            Mode::Review(review) => report_details_count(review.report()),
            Mode::Summary(report) => report_details_count(report),
            _ => 0,
        }
    }
}

fn report_details_count(report: &ImportReport) -> usize {
    report.added.len() + report.conflicts.len() + report.trusted_but_revoked.len()
}

impl Row {
    fn new(pin: TrustedHost, zone: &jiff::tz::TimeZone) -> Self {
        let date = pin
            .trusted_since
            .and_then(|time| i64::try_from(time).ok())
            .and_then(|time| jiff::Timestamp::from_second(time).ok())
            .map_or_else(
                || "—".into(),
                |time| {
                    time.to_zoned(zone.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                },
            );
        let algorithm = pin.key.algorithm();
        let fingerprint = pin.key.fingerprint();
        let search = format!(
            "{}:{} {algorithm} {fingerprint}",
            pin.endpoint.host(),
            pin.endpoint.port()
        )
        .to_ascii_lowercase();
        Self {
            search,
            algorithm,
            fingerprint,
            pin,
            date,
        }
    }

    fn matches(&self, needle: &str) -> bool {
        self.search.contains(needle)
    }
}

impl Workspace {
    fn show_known_hosts(&mut self, owner: window::Id) -> Task<Desktop> {
        if self.exiting || self.scheduler_draining || !self.windows.contains_key(&owner) {
            return Task::none();
        }
        if let Some((_, focus)) = self.owner_dialog(owner) {
            return self.update(focus);
        }
        let hosted = self.window_settings.open_tool_windows_as_tabs;
        let (id, open, placement) = if hosted {
            if let Some(id) = self.hosted_tool_in(owner, tool_tabs::Kind::KnownHosts) {
                return self.focus_hosted_tool(id).unwrap_or_else(Task::none);
            }
            (window::Id::unique(), Task::none(), Placement::Hosted)
        } else {
            let (id, open) = self.open_owned_placed_dialog(
                owner,
                DialogKind::KnownHosts,
                window::Settings {
                    size: Size::new(1060.0, 480.0),
                    min_size: Some(Size::new(850.0, 340.0)),
                    position: window::Position::Centered,
                    exit_on_close_request: false,
                    ..window::Settings::default()
                },
                None,
            );
            self.windows
                .get_mut(&owner)
                .expect("known owner window")
                .cancel_paste();
            (id, open, Placement::Standalone { owner })
        };
        let mut tool = KnownHosts::new(id, placement);
        let load = tool.load();
        self.known_hosts.insert(id, tool);
        if hosted {
            let open = self.host_tool(owner, tool_tabs::Kind::KnownHosts, id);
            if self.hosted_tool_in(owner, tool_tabs::Kind::KnownHosts) != Some(id) {
                self.known_hosts.remove(&id);
                return open;
            }
            return Task::batch([open, load]);
        }
        Task::batch([open, load])
    }

    pub(super) fn known_hosts_title(&self, id: window::Id) -> Option<&'static str> {
        self.known_hosts
            .get(&id)
            .is_some_and(|tool| matches!(tool.placement, Placement::Standalone { .. }))
            .then_some(TITLE)
    }

    pub(super) fn known_hosts_for_owner(&self, owner: window::Id) -> Option<window::Id> {
        self.known_hosts
            .values()
            .find(|tool| tool.placement == Placement::Standalone { owner })
            .map(|tool| tool.id)
    }

    pub(super) fn close_known_hosts(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        let tool = self.known_hosts.get(&id)?;
        let Placement::Standalone { owner } = tool.placement else {
            return None;
        };
        if tool.writing() {
            return Some(Task::none());
        }
        self.known_hosts.remove(&id);
        Some(self.close_placed_dialog_and_focus(id, owner))
    }

    pub(super) fn dismiss_known_hosts(&mut self, id: window::Id) -> Option<Task<Desktop>> {
        self.known_hosts_title(id)?;
        let tool = self.known_hosts.get(&id)?;
        if matches!(tool.mode, Mode::Table) {
            self.close_known_hosts(id)
        } else {
            Some(self.update_known_hosts(id, Message::Back))
        }
    }

    pub(super) fn remove_known_hosts_for_owner(&mut self, owner: window::Id) {
        if let Some(id) = self.known_hosts_for_owner(owner) {
            self.known_hosts.remove(&id);
        }
    }

    pub(super) fn hosted_known_hosts_busy(&self, id: window::Id) -> bool {
        self.known_hosts
            .get(&id)
            .is_some_and(|tool| tool.placement == Placement::Hosted && tool.writing())
    }

    pub(super) fn dismiss_hosted_known_hosts(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(tool) = self
            .known_hosts
            .get(&id)
            .filter(|tool| tool.placement == Placement::Hosted)
        else {
            return Task::none();
        };
        if matches!(tool.mode, Mode::Table) {
            self.request_hosted_tool_close(id)
        } else {
            self.update_known_hosts(id, Message::Back)
        }
    }

    pub(super) fn remove_hosted_known_hosts(&mut self, id: window::Id) {
        if self
            .known_hosts
            .get(&id)
            .is_some_and(|tool| tool.placement == Placement::Hosted)
        {
            self.known_hosts.remove(&id);
        }
    }

    pub(super) fn host_pins_locked(&self) -> bool {
        self.clipboard.policy.host_pins_locked || self.clipboard.policy.problem.is_some()
    }

    pub(super) fn refresh_known_hosts(&mut self) -> Task<Desktop> {
        Task::batch(
            self.known_hosts
                .values_mut()
                .map(KnownHosts::request_reload),
        )
    }

    pub(super) fn update_known_hosts(&mut self, id: window::Id, message: Message) -> Task<Desktop> {
        if matches!(message, Message::Open) {
            return self.show_known_hosts(id);
        }
        if matches!(message, Message::Close) {
            if self
                .known_hosts
                .get(&id)
                .is_some_and(|tool| tool.placement == Placement::Hosted)
            {
                return self.request_hosted_tool_close(id);
            }
            return self.close_known_hosts(id).unwrap_or_else(Task::none);
        }
        let locked = self.host_pins_locked();
        let can_import = self.clipboard.policy.problem.is_none();
        let close_locked = self.hosted_close_locks(id);
        let Some(tool) = self.known_hosts.get_mut(&id) else {
            return Task::none();
        };
        if (tool.busy() || close_locked)
            && !matches!(
                message,
                Message::Loaded(_)
                    | Message::Browsed(_)
                    | Message::Reviewed(_)
                    | Message::Mutated(_)
                    | Message::Focus
            )
        {
            return Task::none();
        }
        match message {
            Message::Focus => {
                return match tool.placement {
                    Placement::Standalone { .. } => window::gain_focus(id),
                    Placement::Hosted => self.focus_hosted_tool(id).unwrap_or_else(Task::none),
                };
            }
            Message::Reload => return tool.load(),
            Message::Loaded(pins) if tool.work == Some(Work::Load) => tool.set_entries(pins),
            Message::Search(query) if query.len() <= 256 && matches!(tool.mode, Mode::Table) => {
                let needle = query.trim().to_ascii_lowercase();
                if tool
                    .selected
                    .is_some_and(|index| !tool.entries[index].matches(&needle))
                {
                    tool.selected = None;
                }
                tool.query = query;
                tool.page = 0;
            }
            Message::Page(forward) => {
                tool.page = if forward {
                    (tool.page + 1).min(tool.visible_count().saturating_sub(1) / PAGE_SIZE)
                } else {
                    tool.page.saturating_sub(1)
                };
            }
            Message::Select(pin) if tool.ready() && matches!(tool.mode, Mode::Table) => {
                tool.selected = tool.entries.iter().position(|row| row.pin == pin);
            }
            Message::Remove if tool.ready() && !locked && matches!(tool.mode, Mode::Table) => {
                if let Some(row) = tool.selected.and_then(|index| tool.entries.get(index)) {
                    tool.mode = Mode::Remove(row.pin.clone());
                    tool.status = None;
                }
            }
            Message::ConfirmRemoval if tool.ready() && !locked => {
                if let Mode::Remove(pin) = &tool.mode {
                    let pin = pin.clone();
                    tool.work = Some(Work::Write);
                    return Task::future(async move {
                        let result = ensure_pin_mutation_allowed()
                            .and_then(|()| trust_store()?.remove_pin(&pin.endpoint, &pin.key))
                            .map(Mutation::Removed);
                        wrap(
                            id,
                            Message::Mutated(result.map_err(|error| error.to_string())),
                        )
                    });
                }
            }
            Message::Import if tool.ready() && can_import && matches!(tool.mode, Mode::Table) => {
                tool.mode = Mode::Source {
                    path: default_source()
                        .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
                    browse: None,
                };
                tool.status = None;
            }
            Message::SourcePath(value) if value.len() <= 4096 => {
                if let Mode::Source { path, .. } = &mut tool.mode {
                    *path = value;
                }
            }
            Message::Browse(path)
                if matches!(tool.mode, Mode::Source { .. }) && path.is_absolute() =>
            {
                tool.work = Some(Work::Browse);
                tool.status = None;
                return Task::future(async move {
                    let result = list_local(&path)
                        .map(Arc::new)
                        .map_err(|error| error.to_string());
                    wrap(id, Message::Browsed(result))
                });
            }
            Message::Browsed(result) if tool.work == Some(Work::Browse) => {
                tool.work = None;
                if let Mode::Source { browse, .. } = &mut tool.mode {
                    match result {
                        Ok(directory) => *browse = Some(directory),
                        Err(error) => {
                            tool.status = Some(format!("Could not open directory ({error})"))
                        }
                    }
                }
            }
            Message::Preview if tool.ready() && can_import => {
                if let Mode::Source { path, .. } = &tool.mode {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() {
                        tool.status = Some("Choose an absolute path to the OpenSSH file.".into());
                        return Task::none();
                    }
                    tool.work = Some(Work::Preview);
                    tool.status = None;
                    return Task::future(async move {
                        let result = read_source(&path)
                            .and_then(preview_import)
                            .map(Arc::new)
                            .map_err(|error| error.to_string());
                        wrap(id, Message::Reviewed(result))
                    });
                }
            }
            Message::Reviewed(result) if tool.work == Some(Work::Preview) => {
                tool.work = None;
                match result {
                    Ok(review) => {
                        tool.mode = Mode::Review(review);
                        tool.page = 0;
                    }
                    Err(error) => {
                        tool.status = Some(format!("Could not prepare the import ({error})"))
                    }
                }
            }
            Message::ApplyImport if tool.ready() && can_import => {
                if let Mode::Review(review) = &tool.mode
                    && !review.report().added.is_empty()
                {
                    let review = review.clone();
                    tool.work = Some(Work::Write);
                    return Task::future(async move {
                        let result = import_reviewed(&review)
                            .map(|report| Mutation::Imported(Arc::new(report)))
                            .map_err(|error| error.to_string());
                        wrap(id, Message::Mutated(result))
                    });
                }
            }
            Message::Mutated(result) if tool.writing() => {
                tool.work = None;
                let reload_all = result.is_ok();
                match result {
                    Ok(Mutation::Removed(removed)) => {
                        tool.mode = Mode::Table;
                        tool.status = Some(if removed {
                            "Trusted key removed. The next connection requires verification again."
                        } else {
                            "The trusted key changed while the confirmation was open. Review the current key."
                        }.into());
                    }
                    Ok(Mutation::Imported(report)) => {
                        tool.mode = Mode::Summary(report);
                        tool.page = 0;
                    }
                    Err(error) => {
                        tool.status = Some(format!("Could not update trusted keys ({error})"))
                    }
                }
                let close = if tool.placement == Placement::Hosted {
                    self.resume_hosted_tool_close(id)
                } else {
                    Task::none()
                };
                let reload = if reload_all {
                    self.refresh_known_hosts()
                } else {
                    self.known_hosts
                        .get_mut(&id)
                        .map_or_else(Task::none, KnownHosts::reload_if_pending)
                };
                return Task::batch([close, reload]);
            }
            Message::Back => {
                tool.mode = Mode::Table;
                tool.status = None;
                tool.page = 0;
            }
            _ => {}
        }
        tool.reload_if_pending()
    }
}

pub(super) fn ensure_pin_mutation_allowed() -> io::Result<()> {
    let policy = ClipboardPolicy::load();
    if policy.host_pins_locked || policy.problem.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed policy locks trusted host keys",
        ));
    }
    Ok(())
}

pub(super) fn preview_import(source: KnownHostsFile) -> io::Result<ImportReview> {
    ensure_import_allowed()?;
    trust_store()?.preview_import(source)
}

fn import_reviewed(review: &ImportReview) -> io::Result<ImportReport> {
    ensure_import_allowed()?;
    trust_store()?.import_reviewed(review)
}

pub(super) fn import_reviewed_selected(
    review: &ImportReview,
    selected: &BTreeSet<usize>,
) -> io::Result<ImportReport> {
    ensure_import_allowed()?;
    trust_store()?.import_reviewed_selected(review, selected)
}

fn ensure_import_allowed() -> io::Result<()> {
    if ClipboardPolicy::load().problem.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "managed policy could not be applied",
        ));
    }
    Ok(())
}

fn read_source(path: &Path) -> io::Result<KnownHostsFile> {
    let file = profile::open_preference(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > KnownHostsFile::MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "choose a regular known_hosts file up to 8 MiB",
        ));
    }
    let mut input = String::new();
    file.take(KnownHostsFile::MAX_BYTES + 1)
        .read_to_string(&mut input)?;
    KnownHostsFile::parse(&input)
}

fn default_source() -> Option<PathBuf> {
    let home =
        env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)?;
    home.is_absolute()
        .then(|| home.join(".ssh").join("known_hosts"))
}

fn wrap(id: window::Id, message: Message) -> Desktop {
    Desktop::KnownHosts(id, message)
}

#[cfg(test)]
mod tests {
    use kortty_ssh::{Endpoint, HostKey};

    use super::*;

    const FIXTURE: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKuSNsxvt1EJ0ELnEMXiOgkaExGvVly+uWvDAKyIGKcX";

    fn pin(host: &str, port: u16) -> TrustedHost {
        TrustedHost {
            endpoint: Endpoint::new(host, port).unwrap(),
            key: HostKey::from_openssh(FIXTURE).unwrap(),
            trusted_since: None,
        }
    }

    #[test]
    fn standalone_known_hosts_keeps_each_native_owner() {
        use iced::futures::{StreamExt, executor::block_on, future::ready};
        use iced_runtime::{Action as RuntimeAction, task, window::Action as WindowAction};

        let mut workspace = Workspace::default();
        let (first, _) = workspace.open_window(false);
        let (second, _) = workspace.open_window(false);
        let _ = workspace.update_known_hosts(first, Message::Open);
        let _ = workspace.update_known_hosts(second, Message::Open);
        let first_dialog = workspace.known_hosts_for_owner(first).unwrap();
        let second_dialog = workspace.known_hosts_for_owner(second).unwrap();
        assert_ne!(first_dialog, second_dialog);
        assert_eq!(workspace.placed_dialog_owner(first_dialog), Some(first));
        assert_eq!(workspace.placed_dialog_owner(second_dialog), Some(second));

        let reopen = workspace.update_known_hosts(first, Message::Open);
        assert!(matches!(
            block_on(task::into_stream(reopen).unwrap().next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == first_dialog
        ));
        assert_eq!(workspace.known_hosts.len(), 2);

        let blocked_close = workspace.update(Desktop::Window(first, window::Event::CloseRequested));
        assert!(matches!(
            block_on(task::into_stream(blocked_close).unwrap().next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == first_dialog
        ));
        assert!(workspace.windows.contains_key(&first));

        let owner_close = task::into_stream(workspace.close_window(first)).unwrap();
        let closed: Vec<_> = block_on(
            owner_close
                .filter_map(|action| {
                    ready(match action {
                        RuntimeAction::Window(WindowAction::Close(id)) => Some(id),
                        _ => None,
                    })
                })
                .collect(),
        );
        assert_eq!(closed, [first_dialog, first]);
        assert!(!workspace.known_hosts.contains_key(&first_dialog));
        assert_eq!(workspace.placed_dialog_owner(first_dialog), None);
        assert_eq!(workspace.known_hosts_for_owner(second), Some(second_dialog));
        assert_eq!(workspace.placed_dialog_owner(second_dialog), Some(second));

        let _ = workspace.finish_owned_dialog_open(second_dialog, second, None);
        let dismiss = workspace.update_known_hosts(second_dialog, Message::Close);
        let mut actions = task::into_stream(dismiss).unwrap();
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Run(id, _))) if id == second_dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::Close(id))) if id == second_dialog
        ));
        assert!(matches!(
            block_on(actions.next()),
            Some(RuntimeAction::Window(WindowAction::GainFocus(id))) if id == second
        ));
    }

    #[test]
    fn known_hosts_filters_and_preserves_the_reviewed_pin_after_reload() {
        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let _ = workspace.update_known_hosts(owner, Message::Open);
        let id = workspace.known_hosts_for_owner(owner).unwrap();
        let original = pin("original.example", 2222);
        let _ = workspace.update_known_hosts(id, Message::Loaded(Ok(vec![original.clone()])));
        let _ = workspace.update_known_hosts(id, Message::Search("ORIGINAL.EXAMPLE:2222".into()));
        assert_eq!(workspace.known_hosts[&id].visible_count(), 1);
        let _ = workspace.update_known_hosts(id, Message::Select(original.clone()));
        let _ = workspace.update_known_hosts(id, Message::Remove);
        let _ = workspace.refresh_known_hosts();
        let _ = workspace
            .update_known_hosts(id, Message::Loaded(Ok(vec![pin("unrelated.example", 22)])));
        let tool = &workspace.known_hosts[&id];
        assert!(matches!(&tool.mode, Mode::Remove(reviewed) if *reviewed == original));
        assert_eq!(tool.selected, None);
        let _ = workspace.update_known_hosts(id, Message::Back);
        let _ = workspace.update_known_hosts(id, Message::Select(original));
        assert_eq!(workspace.known_hosts[&id].selected, None);
    }

    #[test]
    fn hosted_known_hosts_moves_review_and_waits_for_durable_write_before_closing() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (source, _) = workspace.open_window(false);
        let (destination, _) = workspace.open_window(false);
        let _ = workspace.update_known_hosts(source, Message::Open);
        let hosted = workspace.hosted_tools_in(source)[0];
        let instance = hosted.instance;
        assert_eq!(workspace.known_hosts_for_owner(source), None);
        assert_eq!(workspace.known_hosts_title(instance), None);

        // Test doubles deliver task results without touching the live trust store.
        let original = pin("reviewed.example", 2222);
        let _ = workspace.update_known_hosts(instance, Message::Loaded(Ok(vec![original.clone()])));
        let _ = workspace.update_known_hosts(instance, Message::Select(original.clone()));
        let _ = workspace.update_known_hosts(instance, Message::Remove);
        let _ = workspace.transfer(source, Some(destination), hosted.id);
        assert!(workspace.hosted_tools_in(source).is_empty());
        assert_eq!(workspace.hosted_tools_in(destination)[0].instance, instance);
        assert!(
            matches!(&workspace.known_hosts[&instance].mode, Mode::Remove(reviewed) if *reviewed == original)
        );

        let _ = workspace.update_known_hosts(source, Message::Open);
        let peer = workspace.hosted_tools_in(source)[0].instance;
        assert_ne!(peer, instance);
        assert_eq!(workspace.known_hosts[&peer].work, Some(Work::Load));

        let _ = workspace.update_known_hosts(instance, Message::ConfirmRemoval);
        assert!(workspace.hosted_known_hosts_busy(instance));
        let _ = workspace.update_known_hosts(instance, Message::Close);
        let _ = workspace.update_known_hosts(instance, Message::Loaded(Ok(Vec::new())));
        assert!(workspace.hosted_known_hosts_busy(instance));
        assert_eq!(workspace.hosted_tools_in(destination)[0].instance, instance);

        let _ =
            workspace.update_known_hosts(instance, Message::Mutated(Ok(Mutation::Removed(false))));
        assert!(!workspace.known_hosts.contains_key(&instance));
        assert!(workspace.hosted_tools_in(destination).is_empty());
        assert!(workspace.known_hosts[&peer].reload_after_work);
        let _ = workspace.update_known_hosts(peer, Message::Loaded(Ok(vec![original])));
        assert_eq!(workspace.known_hosts[&peer].work, Some(Work::Load));
        assert!(!workspace.known_hosts[&peer].reload_after_work);
    }
}
