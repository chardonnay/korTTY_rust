//! Reviewed rewrites of existing snippets in a saved project analysis.

use std::fmt::Write as _;
use std::ops::Range;

use crate::snippet_analysis::model::{ApplyRun, now_millis};
use crate::snippet_analysis::request::apply::{Progress, Request as ApplyRequest};
use crate::snippet_window::analysis::apply::{self as view, Action};

use super::*;

const MAX_FILES: usize = 12;
const MAX_PREVIEW_BYTES: usize = 4 * 1024 * 1024;

#[derive(Eq, PartialEq)]
struct Revision {
    record: SnippetId,
    files: Vec<(SnippetId, String, String)>,
}

impl Revision {
    fn ids(&self) -> Vec<SnippetId> {
        self.files.iter().map(|(id, _, _)| *id).collect()
    }
}

struct Project<'a> {
    revision: Revision,
    files: Vec<archive::AnalysisFile<'a>>,
    context: Arc<str>,
}

fn capture<'a>(window: &'a SnippetWindow, page: &Page) -> Result<Project<'a>, String> {
    let record = page.record().ok_or("Select the saved project analysis.")?;
    let snapshot = snapshot_for(window, page, Some(record))?;
    let rendered = snapshot.render()?;
    if content_hash(&rendered).map_err(|error| error.to_string())? != record.source.sha256 {
        return Err("The analysed project changed. Run the analysis again.".into());
    }
    let paths: HashSet<_> = snapshot
        .files()
        .iter()
        .map(|file| file.path.as_str())
        .collect();
    let (_, files) = scope(window, page.subject)?;
    let files: Vec<_> = files
        .into_iter()
        .filter(|file| paths.contains(file.path.as_str()))
        .collect();
    let identity = files
        .iter()
        .map(|file| {
            Ok((
                file.snippet.id(),
                file.path.clone(),
                content_hash(file.snippet.content())?,
            ))
        })
        .collect::<io::Result<_>>()
        .map_err(|error| error.to_string())?;
    Ok(Project {
        revision: Revision {
            record: record.id,
            files: identity,
        },
        files,
        context: Arc::from(rendered),
    })
}

struct Job {
    id: SnippetId,
    path: String,
    request: ApplyRequest,
}

pub(super) struct Preview {
    revision: Revision,
    config: Arc<Configuration>,
    guidance: SkillGuidance,
    profile: ProfileChoice,
    transport: local_ai::Profile,
    jobs: Vec<Job>,
    stages: usize,
    prompt: String,
    parts: Vec<Range<usize>>,
    page: usize,
}

impl Preview {
    fn new(
        window: &SnippetWindow,
        page: &Page,
        pre: SnippetId,
        config: Arc<Configuration>,
    ) -> Result<Self, String> {
        let record = page
            .record()
            .filter(|record| record.id == pre)
            .ok_or("Select the analysis with these findings.")?;
        let profile = page
            .profile_for_reference(&config, &record.provenance.profile_id)
            .cloned()
            .ok_or("Enable the AI profile that produced this report.")?;
        let (transport, limit) = config.code_analysis_transport(profile.id)?;
        let Project {
            revision,
            files,
            context,
        } = capture(window, page)?;
        if files
            .iter()
            .map(|file| file.snippet.content().chars().count())
            .sum::<usize>()
            > limit
        {
            return Err("The project source exceeds the AI profile's selection limit.".into());
        }
        let selected = super::apply::selection(page, record);
        if selected.findings.iter().any(|id| {
            record
                .analysis
                .improvements
                .iter()
                .find(|finding| finding.id == *id)
                .is_none_or(|finding| {
                    finding
                        .file
                        .as_deref()
                        .is_some_and(|path| !files.iter().any(|file| file.path == path))
                })
        }) {
            return Err("A selected finding no longer identifies an analysed file.".into());
        }
        let guidance = config.reviewed_snippet_skill_guidance(&page.selected_skills);
        let mut jobs = Vec::new();
        let mut prompt = String::new();
        let mut stages = 0;
        for file in files {
            let findings = record
                .analysis
                .improvements
                .iter()
                .filter(|finding| {
                    selected.findings.contains(&finding.id)
                        && finding.file.as_deref().is_none_or(|path| file.path == path)
                })
                .map(|finding| finding.id.clone())
                .collect();
            let selection = Selection {
                findings,
                dependencies: selected.dependencies.clone(),
                ..Selection::default()
            };
            if selection.is_empty() {
                continue;
            }
            if jobs.len() == MAX_FILES {
                return Err(format!(
                    "Select findings affecting at most {MAX_FILES} project files."
                ));
            }
            let snapshot = Snapshot::snippet(
                file.snippet.name().into(),
                file.snippet.language().into(),
                file.snippet.content().into(),
            )
            .with_snippet_file(file.path.clone(), file.executable);
            let request = ApplyRequest::new(
                snapshot,
                &record.analysis,
                selection,
                page.instructions.clone(),
                guidance.text.clone(),
                &record.source.report_language_code,
                config.code_text_language(),
            )?
            .with_profile(&transport)
            .with_project(Arc::clone(&context));
            let file_prompt = request.preview()?;
            let _ = writeln!(prompt, "\nProject file: {}\n{file_prompt}", file.path);
            if prompt.len() > MAX_PREVIEW_BYTES {
                return Err("Select fewer project files. The apply preview exceeds 4 MiB.".into());
            }
            stages += request.stage_count();
            jobs.push(Job {
                id: file.snippet.id(),
                path: file.path,
                request,
            });
        }
        if jobs.is_empty() {
            return Err("Select one or more project findings or dependency changes.".into());
        }
        Ok(Self {
            revision,
            config,
            guidance,
            profile,
            transport,
            jobs,
            stages,
            parts: super::apply::parts(&prompt),
            prompt,
            page: 0,
        })
    }

    pub(super) fn view(&self) -> Phase<'_> {
        Phase::ApplyPreview(view::Preview {
            profile: &self.profile,
            prompt: &self.prompt[self.parts[self.page].clone()],
            page: self.page,
            pages: self.parts.len(),
            stale: false,
            stages: self.stages,
            completed: 0,
            retrying: false,
            project_files: Some(self.jobs.len()),
        })
    }
}

pub(super) struct Run {
    token: u64,
    config: Arc<Configuration>,
    skill_usage: HashMap<SnippetId, SkillUsageReceipt>,
    revision: Revision,
    _cancel: local_ai::Cancel,
    pub(super) live: Option<Progress>,
}

#[derive(Clone)]
struct Proposal {
    id: SnippetId,
    path: String,
    run: ApplyRun,
    included: bool,
}

#[derive(Clone, Default)]
pub(crate) struct Proposals {
    files: Vec<Proposal>,
    failures: Vec<(String, String)>,
}

#[derive(Clone)]
pub(crate) struct Completed {
    subject: Subject,
    token: u64,
    result: Result<Proposals, String>,
}

pub(super) struct Review {
    revision: Revision,
    proposals: Proposals,
    current: usize,
    change: super::apply::Review,
}

impl Review {
    fn new(record: &Record, revision: Revision, proposals: Proposals) -> Self {
        let change = super::apply::Review::new(record, &proposals.files[0].run);
        Self {
            revision,
            proposals,
            current: 0,
            change,
        }
    }

    pub(super) fn view(&self) -> Phase<'_> {
        let file = &self.proposals.files[self.current];
        Phase::ReviewFolderApply(view::FolderReview {
            change: self.change.details(true, false),
            id: file.id,
            path: &file.path,
            index: self.current,
            total: self.proposals.files.len(),
            included: file.included,
            selected: self
                .proposals
                .files
                .iter()
                .filter(|file| file.included)
                .count(),
            failures: &self.proposals.failures,
        })
    }
}

impl Workspace {
    pub(super) fn update_folder_apply(&mut self, id: window::Id, action: Action) -> Task<Message> {
        match action {
            Action::Plan(pre, _) => return self.preview_folder_apply(id, pre),
            Action::Approve => return self.start_folder_apply(id),
            Action::Accept => return self.accept_folder_apply(id),
            _ => {}
        }
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.current_mut())
        else {
            return Task::none();
        };
        match action {
            Action::PreviewPage(forward) => {
                if let State::FolderApplyPreview(preview) = &mut page.state {
                    preview.page = super::apply::turn(preview.page, preview.parts.len(), forward);
                    return page.reset_scroll();
                }
            }
            Action::Back
                if matches!(
                    page.state,
                    State::FolderApplyPreview(_) | State::ReviewFolderApply(_)
                ) =>
            {
                page.state = State::Ready;
                page.status = None;
                return page.reset_scroll();
            }
            action => {
                let history = page.history.clone();
                let State::ReviewFolderApply(review) = &mut page.state else {
                    return Task::none();
                };
                match action {
                    Action::FolderFile(forward) => {
                        let Some(record) = history.as_deref().and_then(|history| {
                            history
                                .records
                                .iter()
                                .find(|record| record.id == review.revision.record)
                        }) else {
                            return Task::none();
                        };
                        review.current = super::apply::turn(
                            review.current,
                            review.proposals.files.len(),
                            forward,
                        );
                        review.change = super::apply::Review::new(
                            record,
                            &review.proposals.files[review.current].run,
                        );
                    }
                    Action::FolderInclude(file, included) => {
                        if let Some(file) = review
                            .proposals
                            .files
                            .iter_mut()
                            .find(|proposal| proposal.id == file)
                        {
                            file.included = included;
                        }
                    }
                    action => return review.change.navigate(id, action),
                }
            }
        }
        Task::none()
    }

    fn preview_folder_apply(&mut self, id: window::Id, pre: SnippetId) -> Task<Message> {
        let config = self.ai_configuration();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let preview = config
            .ok_or_else(|| "Open AI Manager to enable the report’s AI profile.".into())
            .and_then(|config| Preview::new(&surface.window, page, pre, config));
        match preview {
            Ok(preview) => {
                page.state = State::FolderApplyPreview(Box::new(preview));
                page.status = None;
                page.reset_scroll()
            }
            Err(error) => {
                page.refresh_record_hash(&surface.window);
                page.status = Some(error);
                Task::none()
            }
        }
    }

    fn start_folder_apply(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .current_mut()
            .filter(|page| matches!(page.state, State::FolderApplyPreview(_)))
        else {
            return Task::none();
        };
        let State::FolderApplyPreview(preview) = std::mem::replace(&mut page.state, State::Ready)
        else {
            unreachable!()
        };
        let subject = page.subject;
        let unchanged = config
            .as_ref()
            .is_some_and(|config| config.as_ref() == preview.config.as_ref())
            && capture(&surface.window, page)
                .is_ok_and(|project| project.revision == preview.revision);
        let blocked = self.folder_apply_blocked(id, &preview.revision.ids());
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.page_mut(subject))
        else {
            return Task::none();
        };
        if !unchanged || blocked {
            page.status = Some(if blocked {
                "Save or discard edits to the analysed project files before generating proposals."
            } else {
                "The project or AI settings changed. Review a new apply request."
            }.into());
            return Task::none();
        }
        let Preview {
            revision,
            transport,
            guidance,
            jobs,
            prompt,
            ..
        } = *preview;
        let config = config.expect("the approved configuration was checked");
        let skill_usage = jobs
            .iter()
            .filter_map(|job| {
                config
                    .skill_usage(&guidance, &transport, SkillUsageSurface::Workflow, false)
                    .map(|receipt| (job.id, receipt))
            })
            .collect();
        let (updates, progress) = async_channel::bounded(1);
        let mut observer = super::apply::stream_observer(
            &transport,
            &prompt,
            self.vault.as_ref(),
            self.clipboard.policy.terminal_logging.clone(),
            updates,
        );
        let local_ai::Pending { reply, cancel } = match local_ai::start(transport, move |client| {
            let mut proposals = Proposals::default();
            let total = jobs.len();
            for (index, Job { id, path, request }) in jobs.into_iter().enumerate() {
                if !client.active() {
                    return Err(local_ai::Error::Cancelled.message());
                }
                observer.file(index + 1, total);
                match request.run(client, now_millis(), &observer) {
                    Ok(run) => proposals.files.push(Proposal {
                        id,
                        path,
                        run,
                        included: false,
                    }),
                    Err(error) => proposals.failures.push((path, error)),
                }
            }
            Ok(proposals)
        }) {
            Ok(pending) => pending,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        page.state = State::FolderApplying(Box::new(Run {
            token,
            config,
            skill_usage,
            revision,
            _cancel: cancel,
            live: None,
        }));
        let complete = Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The project apply worker stopped.".into()))
            },
            move |result| {
                Message::SnippetAnalysis(
                    id,
                    super::Event::FolderApplied(Completed {
                        subject,
                        token,
                        result,
                    }),
                )
            },
        );
        Task::batch([
            complete,
            Task::run(progress, move |progress| {
                Message::SnippetAnalysis(
                    id,
                    super::Event::FolderApplyProgress(subject, token, progress),
                )
            }),
        ])
    }

    pub(super) fn snippet_folder_apply_progress(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
        progress: Progress,
    ) -> Task<Message> {
        let config = self.ai_configuration();
        if let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.page_mut(subject))
            && let State::FolderApplying(run) = &mut page.state
            && run.token == token
        {
            if super::apply::current_config(config.as_ref(), &run.config) {
                run.live = Some(progress);
            } else {
                page.state = State::Ready;
                page.status =
                    Some("The AI settings changed. Review a new project apply request.".into());
            }
        }
        Task::none()
    }

    pub(super) fn finish_folder_apply(
        &mut self,
        id: window::Id,
        completed: Completed,
    ) -> Task<Message> {
        let config = self.ai_configuration();
        let Completed {
            subject,
            token,
            result,
        } = completed;
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .page_mut(subject)
            .filter(|page| matches!(&page.state, State::FolderApplying(run) if run.token == token))
        else {
            return Task::none();
        };
        let State::FolderApplying(mut run) = std::mem::replace(&mut page.state, State::Ready)
        else {
            unreachable!()
        };
        if !super::apply::current_config(config.as_ref(), &run.config) {
            page.status =
                Some("The AI settings changed. Review a new project apply request.".into());
            return page.reset_scroll();
        }
        match result {
            Ok(proposals) if !proposals.files.is_empty() => {
                if let Some(record) = page
                    .record()
                    .filter(|record| record.id == run.revision.record)
                {
                    if capture(&surface.window, page)
                        .is_ok_and(|project| project.revision == run.revision)
                    {
                        for proposal in &proposals.files {
                            if let Some(receipt) = run.skill_usage.remove(&proposal.id) {
                                receipt.record();
                            }
                        }
                    }
                    page.state = State::ReviewFolderApply(Box::new(Review::new(
                        record,
                        run.revision,
                        proposals,
                    )));
                    page.status = None;
                }
            }
            Ok(proposals) => {
                page.status = Some(proposals.failures.first().map_or_else(
                    || "The project has no proposed changes.".into(),
                    |(path, error)| format!("No project file could be proposed. {path} · {error}"),
                ));
            }
            Err(error) => page.status = Some(error),
        }
        page.reset_scroll()
    }

    fn accept_folder_apply(&mut self, id: window::Id) -> Task<Message> {
        let Some(surface) = self.snippets.get(&id) else {
            return Task::none();
        };
        let Some(subject) = surface.analysis.active else {
            return Task::none();
        };
        let Some(page) = surface.analysis.page(subject) else {
            return Task::none();
        };
        let State::ReviewFolderApply(review) = &page.state else {
            return Task::none();
        };
        let unchanged =
            capture(&surface.window, page).is_ok_and(|project| project.revision == review.revision);
        let blocked = self.folder_apply_blocked(id, &review.revision.ids());
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.page_mut(subject) else {
            return Task::none();
        };
        let State::ReviewFolderApply(review) = &page.state else {
            return Task::none();
        };
        let selected: Vec<_> = review
            .proposals
            .files
            .iter()
            .filter(|file| file.included)
            .collect();
        if selected.is_empty() {
            return Task::none();
        }
        let count = selected.len();
        let error = if blocked {
            Some(
                "Save or discard edits to the analysed project files before accepting changes."
                    .into(),
            )
        } else if !unchanged {
            Some("The analysed project changed. The proposal cannot be saved.".into())
        } else {
            surface
                .window
                .replace_reviewed_contents(
                    selected
                        .into_iter()
                        .map(|file| (file.id, file.run.before.as_str(), file.run.after.as_str())),
                )
                .err()
                .map(|error| error.to_string())
        };
        if let Some(error) = error {
            page.status = Some(error);
            return Task::none();
        }
        page.state = State::Ready;
        page.refresh_record_hash(&surface.window);
        let unit = if count == 1 { "file" } else { "files" };
        page.status = Some(format!("Saved reviewed changes in {count} project {unit}."));
        let scroll = page.reset_scroll();
        self.snippet_library_changed(id);
        scroll
    }

    pub(super) fn folder_apply_blocked(&self, id: window::Id, files: &[SnippetId]) -> bool {
        self.snippets.get(&id).is_some_and(|origin| {
            self.snippets.values().any(|surface| {
                surface.window.analysis_library().path() == origin.window.analysis_library().path()
                    && surface.window.folder_apply_blocked(files)
            })
        })
    }
}

#[cfg(test)]
mod tests;
