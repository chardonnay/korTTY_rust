use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::widget::{
    button, checkbox, column, row, scrollable, space, text, text_editor, text_input,
};
use iced::{Element, Length, Task, window};

use crate::profile::WorkspaceLayout;
use crate::profile::projects::java::export;
use crate::profile::projects::{
    FileProject, Name, PreparedProject, ProjectFile, ProjectFolder, Store, java,
};
use crate::session::SessionId;

use super::{Message as WorkspaceMessage, Workspace, tool_tabs::HostedTab};

mod attention;
mod browser;
mod java_export;
pub(super) mod local_files;
mod reconnect;
pub(super) use attention::WaitReason;
use browser::{Browser, Intent, Listing, Mode};
pub(super) use reconnect::ProjectReconnects;

#[derive(Debug, Clone)]
pub enum Message {
    Show,
    ShowOpenFile,
    ShowSaveFile,
    Close,
    Name(String),
    Select(Name),
    Save,
    Open,
    JavaProjectPath(String),
    JavaConnectionsPath(String),
    ImportJava,
    ExportJava,
    Browse(browser::Event),
    FileName(u64, String),
    FileDestination(u64, String),
    FileDescription(u64, text_editor::Action),
    AutoReconnect(u64, bool),
    CancelReconnects,
    Attention(attention::Event),
    Decide(u64, bool),
    Finished(u64, Result<Outcome, String>),
}

#[derive(Debug, Clone)]
pub enum Outcome {
    Listed(Vec<Name>),
    Saved(Name),
    Existing {
        name: Name,
        original: Arc<WorkspaceLayout>,
        layout: Arc<WorkspaceLayout>,
    },
    Loaded(Name, Arc<WorkspaceLayout>),
    JavaPreview(Name, Arc<java::Imported>),
    JavaExportPreview(Arc<export::Export>),
    JavaExported(PathBuf),
    FileListed(Arc<Listing>),
    FileLoaded(Arc<FileProject>),
    FileNeedsReplace(Arc<PreparedProject>),
    FileSaved(ProjectFile, Option<String>),
}

#[derive(Default)]
pub(super) struct Panel {
    browser: Option<Browser>,
    from_recent_file: bool,
    name: String,
    names: Vec<Name>,
    java_project: String,
    java_connections: String,
    phase: Phase,
    notice: Option<String>,
}

#[derive(Default)]
enum Phase {
    #[default]
    Ready,
    Working(u64),
    Writing(u64, Box<FileSettings>),
    Confirm(u64, Confirmation),
}

enum Confirmation {
    Replace {
        name: Name,
        original: Arc<WorkspaceLayout>,
        layout: Arc<WorkspaceLayout>,
    },
    Open {
        target: OpenTarget,
        layout: Arc<WorkspaceLayout>,
        tabs: Vec<(u64, SessionId)>,
        details: Vec<String>,
        reconnect_offered: bool,
    },
    ImportJava(Name, Arc<java::Imported>),
    ExportJava(Arc<export::Export>),
    SaveFile(Box<FileSettings>),
    ReplaceFile {
        draft: Box<FileSettings>,
        prepared: Arc<PreparedProject>,
    },
}

struct FileSettings {
    folder: ProjectFolder,
    filename: String,
    layout: WorkspaceLayout,
    name: String,
    description: text_editor::Content,
    auto_reconnect: bool,
}

#[derive(Clone)]
pub(super) enum OpenTarget {
    Named(Name),
    File {
        source: ProjectFile,
        name: String,
        reconnect: bool,
    },
}

impl OpenTarget {
    fn name(&self) -> &str {
        match self {
            Self::Named(name) => name.as_str(),
            Self::File { name, .. } => name,
        }
    }
}

fn wrap(window: window::Id, message: Message) -> WorkspaceMessage {
    WorkspaceMessage::Project(window, message)
}

fn run(
    window: window::Id,
    request: u64,
    work: impl FnOnce() -> io::Result<Outcome> + Send + 'static,
) -> Task<WorkspaceMessage> {
    Task::future(async move {
        let (send, receive) = async_channel::bounded(1);
        let worker = std::thread::Builder::new()
            .name("kortty-project".into())
            .spawn(move || {
                let _ = send.try_send(work().map_err(|error| error.to_string()));
            });
        let result = match worker {
            Ok(_) => receive
                .recv()
                .await
                .unwrap_or_else(|_| Err("The project worker stopped.".into())),
            Err(error) => Err(error.to_string()),
        };
        wrap(window, Message::Finished(request, result))
    })
}

fn save_new(store: &Store, name: Name, layout: Arc<WorkspaceLayout>) -> io::Result<Outcome> {
    Ok(match store.save_new(&name, &layout)? {
        Some(original) => Outcome::Existing {
            name,
            original: Arc::new(original),
            layout,
        },
        None => Outcome::Saved(name),
    })
}

impl Workspace {
    pub(super) fn open_recent_project_file(
        &mut self,
        window: window::Id,
        file: ProjectFile,
    ) -> Task<WorkspaceMessage> {
        let Some(state) = self
            .windows
            .get_mut(&window)
            .filter(|state| state.projects.is_none())
        else {
            return Task::none();
        };
        state.cancel_paste();
        state.projects = Some(Panel {
            browser: Some(Browser::new(Mode::Open)),
            from_recent_file: true,
            ..Panel::default()
        });
        let request = self.begin_project_work(window);
        run(window, request, move || {
            file.read().map(Arc::new).map(Outcome::FileLoaded)
        })
    }

    pub(super) fn open_recent_project(
        &mut self,
        window: window::Id,
        name: Name,
    ) -> Task<WorkspaceMessage> {
        let Some(state) = self
            .windows
            .get_mut(&window)
            .filter(|state| state.projects.is_none())
        else {
            return Task::none();
        };
        state.cancel_paste();
        state.projects = Some(Panel {
            name: name.as_str().into(),
            ..Panel::default()
        });
        self.update_project(window, Message::Open)
    }

    pub(super) fn project_panel_open(&self, window: window::Id) -> bool {
        self.windows
            .get(&window)
            .is_some_and(|state| state.projects.is_some())
    }

    pub(super) fn update_project(
        &mut self,
        window: window::Id,
        message: Message,
    ) -> Task<WorkspaceMessage> {
        if let Message::Attention(event) = message {
            return self.update_project_attention(window, event);
        }
        if matches!(message, Message::CancelReconnects) {
            self.cancel_project_reconnects(window);
            return Task::none();
        }
        if matches!(message, Message::Close) && self.hosted_close_prompt(window).is_some() {
            return Task::none();
        }
        if matches!(
            message,
            Message::Show | Message::ShowOpenFile | Message::ShowSaveFile
        ) {
            let Some(state) = self
                .windows
                .get_mut(&window)
                .filter(|state| state.projects.is_none())
            else {
                return Task::none();
            };
            state.cancel_paste();
            let mode = match message {
                Message::ShowOpenFile => Some(Mode::Open),
                Message::ShowSaveFile => Some(Mode::Save),
                _ => None,
            };
            state.projects = Some(Panel {
                browser: mode.map(Browser::new),
                ..Panel::default()
            });
            if mode.is_some() {
                return self.update_project(window, Message::Browse(browser::Event::List));
            }
            let request = self.begin_project_work(window);
            return run(window, request, || {
                Store::open()?.list().map(Outcome::Listed)
            });
        }
        let Some(panel) = self
            .windows
            .get_mut(&window)
            .and_then(|state| state.projects.as_mut())
        else {
            return Task::none();
        };
        match message {
            Message::Close if !matches!(panel.phase, Phase::Writing(..)) => {
                self.windows
                    .get_mut(&window)
                    .expect("project window")
                    .projects = None
            }
            Message::Browse(event) if matches!(panel.phase, Phase::Ready) => {
                let Some(browser) = panel.browser.as_mut() else {
                    return Task::none();
                };
                panel.notice = None;
                match browser.update(event) {
                    Ok(Some(intent)) => return self.run_project_file_intent(window, intent),
                    Ok(None) => {}
                    Err(error) => panel.notice = Some(error.to_string()),
                }
            }
            Message::FileName(request, name) if name.len() <= 256 => {
                if let Phase::Confirm(active, Confirmation::SaveFile(settings)) = &mut panel.phase
                    && request == *active
                {
                    settings.name = name;
                }
            }
            Message::FileDestination(request, filename) if filename.len() <= 240 => {
                if let Phase::Confirm(active, Confirmation::SaveFile(settings)) = &mut panel.phase
                    && request == *active
                {
                    settings.filename = filename;
                    panel.notice = None;
                }
            }
            Message::FileDescription(request, edit) => {
                if let Phase::Confirm(active, Confirmation::SaveFile(settings)) = &mut panel.phase
                    && request == *active
                {
                    let before = settings.description.text();
                    settings.description.perform(edit);
                    if settings.description.text().len() > 4096 {
                        settings.description = text_editor::Content::with_text(&before);
                    }
                }
            }
            Message::AutoReconnect(request, enabled) => {
                if let Phase::Confirm(active, confirmation) = &mut panel.phase
                    && *active == request
                {
                    match confirmation {
                        Confirmation::SaveFile(settings) => settings.auto_reconnect = enabled,
                        Confirmation::Open {
                            target: OpenTarget::File { reconnect, .. },
                            reconnect_offered: true,
                            ..
                        } => *reconnect = enabled,
                        _ => {}
                    }
                }
            }
            Message::Name(name) if matches!(panel.phase, Phase::Ready) && name.len() <= 256 => {
                panel.name = name;
                panel.notice = None;
            }
            Message::Select(name)
                if matches!(panel.phase, Phase::Ready) && panel.names.contains(&name) =>
            {
                panel.name = name.as_str().into();
                panel.notice = None;
            }
            Message::JavaProjectPath(path)
                if matches!(panel.phase, Phase::Ready) && path.len() <= 4096 =>
            {
                panel.java_project = path;
                panel.notice = None;
            }
            Message::JavaConnectionsPath(path)
                if matches!(panel.phase, Phase::Ready) && path.len() <= 4096 =>
            {
                panel.java_connections = path;
                panel.notice = None;
            }
            Message::ImportJava | Message::ExportJava if matches!(panel.phase, Phase::Ready) => {
                let name = match Name::parse(&panel.name) {
                    Ok(name) => name,
                    Err(error) => {
                        panel.notice = Some(error.to_string());
                        return Task::none();
                    }
                };
                let project = PathBuf::from(panel.java_project.trim());
                let connections = (!panel.java_connections.trim().is_empty())
                    .then(|| PathBuf::from(panel.java_connections.trim()));
                let export = matches!(message, Message::ExportJava);
                let request = self.begin_project_work(window);
                return run(window, request, move || {
                    if export {
                        let layout = Store::open()?.load(&name)?;
                        let prepared = java_export::prepare(&name, layout, project, connections)?;
                        return Ok(Outcome::JavaExportPreview(Arc::new(prepared)));
                    }
                    let imported = java::read(&project, connections.as_deref())?;
                    Ok(Outcome::JavaPreview(name, Arc::new(imported)))
                });
            }
            Message::Save | Message::Open if matches!(panel.phase, Phase::Ready) => {
                let name = match Name::parse(&panel.name) {
                    Ok(name) => name,
                    Err(error) => {
                        panel.notice = Some(error.to_string());
                        return Task::none();
                    }
                };
                let layout = if matches!(message, Message::Save) {
                    let Some(layout) = self.capture_layout_ordered(Some(window)).filter(|layout| {
                        layout.windows.iter().any(|window| !window.tabs.is_empty())
                    }) else {
                        self.project_notice(
                            window,
                            "There are no non-incognito tabs to save.".into(),
                        );
                        return Task::none();
                    };
                    Some(Arc::new(layout))
                } else {
                    if self.clipboard.policy.problem.is_some() {
                        self.project_notice(
                            window,
                            "Managed policy could not be applied. Sessions are locked.".into(),
                        );
                        return Task::none();
                    }
                    None
                };
                let request = self.begin_project_work(window);
                return run(window, request, move || {
                    let store = Store::open()?;
                    let Some(layout) = layout else {
                        return store
                            .load(&name)
                            .map(|layout| Outcome::Loaded(name, Arc::new(layout)));
                    };
                    save_new(&store, name, layout)
                });
            }
            Message::Finished(request, result) if matches!(panel.phase, Phase::Working(active) | Phase::Writing(active, _) if request == active) =>
            {
                let draft = match std::mem::take(&mut panel.phase) {
                    Phase::Writing(_, draft) => Some(draft),
                    _ => None,
                };
                match result {
                    Ok(Outcome::Listed(names)) => panel.names = names,
                    Ok(Outcome::Saved(name)) => {
                        let remembered = name.clone();
                        panel.notice = Some(format!("Project {} saved.", name.as_str()));
                        panel.name = name.as_str().into();
                        if let Err(index) = panel.names.binary_search(&name) {
                            panel.names.insert(index, name);
                        }
                        return self.remember_open_recent_project(window, &remembered);
                    }
                    Ok(Outcome::Existing {
                        name,
                        original,
                        layout,
                    }) => {
                        panel.phase = Phase::Confirm(
                            request,
                            Confirmation::Replace {
                                name,
                                original,
                                layout,
                            },
                        );
                    }
                    Ok(Outcome::Loaded(name, layout)) => {
                        let tabs = self.project_tab_identity(window);
                        let target = OpenTarget::Named(name);
                        let details: Vec<_> = local_file_details(&layout).collect();
                        if tabs.is_empty() && details.is_empty() {
                            return self.open_project_layout(window, target, layout, tabs);
                        }
                        self.windows
                            .get_mut(&window)
                            .expect("project window")
                            .projects
                            .as_mut()
                            .expect("project panel")
                            .phase = Phase::Confirm(
                            request,
                            Confirmation::Open {
                                target,
                                layout,
                                tabs,
                                details,
                                reconnect_offered: false,
                            },
                        );
                    }
                    Ok(Outcome::JavaPreview(name, imported)) => {
                        panel.phase =
                            Phase::Confirm(request, Confirmation::ImportJava(name, imported));
                    }
                    Ok(Outcome::JavaExportPreview(prepared)) => {
                        panel.phase = Phase::Confirm(request, Confirmation::ExportJava(prepared));
                    }
                    Ok(Outcome::JavaExported(path)) => {
                        panel.notice =
                            Some(format!("Java project exported to {}.", path.display()));
                    }
                    Ok(Outcome::FileListed(listing)) => {
                        if let Some(browser) = panel.browser.as_mut() {
                            browser.listed(request, listing);
                        }
                    }
                    Ok(Outcome::FileLoaded(project)) => {
                        return self.review_project_file(
                            window,
                            request,
                            Arc::unwrap_or_clone(project),
                        );
                    }
                    Ok(Outcome::FileNeedsReplace(prepared)) => {
                        if let Some(draft) = draft {
                            panel.phase = Phase::Confirm(
                                request,
                                Confirmation::ReplaceFile { draft, prepared },
                            );
                        }
                    }
                    Ok(Outcome::FileSaved(source, warning)) => {
                        self.windows
                            .get_mut(&window)
                            .expect("project window")
                            .projects = None;
                        let mut notice = format!("Project saved to {}.", source.path().display());
                        if let Some(warning) = warning {
                            notice.push(' ');
                            notice.push_str(&warning);
                        }
                        self.notice(window, notice);
                        return self.remember_open_recent_project_file(window, source);
                    }
                    Err(error) => {
                        panel.notice = Some(format!("Project operation failed ({error})"));
                        if let Some(draft) = draft {
                            panel.phase = Phase::Confirm(request, Confirmation::SaveFile(draft));
                        }
                    }
                }
            }
            Message::Decide(request, accepted) if matches!(panel.phase, Phase::Confirm(active, _) if request == active) =>
            {
                let Phase::Confirm(_, action) = std::mem::take(&mut panel.phase) else {
                    unreachable!("confirmed project action");
                };
                if !accepted {
                    match action {
                        Confirmation::ReplaceFile { draft, .. } => {
                            let request = self.begin_project_work(window);
                            self.set_project_phase(
                                window,
                                Phase::Confirm(request, Confirmation::SaveFile(draft)),
                            );
                        }
                        Confirmation::Open { .. } if panel.from_recent_file => {
                            self.windows
                                .get_mut(&window)
                                .expect("project window")
                                .projects = None;
                        }
                        _ => {}
                    }
                    return Task::none();
                }
                match action {
                    Confirmation::Replace {
                        name,
                        original,
                        layout,
                    } => {
                        let request = self.begin_project_work(window);
                        return run(window, request, move || {
                            Store::open()?.replace(&name, &original, &layout)?;
                            Ok(Outcome::Saved(name))
                        });
                    }
                    Confirmation::Open {
                        target,
                        layout,
                        tabs,
                        ..
                    } => {
                        if self.project_tab_identity(window) == tabs {
                            return self.open_project_layout(window, target, layout, tabs);
                        }
                        self.project_notice(
                            window,
                            "The window's tabs changed. Open the project again.".into(),
                        );
                    }
                    Confirmation::ImportJava(name, imported) => {
                        let request = self.begin_project_work(window);
                        return run(window, request, move || {
                            let layout = Arc::new(Arc::unwrap_or_clone(imported).layout);
                            save_new(&Store::open()?, name, layout)
                        });
                    }
                    Confirmation::ExportJava(prepared) => {
                        let request = self.begin_project_work(window);
                        return run(window, request, move || {
                            prepared.publish()?;
                            Ok(Outcome::JavaExported(prepared.destination.clone()))
                        });
                    }
                    Confirmation::SaveFile(settings) => {
                        let folder = settings.folder.clone();
                        let filename = settings.filename.trim().to_owned();
                        let name = settings.name.clone();
                        let description = settings.description.text();
                        let layout = settings.layout.clone();
                        let auto_reconnect = settings.auto_reconnect;
                        let request = self.begin_project_work(window);
                        self.set_project_phase(window, Phase::Writing(request, settings));
                        return run(window, request, move || {
                            let prepared = folder.prepare(
                                &filename,
                                name,
                                description,
                                layout,
                                auto_reconnect,
                            )?;
                            if prepared.exists() {
                                return Ok(Outcome::FileNeedsReplace(Arc::new(prepared)));
                            }
                            let warning = prepared.publish()?;
                            Ok(Outcome::FileSaved(prepared.source, warning))
                        });
                    }
                    Confirmation::ReplaceFile { draft, prepared } => {
                        let request = self.begin_project_work(window);
                        self.set_project_phase(window, Phase::Writing(request, draft));
                        return run(window, request, move || {
                            let warning = prepared.replace()?;
                            Ok(Outcome::FileSaved(prepared.source.clone(), warning))
                        });
                    }
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn run_project_file_intent(
        &mut self,
        window: window::Id,
        intent: Intent,
    ) -> Task<WorkspaceMessage> {
        match intent {
            Intent::List(path) => {
                let request = self.begin_project_work(window);
                run(window, request, move || {
                    Listing::read(&path).map(Arc::new).map(Outcome::FileListed)
                })
            }
            Intent::Open(folder, file) => {
                if !self.project_open_allowed(window) {
                    return Task::none();
                }
                let request = self.begin_project_work(window);
                run(window, request, move || {
                    file.read_from(&folder)
                        .map(Arc::new)
                        .map(Outcome::FileLoaded)
                })
            }
            Intent::Save(folder, filename) => {
                let Some(layout) = self
                    .capture_layout_ordered(Some(window))
                    .filter(|layout| layout.windows.iter().any(|window| !window.tabs.is_empty()))
                else {
                    self.project_notice(window, "There are no non-incognito tabs to save.".into());
                    return Task::none();
                };
                let name = Path::new(&filename)
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .unwrap_or(&filename)
                    .to_owned();
                let request = self.begin_project_work(window);
                let panel = self
                    .windows
                    .get_mut(&window)
                    .expect("project window")
                    .projects
                    .as_mut()
                    .expect("project panel");
                panel.phase = Phase::Confirm(
                    request,
                    Confirmation::SaveFile(Box::new(FileSettings {
                        folder,
                        filename,
                        layout,
                        name,
                        description: text_editor::Content::new(),
                        auto_reconnect: false,
                    })),
                );
                Task::none()
            }
        }
    }

    fn review_project_file(
        &mut self,
        window: window::Id,
        request: u64,
        project: FileProject,
    ) -> Task<WorkspaceMessage> {
        let FileProject {
            source,
            layout,
            name,
            description,
            auto_reconnect,
            java_notes,
        } = project;
        let tabs = self.project_tab_identity(window);
        let mut details = vec![layout_summary(&layout)];
        if !description.is_empty() {
            details.push(description);
        }
        details.extend(java_notes);
        details.extend(local_file_details(&layout));
        let panel = self
            .windows
            .get_mut(&window)
            .expect("project window")
            .projects
            .as_mut()
            .expect("project panel");
        panel.phase = Phase::Confirm(
            request,
            Confirmation::Open {
                target: OpenTarget::File {
                    source,
                    name,
                    reconnect: false,
                },
                layout: Arc::new(layout),
                tabs,
                details,
                reconnect_offered: auto_reconnect,
            },
        );
        Task::none()
    }

    fn begin_project_work(&mut self, window: window::Id) -> u64 {
        let request = self.allocate();
        let panel = self
            .windows
            .get_mut(&window)
            .expect("project window")
            .projects
            .as_mut()
            .expect("project panel");
        panel.phase = Phase::Working(request);
        panel.notice = None;
        request
    }

    fn set_project_phase(&mut self, window: window::Id, phase: Phase) {
        self.windows
            .get_mut(&window)
            .expect("project window")
            .projects
            .as_mut()
            .expect("project panel")
            .phase = phase;
    }

    fn project_tab_identity(&self, window: window::Id) -> Vec<(u64, SessionId)> {
        let mut tabs: Vec<_> = self.windows[&window]
            .terminal_tabs()
            .flat_map(|tab| tab.grid.iter().map(move |(_, &session)| (tab.id, session)))
            .collect();
        tabs.sort_unstable_by_key(|(tab, session)| (*tab, session.0));
        tabs
    }

    fn open_project_layout(
        &mut self,
        owner: window::Id,
        target: OpenTarget,
        layout: Arc<WorkspaceLayout>,
        tabs: Vec<(u64, SessionId)>,
    ) -> Task<WorkspaceMessage> {
        if !self.project_open_allowed(owner) {
            return Task::none();
        }
        if let Some(guard) = self.guard_hosted_project_close(
            owner,
            target.clone(),
            Arc::clone(&layout),
            tabs.clone(),
        ) {
            return guard;
        }
        self.resume_project_after_tools(owner, target, layout, tabs, &[])
    }

    pub(super) fn resume_project_after_tools(
        &mut self,
        owner: window::Id,
        target: OpenTarget,
        layout: Arc<WorkspaceLayout>,
        expected_terminal: Vec<(u64, SessionId)>,
        approved: &[HostedTab],
    ) -> Task<WorkspaceMessage> {
        if !self.project_panel_open(owner) || !self.project_open_allowed(owner) {
            return Task::none();
        }
        if self.project_tab_identity(owner) != expected_terminal {
            self.project_notice(
                owner,
                "The window's tabs changed. Open the project again.".into(),
            );
            return Task::none();
        }
        match self.restore_layout_into_with_tools(&layout, Some(owner), approved) {
            Ok((windows, restored)) => {
                self.windows
                    .get_mut(&owner)
                    .expect("project window")
                    .projects = None;
                self.notice(owner, format!("Project {} opened.", target.name()));
                self.session_restore.changed(false);
                let recent = match target {
                    OpenTarget::Named(name) => self.remember_open_recent_project(owner, &name),
                    OpenTarget::File {
                        source, reconnect, ..
                    } => {
                        if reconnect {
                            self.queue_project_reconnects(&windows);
                        }
                        self.remember_open_recent_project_file(owner, source)
                    }
                };
                Task::batch([restored.chain(window::gain_focus(owner)), recent])
            }
            Err(error) => {
                self.project_notice(owner, format!("Could not restore project ({error})"));
                Task::none()
            }
        }
    }

    fn project_open_allowed(&mut self, owner: window::Id) -> bool {
        if self.clipboard.policy.problem.is_none() {
            return true;
        }
        self.project_notice(
            owner,
            "Managed policy could not be applied. Sessions are locked.".into(),
        );
        false
    }

    fn project_notice(&mut self, window: window::Id, notice: String) {
        if let Some(panel) = self
            .windows
            .get_mut(&window)
            .and_then(|state| state.projects.as_mut())
        {
            panel.notice = Some(notice);
        }
    }

    pub(super) fn project_view(&self, window: window::Id) -> Option<Element<'_, WorkspaceMessage>> {
        let panel = self.windows.get(&window)?.projects.as_ref()?;
        let message = move |action| wrap(window, action);
        let title = if matches!(&panel.phase, Phase::Confirm(_, Confirmation::SaveFile(_))) {
            "Project Settings"
        } else {
            panel.browser.as_ref().map_or("Projects", Browser::title)
        };
        let mut contents = column![row![
            text(title).size(22),
            space::horizontal(),
            button("Close").on_press_maybe(
                (!matches!(panel.phase, Phase::Writing(..))).then_some(message(Message::Close))
            ),
        ]]
        .spacing(10);
        if let Phase::Confirm(request, confirmation) = &panel.phase {
            let (question, action) = match confirmation {
                Confirmation::Replace { name, .. } => (
                    format!("Replace the saved project {}?", name.as_str()),
                    "Replace project",
                ),
                Confirmation::Open {
                    target,
                    tabs,
                    details,
                    reconnect_offered,
                    ..
                } => {
                    for detail in details {
                        contents = contents.push(text(detail).size(14));
                    }
                    if let OpenTarget::File {
                        source, reconnect, ..
                    } = target
                    {
                        contents =
                            contents.push(text(source.path().display().to_string()).size(13));
                        if *reconnect_offered {
                            let request = *request;
                            contents = contents
                                .push(checkbox(*reconnect).label("Reconnect eligible saved SSH sessions")
                                    .on_toggle(move |enabled| message(Message::AutoReconnect(request, enabled))))
                                .push(text("Sessions requiring a new host key or sign-in stay disconnected.").size(13));
                        }
                    }
                    if tabs.is_empty() {
                        (format!("Open {}?", target.name()), "Open project")
                    } else {
                        (
                            format!(
                                "Open {} and replace this window's tabs? Their sessions will close.",
                                target.name()
                            ),
                            "Open and replace tabs",
                        )
                    }
                }
                Confirmation::ImportJava(name, imported) => {
                    let windows = imported.layout.windows.len();
                    let tabs: usize = imported
                        .layout
                        .windows
                        .iter()
                        .map(|window| window.tabs.len())
                        .sum();
                    contents = contents
                        .push(text(format!("{windows} windows and {tabs} supported tabs")))
                        .push(text("Local panes use your default shell. SSH and serial panes open offline. Java credentials, commands and stored terminal output are excluded.").size(14))
                        .push(text("Java records the active tab. Imported splits focus pane zero or the first supported pane.").size(14));
                    for (note, count) in &imported.notes {
                        contents = contents
                            .push(text(format!("{} ({count})", note.description())).size(13));
                    }
                    (
                        format!("Save this Java import as {}?", name.as_str()),
                        "Import project",
                    )
                }
                Confirmation::ExportJava(prepared) => {
                    contents = contents
                        .push(text(format!("{} windows and {} exportable tabs", prepared.windows, prepared.tabs)))
                        .push(text("Java requires Auto-Reconnect to restore terminal tabs. When you open the file there, Java can start local shells and use its saved connection settings.").size(14));
                    if let Some(catalogue) = &prepared.catalogue {
                        contents = contents.push(
                            text(format!(
                                "Open the export with the same Java connection catalogue ({})",
                                catalogue.display()
                            ))
                            .size(13),
                        );
                    }
                    for note in &prepared.notes {
                        contents = contents.push(text(note).size(13));
                    }
                    (
                        format!(
                            "Write the Java project to {}?",
                            prepared.destination.display()
                        ),
                        "Export project",
                    )
                }
                Confirmation::SaveFile(settings) => {
                    let request = *request;
                    let form = column![
                        text(layout_summary(&settings.layout)),
                        text(format!("Saving in {}", settings.folder.path().display())).size(13),
                        text("Filename"),
                        text_input("Project filename", &settings.filename).on_input(move |value| {
                            message(Message::FileDestination(request, value))
                        }),
                        text("Project name"),
                        text_input("Project name", &settings.name)
                            .on_input(move |value| message(Message::FileName(request, value))),
                        text("Description"),
                        text_editor(&settings.description)
                            .height(94)
                            .on_action(move |action| message(Message::FileDescription(
                                request, action
                            ))),
                        checkbox(settings.auto_reconnect)
                            .label("Offer SSH auto-reconnect when opening this project")
                            .on_toggle(move |enabled| message(Message::AutoReconnect(
                                request, enabled
                            ))),
                        text("Remote panes reopen disconnected. Connect them when you are ready.")
                            .size(13),
                        row![
                            button("Cancel").on_press(message(Message::Decide(request, false))),
                            button("Save project").on_press_maybe(
                                (!settings.name.trim().is_empty()
                                    && !settings.filename.trim().is_empty())
                                .then_some(message(Message::Decide(request, true)))
                            ),
                        ]
                        .spacing(8),
                    ]
                    .spacing(8);
                    contents = contents.push(form);
                    if let Some(notice) = &panel.notice {
                        contents = contents.push(text(notice).size(13));
                    }
                    return Some(scrollable(contents.padding(16).width(Length::Fill)).into());
                }
                Confirmation::ReplaceFile { prepared, .. } => (
                    format!(
                        "Replace {} with this native kortty project?",
                        prepared.source.path().display()
                    ),
                    "Replace project",
                ),
            };
            contents = contents.push(text(question)).push(
                row![
                    button("Cancel").on_press(message(Message::Decide(*request, false))),
                    button(action).on_press(message(Message::Decide(*request, true))),
                ]
                .spacing(8),
            );
        } else if let Some(browser) = &panel.browser {
            contents = if matches!(panel.phase, Phase::Writing(..)) {
                contents.push(text("Saving project…"))
            } else {
                contents.push(
                    browser
                        .view(!matches!(panel.phase, Phase::Ready))
                        .map(move |event| message(Message::Browse(event))),
                )
            };
        } else if matches!(panel.phase, Phase::Ready) {
            let valid = Name::parse(&panel.name).is_ok();
            contents = contents
                .push(text("Save the workspace under a name or open a saved project.").size(14))
                .push(
                    text_input("Project name", &panel.name)
                        .on_input(move |name| message(Message::Name(name))),
                )
                .push(
                    row![
                        button("Save project")
                            .on_press_maybe(valid.then_some(message(Message::Save))),
                        button("Open project")
                            .on_press_maybe(valid.then_some(message(Message::Open))),
                    ]
                    .spacing(8),
                );
            for name in &panel.names {
                contents = contents.push(
                    button(text(name.as_str())).on_press(message(Message::Select(name.clone()))),
                );
            }
            let valid_exchange = valid
                && Path::new(panel.java_project.trim()).is_absolute()
                && (panel.java_connections.trim().is_empty()
                    || Path::new(panel.java_connections.trim()).is_absolute());
            contents = contents
                .push(text("Java project files").size(17))
                .push(text("Choose a project name above. SSH and serial use the selected Java connections.xml.").size(14))
                .push(text_input("Absolute .kortty input or new output path", &panel.java_project)
                    .on_input(move |path| message(Message::JavaProjectPath(path))))
                .push(text_input("Absolute path to its Java connections.xml (optional)", &panel.java_connections)
                    .on_input(move |path| message(Message::JavaConnectionsPath(path))))
                .push(row![
                    button("Review Java import").on_press_maybe(valid_exchange.then_some(message(Message::ImportJava))),
                    button("Review Java export").on_press_maybe(valid_exchange.then_some(message(Message::ExportJava))),
                ].spacing(8));
        } else {
            contents = contents.push(text("Working…"));
        }
        if let Some(notice) = &panel.notice {
            contents = contents.push(text(notice).size(13));
        }
        Some(scrollable(contents.padding(16).width(Length::Fill)).into())
    }
}

fn layout_summary(layout: &WorkspaceLayout) -> String {
    let windows = layout.windows.len();
    let tabs: usize = layout.windows.iter().map(|window| window.tabs.len()).sum();
    let window_label = if windows == 1 { "window" } else { "windows" };
    let tab_label = if tabs == 1 { "tab" } else { "tabs" };
    format!("{windows} {window_label} and {tabs} {tab_label}")
}

fn local_file_details(layout: &WorkspaceLayout) -> impl Iterator<Item = String> + '_ {
    layout
        .windows
        .iter()
        .flat_map(|window| &window.tabs)
        .filter_map(|tab| match tab {
            crate::profile::PaneLayout::LocalFile { path, .. } => {
                Some(format!("Local file tab · {}", path.display()))
            }
            crate::profile::PaneLayout::RemoteTool { target, tool } => {
                Some(remote_tool_description(target, tool))
            }
            _ => None,
        })
}

fn remote_tool_description(
    target: &super::SshTarget,
    tool: &crate::profile::RemoteProjectTool,
) -> String {
    use crate::profile::RemoteProjectTool;

    let route = target.project_route_label();
    match tool {
        RemoteProjectTool::File { path, .. } => format!("Remote file tab · {route} · {path}"),
        RemoteProjectTool::Sftp {
            local,
            remote,
            idle_minutes,
        } => {
            let local = local
                .as_deref()
                .map_or_else(|| "local home".into(), |path| path.display().to_string());
            let remote = remote.as_deref().unwrap_or("remote home");
            let idle = idle_minutes.map_or_else(String::new, |minutes| {
                format!(" · Idle close after {minutes} min")
            });
            format!("SFTP tab · {route} · {local} ↔ {remote}{idle}")
        }
    }
}

#[cfg(test)]
mod tests {
    use iced::keyboard::{Key, Location, Modifiers, key::Named};
    use iced::widget::pane_grid;
    use kortty_terminal::TerminalProfile;

    use crate::profile::{PaneLayout, WindowLayout};
    use crate::session::Status;
    use crate::session_security::IsolationLevel;

    use super::*;

    #[test]
    fn project_and_manual_shortcuts_preserve_the_ibm_host_keys() {
        use crate::session::{GridSize, IbmKind, Session};

        let mut workspace = Workspace::default();
        let (owner, _) = workspace.open_window(false);
        let session = workspace.store_session(
            Session::disconnected_ibm(IbmKind::Tn5250),
            GridSize::default(),
            14,
        );
        workspace.add_tab(owner, session);
        for (key, modifiers) in [
            (Key::Named(Named::F1), Modifiers::empty()),
            (Key::Character("s".into()), Modifiers::CTRL),
        ] {
            let _ = workspace.key(owner, key, Location::Standard, modifiers, None);
        }
        assert!(!workspace.manual_open());
        assert!(!workspace.project_panel_open(owner));
    }

    fn ssh(font: u8, focused: bool, host: &str) -> PaneLayout {
        PaneLayout::Ssh {
            font,
            focused,
            term: TerminalProfile::Xterm.term_name().into(),
            initial_size: Default::default(),
            target: serde_json::from_value(serde_json::json!({
                "host": host,
                "port": 22,
                "username": "operator",
            }))
            .unwrap(),
        }
    }

    fn saved_window(width: f32, active: usize, tabs: Vec<PaneLayout>) -> WindowLayout {
        WindowLayout {
            tab_titles: Default::default(),
            tab_groups: Default::default(),
            tab_primary_panes: Default::default(),
            tab_terminal_effects: Default::default(),
            tab_terminal_appearance: Default::default(),
            width,
            height: 610.0,
            position: None,
            maximized: false,
            dashboard_visible: None,
            active,
            tabs,
        }
    }

    fn existing(workspace: &mut Workspace, host: &str) -> (window::Id, SessionId) {
        let (window, _) = workspace.open_window(false);
        let PaneLayout::Ssh { target, .. } = ssh(14, true, host) else {
            unreachable!()
        };
        let session = workspace
            .store_disconnected_ssh(target, TerminalProfile::Xterm, 14, Default::default())
            .unwrap();
        workspace.add_tab(window, session);
        (window, session)
    }

    #[test]
    fn project_file_reconnect_request_requires_a_current_explicit_approval() {
        let mut workspace = Workspace::default();
        let (owner, _) = existing(&mut workspace, "previous.example");
        let layout = WorkspaceLayout::new(vec![saved_window(
            800.0,
            0,
            vec![ssh(14, true, "project.example")],
        )]);
        let path = std::env::temp_dir().join("project-review.kortty");
        let source = ProjectFile::new(path, None).unwrap();
        workspace.windows.get_mut(&owner).unwrap().projects = Some(Panel::default());
        let request = workspace.allocate();
        let _ = workspace.review_project_file(
            owner,
            request,
            FileProject {
                source,
                layout,
                name: "Project review".into(),
                description: String::new(),
                auto_reconnect: true,
                java_notes: Vec::new(),
            },
        );
        let approved = |workspace: &Workspace| {
            matches!(
                &workspace.windows[&owner].projects.as_ref().unwrap().phase,
                Phase::Confirm(
                    _,
                    Confirmation::Open {
                        target: OpenTarget::File {
                            reconnect: true,
                            ..
                        },
                        ..
                    }
                )
            )
        };
        assert!(!approved(&workspace));
        let _ = workspace.update(wrap(owner, Message::AutoReconnect(request + 1, true)));
        assert!(!approved(&workspace));
        let _ = workspace.update(wrap(owner, Message::AutoReconnect(request, true)));
        assert!(approved(&workspace));
        let _ = workspace.update(wrap(owner, Message::Decide(request, false)));
        assert!(workspace.project_reconnect_banner(owner).is_none());
    }

    #[test]
    fn project_file_cancelled_replace_keeps_the_draft_and_rejects_the_old_ticket() -> io::Result<()>
    {
        use std::fs;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let scratch = Scratch(std::env::temp_dir().join(format!(
            "kortty-project-dialog-{}",
            crate::private_files::nonce()?
        )));
        fs::create_dir(&scratch.0)?;
        let folder = ProjectFolder::inspect(&scratch.0)?;
        let path = folder.path().join("Release.kortty");
        fs::write(&path, b"previous project")?;
        let layout = WorkspaceLayout::new(vec![saved_window(
            800.0,
            0,
            vec![ssh(14, true, "review.example")],
        )]);
        let prepared = folder.prepare(
            "Release",
            "New title".into(),
            "New notes".into(),
            layout.clone(),
            true,
        )?;

        let mut workspace = Workspace::default();
        let (owner, _) = existing(&mut workspace, "current.example");
        let initial = workspace.allocate();
        workspace.windows.get_mut(&owner).unwrap().projects = Some(Panel {
            phase: Phase::Confirm(
                initial,
                Confirmation::SaveFile(Box::new(FileSettings {
                    folder,
                    filename: "Release".into(),
                    layout,
                    name: "New title".into(),
                    description: text_editor::Content::with_text("New notes"),
                    auto_reconnect: true,
                })),
            ),
            ..Panel::default()
        });
        let _ = workspace.update(wrap(owner, Message::Decide(initial, true)));
        let Phase::Writing(replace, _) = workspace.windows[&owner].projects.as_ref().unwrap().phase
        else {
            panic!("expected a prepared-file worker");
        };
        // Test double for Iced delivery. The prepared destination and its file stamp are real.
        let _ = workspace.update(wrap(
            owner,
            Message::Finished(replace, Ok(Outcome::FileNeedsReplace(Arc::new(prepared)))),
        ));
        let _ = workspace.update(wrap(owner, Message::Decide(replace, false)));
        let _ = workspace.update(wrap(owner, Message::Decide(replace, true)));
        let Phase::Confirm(current, Confirmation::SaveFile(draft)) =
            &workspace.windows[&owner].projects.as_ref().unwrap().phase
        else {
            panic!("expected the editable draft after cancellation");
        };
        assert_ne!(*current, replace);
        assert_eq!(draft.name, "New title");
        assert_eq!(draft.description.text().trim_end(), "New notes");
        assert!(draft.auto_reconnect);
        assert_eq!(fs::read(path)?, b"previous project");
        Ok(())
    }

    fn complete_open(
        workspace: &mut Workspace,
        window: window::Id,
        layout: WorkspaceLayout,
    ) -> u64 {
        if !workspace.project_panel_open(window) {
            let _ = workspace.update(wrap(window, Message::Show));
            let request = running(workspace, window);
            let _ = workspace.update(wrap(
                window,
                Message::Finished(request, Ok(Outcome::Listed(vec![]))),
            ));
        }
        let name = Name::parse("Release prep").unwrap();
        let _ = workspace.update(wrap(window, Message::Name(name.as_str().into())));
        let _ = workspace.update(wrap(window, Message::Open));
        let request = running(workspace, window);
        let _ = workspace.update(wrap(
            window,
            Message::Finished(request, Ok(Outcome::Loaded(name, Arc::new(layout)))),
        ));
        request
    }

    fn running(workspace: &Workspace, window: window::Id) -> u64 {
        let Phase::Working(request) = workspace.windows[&window].projects.as_ref().unwrap().phase
        else {
            panic!("expected running project operation");
        };
        request
    }

    #[test]
    fn java_project_import_restores_primary_appearance_across_splits() {
        use crate::terminal_theme::{CursorStyle, Id, Preset, Rgb};
        use iced::futures::{StreamExt, executor::block_on};

        let profile = crate::snippets::tests::Profile::new();
        let project = profile.0.join("appearance.kortty");
        let catalogue = profile.0.join("connections.xml");
        std::fs::write(&project, r#"<project><windows><window><tabs>
          <tab><connectionId>primary-local</connectionId><fontSizeOverride>22</fontSizeOverride>
            <settings><fontSize>49</fontSize></settings>
            <splitPaneState><orientation>HORIZONTAL</orientation>
              <leftChild><widgetIndex>0</widgetIndex></leftChild>
              <rightChild><widgetIndex>1</widgetIndex><connectionId>secondary-ssh</connectionId></rightChild>
            </splitPaneState>
          </tab>
          <tab><connectionId>primary-local</connectionId><localShell>true</localShell>
            <settings><fontSize>50</fontSize></settings>
          </tab>
          <tab><connectionId>global-ssh</connectionId></tab>
          <tab><connectionId>plain-ssh</connectionId></tab>
        </tabs></window></windows></project>"#).unwrap();
        std::fs::write(&catalogue, r#"<connections schemaVersion="2">
          <connection id="primary-local"><protocol>LOCAL_SHELL</protocol><settings>
            <useGlobalSettings>false</useGlobalSettings><fontSize>18</fontSize><themeId>nord</themeId>
            <backgroundColor>#112233</backgroundColor><cursorStyle>STEADY_UNDERLINE</cursorStyle>
            <terminalColumns>110</terminalColumns><terminalRows>27</terminalRows>
          </settings></connection>
          <connection id="secondary-ssh"><host>alternate.example</host><username>operator</username><settings>
            <useGlobalSettings>false</useGlobalSettings><fontSize>41</fontSize><themeId>monokai</themeId>
            <terminalColumns>96</terminalColumns><terminalRows>30</terminalRows>
          </settings></connection>
          <connection id="global-ssh"><host>global.example</host><username>operator</username><settings>
            <useGlobalSettings>true</useGlobalSettings><fontSize>46</fontSize><themeId>monokai</themeId>
          </settings></connection>
          <connection id="plain-ssh"><host>plain.example</host><username>operator</username><settings>
            <useGlobalSettings>false</useGlobalSettings><themeId>missing-theme</themeId>
            <backgroundColor>#334455</backgroundColor>
          </settings></connection>
        </connections>"#).unwrap();

        let mut workspace = Workspace::default();
        workspace.window_settings.terminal_font = 17;
        workspace
            .window_settings
            .themes
            .select(Some(Id::BuiltIn(Preset::Dark)));
        let global = workspace.window_settings.terminal_appearance();
        let nord = workspace
            .window_settings
            .themes
            .find(Id::BuiltIn(Preset::Nord))
            .unwrap()
            .scheme;
        let (owner, _) = workspace.open_window(false);
        workspace.windows.get_mut(&owner).unwrap().projects = Some(Panel::default());
        for edit in [
            Message::Name("Java appearance".into()),
            Message::JavaProjectPath(project.to_str().unwrap().into()),
            Message::JavaConnectionsPath(catalogue.to_str().unwrap().into()),
        ] {
            let _ = workspace.update(wrap(owner, edit));
        }
        let import = workspace.update(wrap(owner, Message::ImportJava));
        let mut stream = iced_runtime::task::into_stream(import).unwrap();
        let Some(iced_runtime::Action::Output(result)) = block_on(stream.next()) else {
            panic!("expected a Java project import result");
        };
        let _ = workspace.update(result);
        let Phase::Confirm(request, Confirmation::ImportJava(_, imported)) =
            &workspace.windows[&owner].projects.as_ref().unwrap().phase
        else {
            panic!("expected the imported project preview");
        };
        let request = *request;
        let source = serde_json::to_vec(&imported.layout).unwrap();
        let layout: WorkspaceLayout = serde_json::from_slice(&source).unwrap();
        let _ = workspace.update(wrap(owner, Message::Decide(request, false)));
        let _ = complete_open(&mut workspace, owner, layout.clone());

        let tabs = workspace.windows[&owner]
            .terminal_tabs()
            .collect::<Vec<_>>();
        assert_eq!(tabs.len(), 4);
        let split = tabs[0];
        assert_eq!(split.grid.len(), 2);
        for (_, &session) in split.grid.iter() {
            let pane = &workspace.sessions[&session];
            let appearance = workspace.pane_appearance(session);
            assert_eq!(appearance.background, nord.background);
            assert_eq!(
                workspace.pane_theme_id(session),
                Some(Id::BuiltIn(Preset::Nord))
            );
            assert_ne!(appearance.background, Rgb::from_hex(0x112233));
            assert_eq!(
                appearance.cursor_shape,
                CursorStyle::SteadyUnderline.behavior().0
            );
            assert_eq!(appearance.cursor_blink, global.cursor_blink);
            assert_eq!(workspace.pane_terminal_font(pane, false).0, 22);
            if let Some(ssh) = &pane.ssh {
                assert_eq!(ssh.label(), "operator@alternate.example:22");
                assert_eq!(pane.frame.status, Status::Disconnected);
                assert_eq!((pane.grid.columns, pane.grid.rows), (96, 30));
            } else {
                assert_eq!((pane.grid.columns, pane.grid.rows), (110, 27));
            }
        }
        for tab in &tabs[1..3] {
            let session = tab.focused_session().unwrap();
            assert_eq!(workspace.pane_appearance(session), global);
            assert_eq!(
                workspace.pane_theme_id(session),
                Some(Id::BuiltIn(Preset::Dark))
            );
            assert_eq!(
                workspace
                    .pane_terminal_font(&workspace.sessions[&session], false)
                    .0,
                17
            );
        }
        let plain = tabs[3].focused_session().unwrap();
        assert_eq!(workspace.pane_theme_id(plain), None);
        assert_eq!(
            workspace.pane_appearance(plain).background,
            Rgb::from_hex(0x334455)
        );
        let captured = workspace.capture_layout_ordered(Some(owner)).unwrap();
        assert_eq!(
            captured.windows[0].tab_terminal_appearance,
            layout.windows[0].tab_terminal_appearance
        );
        for session in workspace.sessions.keys().copied().collect::<Vec<_>>() {
            workspace.stop_session(session);
        }
    }

    #[test]
    fn confirmed_open_reuses_the_invoking_window_and_restores_additional_split_windows() {
        let mut workspace = Workspace::default();
        let (invoking, original) = existing(&mut workspace, "original.example");
        let (other, unrelated) = existing(&mut workspace, "unrelated.example");
        let split = PaneLayout::Split {
            vertical: true,
            ratio: 0.37,
            first: Box::new(ssh(13, false, "left.example")),
            second: Box::new(PaneLayout::Split {
                vertical: false,
                ratio: 0.66,
                first: Box::new(ssh(16, false, "top.example")),
                second: Box::new(ssh(19, true, "bottom.example")),
            }),
        };
        let mut first = saved_window(
            860.0,
            1,
            vec![ssh(15, true, "first.example"), split.clone()],
        );
        let mut second = saved_window(760.0, 0, vec![ssh(17, true, "extra.example")]);
        first.dashboard_visible = Some(false);
        second.dashboard_visible = Some(true);
        let _ = workspace.set_dashboard_visible(invoking, true);
        let layout = WorkspaceLayout::new(vec![first.clone(), second.clone()]);
        let canceled = complete_open(&mut workspace, invoking, layout.clone());
        assert_eq!(workspace.windows.len(), 2);
        assert!(workspace.sessions.contains_key(&original));
        let _ = workspace.update(wrap(invoking, Message::Decide(canceled, false)));
        let accepted = complete_open(&mut workspace, invoking, layout);
        let _ = workspace.update(wrap(invoking, Message::Decide(canceled, true)));
        assert_eq!(workspace.windows.len(), 2);
        assert!(workspace.sessions.contains_key(&original));
        let _ = workspace.update(wrap(invoking, Message::Decide(accepted, true)));

        assert_eq!(workspace.windows.len(), 3);
        assert!(!workspace.project_panel_open(invoking));
        assert!(!workspace.sessions.contains_key(&original));
        assert_eq!(
            workspace.windows[&other].tab().unwrap().focused_session(),
            Some(unrelated)
        );
        let window = &workspace.windows[&invoking];
        assert_eq!(window.active, 1);
        assert!(!window.dashboard.visible());
        assert_eq!(window.placement().width, first.width);
        let active = window.tab().unwrap();
        assert_eq!(
            workspace.capture_pane(active, active.grid.layout(), &mut Vec::new(), &mut |_| {}),
            Some(split)
        );
        assert_eq!(
            workspace.sessions[&active.focused_session().unwrap()].font,
            19
        );
        let extra = workspace
            .windows
            .iter()
            .find(|(id, _)| **id != invoking && **id != other)
            .unwrap()
            .1;
        assert_eq!(extra.placement().width, second.width);
        assert!(extra.dashboard.visible());
        let captured = workspace.capture_layout_ordered(Some(invoking)).unwrap();
        assert_eq!(captured.windows[0].dashboard_visible, Some(false));
        assert_eq!(captured.windows[2].dashboard_visible, Some(true));
        assert_eq!(
            workspace.sessions[&extra.tab().unwrap().focused_session().unwrap()].font,
            17
        );
        assert!(
            workspace
                .sessions
                .values()
                .all(|pane| pane.session.frame().status == Status::Disconnected)
        );
    }

    #[test]
    fn failed_staging_keeps_the_original_tabs_and_window_geometry() {
        let mut workspace = Workspace::default();
        let (window, original) = existing(&mut workspace, "original.example");
        let original_placement = workspace.windows[&window].placement();
        workspace.clipboard.policy.minimum_isolation = IsolationLevel::Sandbox;
        let layout = WorkspaceLayout::new(vec![
            saved_window(860.0, 0, vec![ssh(15, true, "first.example")]),
            saved_window(
                720.0,
                0,
                vec![PaneLayout::Terminal {
                    font: 14,
                    focused: true,
                    vt100: false,
                    term: None,
                    security: None,
                    initial_size: Default::default(),
                }],
            ),
        ]);
        let request = complete_open(&mut workspace, window, layout);
        let _ = workspace.update(wrap(window, Message::Decide(request, true)));
        assert_eq!(workspace.windows.len(), 1);
        assert_eq!(workspace.windows[&window].placement(), original_placement);
        assert_eq!(
            workspace.windows[&window].tab().unwrap().focused_session(),
            Some(original)
        );
        assert_eq!(workspace.sessions.len(), 1);
        assert!(
            workspace.windows[&window]
                .projects
                .as_ref()
                .unwrap()
                .notice
                .as_ref()
                .unwrap()
                .contains("choose an absolute sandbox workspace")
        );
    }

    #[test]
    fn deferred_open_rechecks_terminal_identity_and_policy_before_replacing_tabs() {
        let mut workspace = Workspace::default();
        workspace.window_settings.open_tool_windows_as_tabs = true;
        let (owner, original) = existing(&mut workspace, "original.example");
        let _ = workspace.show_openpgp(owner);
        let approved = workspace.hosted_tools_in(owner);
        assert_eq!(approved.len(), 1);
        workspace.windows.get_mut(&owner).unwrap().projects = Some(Panel::default());

        let expected = workspace.project_tab_identity(owner);
        let target = workspace.sessions[&original].ssh.clone().unwrap();
        let added = workspace
            .store_disconnected_ssh(target, TerminalProfile::Xterm, 14, Default::default())
            .unwrap();
        workspace.add_tab(owner, added);
        let layout = Arc::new(WorkspaceLayout::new(vec![saved_window(
            860.0,
            0,
            vec![ssh(15, true, "project.example")],
        )]));
        let name = Name::parse("Release prep").unwrap();
        let _ = workspace.resume_project_after_tools(
            owner,
            OpenTarget::Named(name.clone()),
            Arc::clone(&layout),
            expected,
            &approved,
        );
        assert_eq!(
            workspace.windows[&owner]
                .projects
                .as_ref()
                .unwrap()
                .notice
                .as_deref(),
            Some("The window's tabs changed. Open the project again.")
        );

        workspace.clipboard.policy.problem = Some("invalid policy fixture".into());
        let current = workspace.project_tab_identity(owner);
        let _ = workspace.resume_project_after_tools(
            owner,
            OpenTarget::Named(name),
            layout,
            current.clone(),
            &approved,
        );
        assert_eq!(
            workspace.windows[&owner]
                .projects
                .as_ref()
                .unwrap()
                .notice
                .as_deref(),
            Some("Managed policy could not be applied. Sessions are locked.")
        );
        assert_eq!(workspace.project_tab_identity(owner), current);
        assert_eq!(workspace.hosted_tools_in(owner)[0].id, approved[0].id);
        assert!(workspace.sessions.contains_key(&original));
        assert!(workspace.sessions.contains_key(&added));
    }

    #[test]
    fn capture_prioritizes_the_invoking_window_and_omits_private_tabs_and_windows() {
        let mut workspace = Workspace::default();
        let (first, _) = existing(&mut workspace, "earlier.example");
        let (invoking, original) = existing(&mut workspace, "invoking.example");
        let (private, _) = existing(&mut workspace, "private.example");
        let (empty, _) = workspace.open_window(false);
        workspace
            .windows
            .get_mut(&private)
            .unwrap()
            .tab_mut()
            .unwrap()
            .incognito = true;
        let target = workspace.sessions[&original].ssh.clone().unwrap();
        let sibling = workspace
            .store_disconnected_ssh(target, TerminalProfile::Xterm, 18, Default::default())
            .unwrap();
        let tab = workspace.active_tab_mut(invoking).unwrap();
        tab.grid
            .split(pane_grid::Axis::Vertical, tab.focused, sibling)
            .unwrap();
        let project = workspace.capture_layout_ordered(Some(invoking)).unwrap();
        assert_eq!(project.windows.len(), 2);
        assert!(matches!(
            project.windows[0].tabs[0],
            PaneLayout::Split { .. }
        ));
        assert_eq!(
            project.windows[1].tabs,
            workspace
                .capture_layout_ordered(Some(first))
                .unwrap()
                .windows[0]
                .tabs
        );
        let chosen_empty = workspace.capture_layout_ordered(Some(empty)).unwrap();
        assert!(chosen_empty.windows[0].tabs.is_empty());
    }
}
