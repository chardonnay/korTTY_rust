//! Prepare terminal sidebar files through the verified SFTP desktop-copy path.

use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use iced::widget::{button, container, text};
use iced::{Element, Point, Task, mouse, window};

use crate::clipboard_policy::ClipboardPolicy;
use crate::sftp_window::worker::drag_out::{self, Candidate, Preparation, Prepared};
use crate::sftp_window::{Entry, EntryKind, EntryName, Location, file_drag_grip};

use super::{Action, Binding, Desktop, Dock, Message, Target, Window, Workspace};
#[cfg(target_os = "linux")]
use crate::workspace::wayland_file_drop::DragSource;
#[cfg(target_os = "windows")]
use crate::workspace::windows_file_drag::DragSource;

pub type PreparationResult = Result<Arc<Prepared>, Arc<str>>;

#[derive(Clone, PartialEq, Eq)]
pub struct Scope {
    target: Target,
    names: Arc<[EntryName]>,
}

impl fmt::Debug for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Remote sidebar desktop file selection")
    }
}

impl Scope {
    pub(super) fn tab(&self) -> u64 {
        self.target.tab
    }

    fn matches(&self, target: Option<&Target>, selected: &HashSet<EntryName>) -> bool {
        target == Some(&self.target)
            && self.names.len() == selected.len()
            && self.names.iter().all(|name| selected.contains(name))
    }
}

#[cfg(all(test, windows))]
pub(in crate::workspace) fn test_scope(session: crate::session::SessionId) -> Scope {
    Scope {
        target: Target {
            tab: 1,
            generation: 1,
            session,
            path: b"/test".to_vec(),
            listing: 1,
        },
        names: vec![EntryName::Remote(b"file".to_vec())].into(),
    }
}

#[derive(Default)]
pub(super) struct State {
    sequence: u64,
    pending: Option<(Scope, u64, Preparation)>,
    prepared: Option<(Scope, Arc<Prepared>, Instant)>,
    error: Option<(Scope, Arc<str>)>,
}

impl State {
    fn scope(&self) -> Option<&Scope> {
        self.pending
            .as_ref()
            .map(|(scope, _, _)| scope)
            .or_else(|| self.prepared.as_ref().map(|(scope, _, _)| scope))
            .or_else(|| self.error.as_ref().map(|(scope, _)| scope))
    }

    fn clear(&mut self) {
        self.pending = None;
        self.prepared = None;
        self.error = None;
    }

    fn ready(&self, scope: &Scope) -> bool {
        self.prepared
            .as_ref()
            .is_some_and(|(prepared, _, deadline)| prepared == scope && Instant::now() < *deadline)
    }
}

fn candidates(
    target: &Target,
    entries: &[Entry],
    selected: &HashSet<EntryName>,
) -> Option<(Scope, Vec<Candidate>)> {
    if !(1..=drag_out::MAX_FILES).contains(&selected.len()) {
        return None;
    }
    let directory = Location::Remote(target.path.clone());
    let mut names = Vec::with_capacity(selected.len());
    let mut files = Vec::with_capacity(selected.len());
    let mut remaining = drag_out::MAX_BYTES;
    for entry in entries
        .iter()
        .filter(|entry| selected.contains(&entry.name))
    {
        if entry.kind != EntryKind::File {
            return None;
        }
        let candidate = Candidate::for_entry(&directory, entry)?;
        remaining = remaining.checked_sub(candidate.size)?;
        names.push(entry.name.clone());
        files.push(candidate);
    }
    (names.len() == selected.len()).then(|| {
        (
            Scope {
                target: target.clone(),
                names: names.into(),
            },
            files,
        )
    })
}

impl Dock {
    pub(super) fn desktop_drag_controls(
        &self,
        tab: u64,
        binding: &Binding,
    ) -> Option<(Element<'_, Action>, Option<&str>)> {
        let target = binding.target(tab)?;
        let (scope, _) = candidates(&target, binding.browser.entries(), &self.selected)?;
        let state = &self.desktop_drag;
        let error = state
            .error
            .as_ref()
            .filter(|(failed, _)| *failed == scope)
            .map(|(_, error)| error.as_ref());
        if state.ready(&scope) {
            let handle = container(text("Drag selection to desktop").size(13))
                .padding([5, 8])
                .style(container::bordered_box);
            return Some((
                file_drag_grip(
                    handle.into(),
                    scope,
                    mouse::Cursor::position,
                    Action::StartDesktopDrag,
                ),
                error,
            ));
        }
        let preparing = state.pending.is_some();
        let control = button(if preparing {
            "Preparing desktop drag…"
        } else {
            "Prepare desktop drag"
        })
        .on_press_maybe((!preparing).then_some(Action::PrepareDesktopDrag(scope)));
        Some((control.into(), error))
    }
}

impl Workspace {
    pub(super) fn sidebar_drag_available(
        &self,
        owner: window::Id,
        tab: u64,
        binding: &Binding,
    ) -> bool {
        #[cfg(target_os = "linux")]
        let native = self.wayland_file_drop.can_drag(owner);
        #[cfg(target_os = "windows")]
        let native = self.windows_drag_owner_available(owner);
        native
            && !self.exiting
            && !self.scheduler_draining
            && self.owner_dialog(owner).is_none()
            && !self.palette.is_open(owner)
            && !self.terminal_only_fullscreen_active(owner)
            && !binding.follow.foreign
            && binding.manager_loan.is_none()
            && !self.remote_manager_owns(binding.browser.session())
            && binding.browser.idle_worker().is_some()
            && binding
                .target(tab)
                .is_some_and(|target| self.valid_remote_transfer(owner, &target))
            && self
                .windows
                .get(&owner)
                .and_then(Window::tab)
                .is_some_and(|tab| !tab.incognito || self.clipboard.policy.incognito_allowed)
    }

    pub(in crate::workspace) fn sidebar_drag_current(
        &self,
        owner: window::Id,
        scope: &Scope,
        installed: &ClipboardPolicy,
    ) -> bool {
        let Some(tab) = self.windows.get(&owner).and_then(Window::tab) else {
            return false;
        };
        let dock = &tab.remote_files;
        let Some(binding) = &dock.binding else {
            return false;
        };
        scope.matches(binding.target(tab.id).as_ref(), &dock.selected)
            && self.sidebar_drag_available(owner, tab.id, binding)
            && (!tab.incognito || installed.incognito_allowed)
            && installed.problem.is_none()
            && installed.file_transfer_allowed
            && self
                .remote_pane(scope.target.session)
                .and_then(|(pane, _)| {
                    pane.ssh.as_ref().map(|target| {
                        target.workflow_route_allowed(installed)
                            && (installed.minimum_isolation
                                != crate::session_security::IsolationLevel::Sandbox
                                || pane.session.sandbox_backend().is_some())
                    })
                })
                .unwrap_or(false)
    }

    fn sidebar_drag_state_mut(&mut self, owner: window::Id, tab: u64) -> Option<&mut State> {
        self.active_tab_mut(owner)
            .filter(|active| active.id == tab)
            .map(|tab| &mut tab.remote_files.desktop_drag)
    }

    pub(super) fn prepare_sidebar_file_drag(
        &mut self,
        owner: window::Id,
        scope: Scope,
    ) -> Task<Desktop> {
        let installed = ClipboardPolicy::load();
        if !self.sidebar_drag_current(owner, &scope, &installed) {
            return Task::none();
        }
        let Some(tab) = self.active_tab_mut(owner) else {
            return Task::none();
        };
        let dock = &mut tab.remote_files;
        let state = &mut dock.desktop_drag;
        if state.pending.is_some() {
            return Task::none();
        }
        let Some(binding) = &dock.binding else {
            return Task::none();
        };
        let Some(worker) = binding.browser.idle_worker() else {
            return Task::none();
        };
        let Some((selection, files)) =
            candidates(&scope.target, binding.browser.entries(), &dock.selected)
        else {
            return Task::none();
        };
        if selection != scope {
            return Task::none();
        }
        let (preparation, receive) = match worker.prepare_desktop_drag(files) {
            Ok(work) => work,
            Err(error) => {
                state.error = Some((scope, error.to_string().into()));
                return Task::none();
            }
        };
        state.clear();
        state.sequence = state.sequence.wrapping_add(1);
        let ticket = state.sequence;
        state.pending = Some((scope.clone(), ticket, preparation));
        Task::perform(
            async move {
                match receive.recv().await {
                    Ok(Ok(prepared)) => Ok(Arc::new(prepared)),
                    Ok(Err(error)) => Err(error.to_string().into()),
                    Err(_) => Err("Desktop drag preparation stopped.".into()),
                }
            },
            move |result| {
                Desktop::RemoteDock(
                    owner,
                    Message::DesktopDragPrepared(scope.clone(), ticket, result),
                )
            },
        )
    }

    pub(super) fn finish_sidebar_file_drag_preparation(
        &mut self,
        owner: window::Id,
        scope: Scope,
        ticket: u64,
        result: PreparationResult,
    ) {
        let installed = ClipboardPolicy::load();
        if !self.sidebar_drag_current(owner, &scope, &installed) {
            return;
        }
        let result = result.and_then(|prepared| {
            let worker = self
                .windows
                .get(&owner)
                .and_then(Window::tab)
                .and_then(|tab| tab.remote_files.binding.as_ref())
                .and_then(|binding| binding.browser.idle_worker());
            if worker.is_some_and(|worker| prepared.matches(worker))
                && prepared.permitted(&installed)
                && prepared.permitted(&self.clipboard.policy)
            {
                Ok(prepared)
            } else {
                Err("The original SFTP connection is no longer available.".into())
            }
        });
        let Some(state) = self.sidebar_drag_state_mut(owner, scope.tab()) else {
            return;
        };
        if state
            .pending
            .as_ref()
            .is_none_or(|(current, expected, _)| *expected != ticket || *current != scope)
        {
            return;
        }
        state.pending = None;
        match result {
            Ok(prepared) => {
                state.prepared = Some((
                    scope,
                    prepared,
                    Instant::now() + drag_out::PREPARED_LIFETIME,
                ));
            }
            Err(error) => state.error = Some((scope, error)),
        }
    }

    pub(super) fn start_sidebar_file_drag(
        &mut self,
        owner: window::Id,
        scope: Scope,
        _origin: Point,
    ) -> Task<Desktop> {
        let installed = ClipboardPolicy::load();
        #[cfg(target_os = "linux")]
        let native = &self.wayland_file_drop;
        #[cfg(target_os = "windows")]
        let native = &self.windows_file_drag;
        #[cfg(target_os = "linux")]
        let can_start = native.can_start_drag(owner);
        #[cfg(target_os = "windows")]
        let can_start = native.can_start_drag();
        if !self.sidebar_drag_current(owner, &scope, &installed) || !can_start {
            return Task::none();
        }
        if native.remote_drag_limit_reached() {
            self.set_sidebar_drag_error(
                owner,
                &scope,
                "Wait for earlier desktop file copies to finish.",
            );
            return Task::none();
        }
        let prepared = self
            .windows
            .get(&owner)
            .and_then(Window::tab)
            .and_then(|tab| {
                let dock = &tab.remote_files;
                let worker = dock.binding.as_ref()?.browser.idle_worker()?;
                let (current, prepared, deadline) = dock.desktop_drag.prepared.as_ref()?;
                (*current == scope
                    && Instant::now() < *deadline
                    && prepared.matches(worker)
                    && prepared.permitted(&installed)
                    && prepared.permitted(&self.clipboard.policy))
                .then(|| Arc::clone(prepared))
            });
        let Some(prepared) = prepared else {
            return Task::none();
        };
        #[cfg(target_os = "windows")]
        let incognito = self
            .windows
            .get(&owner)
            .and_then(Window::tab)
            .is_some_and(|tab| tab.incognito);
        if let Some(state) = self.sidebar_drag_state_mut(owner, scope.tab()) {
            state.clear();
        }
        let source = DragSource::Sidebar(scope);
        #[cfg(target_os = "linux")]
        {
            self.start_native_file_drag(owner, source, _origin, &[], Some(prepared));
            Task::none()
        }
        #[cfg(target_os = "windows")]
        {
            self.start_windows_file_drag(owner, source, Vec::new(), Some(prepared), incognito)
        }
    }

    pub(in crate::workspace) fn set_sidebar_drag_error(
        &mut self,
        owner: window::Id,
        scope: &Scope,
        error: &'static str,
    ) {
        if let Some(state) = self.sidebar_drag_state_mut(owner, scope.tab()) {
            state.error = Some((scope.clone(), error.into()));
        }
    }

    pub(in crate::workspace) fn sidebar_has_drag_preparation(&self) -> bool {
        self.windows
            .values()
            .flat_map(Window::terminal_tabs)
            .any(|tab| {
                let state = &tab.remote_files.desktop_drag;
                state.pending.is_some() || state.prepared.is_some()
            })
    }

    pub(in crate::workspace) fn sidebar_has_drag_state(&self) -> bool {
        self.windows
            .values()
            .flat_map(Window::terminal_tabs)
            .any(|tab| tab.remote_files.desktop_drag.scope().is_some())
    }

    pub(in crate::workspace) fn revalidate_sidebar_drag_preparations(
        &mut self,
        installed: Option<&ClipboardPolicy>,
    ) {
        let now = Instant::now();
        let workspace: &Workspace = self;
        let stale = workspace
            .windows
            .iter()
            .flat_map(|(&owner, window)| {
                window.terminal_tabs().filter_map(move |tab| {
                    let state = &tab.remote_files.desktop_drag;
                    let scope = state.scope()?;
                    let expired = state
                        .prepared
                        .as_ref()
                        .is_some_and(|(_, prepared, deadline)| {
                            now >= *deadline
                                || !prepared.permitted(&workspace.clipboard.policy)
                                || installed.is_none_or(|policy| !prepared.permitted(policy))
                        });
                    let current = installed
                        .is_none_or(|policy| workspace.sidebar_drag_current(owner, scope, policy));
                    (expired || !current).then_some((owner, tab.id))
                })
            })
            .collect::<Vec<_>>();
        for (owner, tab) in stale {
            if let Some(tab) = self.windows.get_mut(&owner).and_then(|window| {
                window
                    .terminal_tabs_mut()
                    .find(|candidate| candidate.id == tab)
            }) {
                tab.remote_files.desktop_drag.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::session::SessionId;

    use super::*;

    #[test]
    fn sidebar_scope_requires_the_exact_session_generation_listing_path_and_selection() {
        let name = EntryName::Remote(b"report.txt".to_vec());
        let target = Target {
            tab: 1,
            generation: 2,
            session: SessionId(3),
            path: b"/original".to_vec(),
            listing: 4,
        };
        let scope = Scope {
            target: target.clone(),
            names: vec![name.clone()].into(),
        };
        let selected = HashSet::from([name]);
        assert!(scope.matches(Some(&target), &selected));
        for stale in [
            Target {
                tab: 2,
                ..target.clone()
            },
            Target {
                generation: 3,
                ..target.clone()
            },
            Target {
                session: SessionId(4),
                ..target.clone()
            },
            Target {
                path: b"/renamed".to_vec(),
                ..target.clone()
            },
            Target {
                listing: 5,
                ..target.clone()
            },
        ] {
            assert!(!scope.matches(Some(&stale), &selected));
        }
        assert!(!scope.matches(None, &selected));
        assert!(!scope.matches(
            Some(&target),
            &HashSet::from([EntryName::Remote(b"another.txt".to_vec())]),
        ));
    }

    #[test]
    fn sidebar_stages_only_the_selected_small_regular_files() -> std::io::Result<()> {
        let fixture = crate::snippets::tests::Profile::new();
        std::fs::write(fixture.0.join("report.txt"), b"report")?;
        std::fs::write(fixture.0.join("unselected.txt"), b"unselected")?;
        std::fs::create_dir(fixture.0.join("directory"))?;
        let mut entries = crate::sftp_window::list_local(&fixture.0)?.entries;
        for entry in &mut entries {
            let EntryName::Local(name) = &entry.name else {
                panic!("expected a local fixture entry");
            };
            entry.name = EntryName::Remote(name.as_encoded_bytes().to_vec());
        }
        let target = Target {
            tab: 1,
            generation: 2,
            session: SessionId(3),
            path: b"/remote/snapshot".to_vec(),
            listing: 4,
        };
        let report = EntryName::Remote(b"report.txt".to_vec());
        let selection = HashSet::from([report.clone()]);
        let (scope, files) = candidates(&target, &entries, &selection).unwrap();
        assert_eq!(scope.target.path, b"/remote/snapshot");
        assert_eq!(scope.names.as_ref(), std::slice::from_ref(&report));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size, 6);
        let mixed = HashSet::from([report.clone(), EntryName::Remote(b"directory".to_vec())]);
        assert!(candidates(&target, &entries, &mixed).is_none());
        let missing = HashSet::from([report.clone(), EntryName::Remote(b"missing".to_vec())]);
        assert!(candidates(&target, &entries, &missing).is_none());
        let regular = entries
            .iter_mut()
            .find(|entry| entry.name == report)
            .unwrap();
        regular.size = Some(drag_out::MAX_BYTES + 1);
        assert!(candidates(&target, &entries, &selection).is_none());
        Ok(())
    }
}
