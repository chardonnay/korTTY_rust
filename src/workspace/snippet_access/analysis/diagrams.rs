//! Per-record flowchart approval, generation and native viewing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::{Size, Task, widget::svg, window};
use kortty_diagram::Theme;

use crate::diagram_window::DiagramWindow;
use crate::snippet_window::Message as SnippetMessage;
use crate::workspace::dialog_placement;

use super::{
    AnalysisDiagram, AnalysisFlowchartRequest, Configuration, Event, HistoryChange, Message, Phase,
    PreviewPart, ProfileChoice, ReviewMode, SnippetId, State, Subject, Workspace, history_task,
    local_ai, preview_parts, preview_phase, record_hash, scope, store,
};

pub(super) struct Approved {
    pub(super) request: AnalysisFlowchartRequest,
    pub(super) config: Arc<Configuration>,
    pub(super) profile: ProfileChoice,
    pub(super) transport: local_ai::Profile,
    pub(super) source_hash: String,
}

pub(super) struct Review {
    approved: Approved,
    record: SnippetId,
    pub(super) parts: Vec<PreviewPart>,
    pub(super) page: usize,
}

impl Review {
    pub(super) fn preview(&self) -> Phase<'_> {
        preview_phase(
            &self.parts,
            self.page,
            None,
            Some(&self.approved.request),
            None,
            ReviewMode::Flowchart(&self.approved.profile),
        )
    }
}

pub(super) struct Run {
    token: u64,
    record: SnippetId,
    source_hash: String,
    expected: Option<AnalysisDiagram>,
    _cancel: local_ai::Cancel,
}

impl Workspace {
    pub(super) fn review_snippet_diagram(&mut self, id: window::Id) -> Task<Message> {
        let config = self.ai_configuration();
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.current_mut())
            .filter(|page| matches!(page.state, State::Ready))
        else {
            return Task::none();
        };
        page.configure(config.as_deref());
        let review = (|| {
            let config = config.ok_or("AI features are disabled or unavailable.")?;
            let record = page.record().ok_or("Select a saved analysis first.")?;
            let profile = page
                .profile_for_reference(&config, &record.provenance.profile_id)
                .filter(|profile| !profile.remote && !profile.cli)
                .cloned()
                .ok_or("The analysis AI profile is unavailable.")?;
            let (transport, limit) = config.code_analysis_transport(profile.id)?;
            let source = record
                .source
                .content
                .as_ref()
                .ok_or("The analysed source was not stored.")?;
            if source.chars().count() > limit {
                return Err("The analysed source exceeds the AI profile's selection limit.".into());
            }
            let request = AnalysisFlowchartRequest::new(
                Arc::clone(source),
                matches!(page.subject, Subject::Folder(_)),
            )?
            .with_profile(&transport);
            let parts = preview_parts(None, Some(&request), None);
            Ok(Review {
                approved: Approved {
                    request,
                    config,
                    profile,
                    transport,
                    source_hash: record.source.sha256.clone(),
                },
                record: record.id,
                parts,
                page: 0,
            })
        })();
        match review {
            Ok(review) => {
                page.status = None;
                page.state = State::DiagramReview(Box::new(review));
                page.reset_scroll()
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    pub(super) fn start_reviewed_snippet_diagram(&mut self, id: window::Id) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.current_mut())
            .filter(|page| matches!(page.state, State::DiagramReview(_)))
        else {
            return Task::none();
        };
        let State::DiagramReview(review) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked diagram review");
        };
        let subject = page.subject;
        self.start_approved_snippet_diagram(id, subject, review.record, review.approved)
    }

    pub(super) fn start_approved_snippet_diagram(
        &mut self,
        id: window::Id,
        subject: Subject,
        record: SnippetId,
        approved: Approved,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining || self.hosted_close_locks(id) {
            return Task::none();
        }
        let config = self.ai_configuration();
        let token = self.allocate();
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface.analysis.page_mut(subject) else {
            return Task::none();
        };
        let source = (|| {
            if config
                .as_ref()
                .is_none_or(|config| !Arc::ptr_eq(config, &approved.config))
            {
                return Err("The AI settings changed. Review the flowchart request again.".into());
            }
            scope(&surface.window, subject)?;
            let record = page
                .history
                .as_ref()
                .and_then(|history| history.records.iter().find(|entry| entry.id == record))
                .ok_or("The analysis record no longer exists.")?;
            if record.source.sha256 != approved.source_hash {
                return Err("The approved flowchart source is no longer available.".into());
            }
            Ok((
                record.source.line_count,
                record.source.sha256.clone(),
                record.diagram.clone(),
            ))
        })();
        let (line_count, source_hash, expected) = match source {
            Ok(source) => source,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let pending = match approved.request.start(approved.transport) {
            Ok(pending) => pending,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let local_ai::Pending { reply, cancel } = pending;
        page.state = State::Drawing(Box::new(Run {
            token,
            record,
            source_hash,
            expected,
            _cancel: cancel,
        }));
        Task::perform(
            async move {
                reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The flowchart worker stopped.".into()))
                    .and_then(|chart| AnalysisDiagram::capture(&chart, line_count))
            },
            move |result| Message::SnippetAnalysis(id, Event::Diagrammed(subject, token, result)),
        )
    }

    pub(super) fn finish_snippet_diagram(
        &mut self,
        id: window::Id,
        subject: Subject,
        token: u64,
        result: Result<AnalysisDiagram, String>,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get_mut(&id) else {
            return Task::none();
        };
        let Some(page) = surface
            .analysis
            .page_mut(subject)
            .filter(|page| matches!(&page.state, State::Drawing(run) if run.token == token))
        else {
            return Task::none();
        };
        let State::Drawing(run) = std::mem::replace(&mut page.state, State::Ready) else {
            unreachable!("checked flowchart run");
        };
        let save = result.and_then(|diagram| {
            scope(&surface.window, subject)?;
            let store = store(&surface.window).map_err(|error| error.to_string())?;
            Ok((store, diagram))
        });
        match save {
            Ok((store, diagram)) => {
                let Run {
                    record,
                    source_hash,
                    expected,
                    ..
                } = *run;
                let cancelled = Arc::new(AtomicBool::new(false));
                page.state =
                    State::HistoryWork(token, "Saving flowchart…", Some(Arc::clone(&cancelled)));
                history_task(id, subject, token, HistoryChange::Diagram, move || {
                    store.set_diagram_while(
                        subject,
                        record,
                        &source_hash,
                        expected.as_ref(),
                        diagram,
                        || !cancelled.load(Ordering::Acquire),
                    )
                })
            }
            Err(error) => {
                page.status = Some(error);
                Task::none()
            }
        }
    }

    pub(super) fn open_saved_snippet_diagram(&mut self, id: window::Id) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.current_mut())
        else {
            return Task::none();
        };
        if let Some(id) = page
            .diagram_window
            .filter(|id| self.diagrams.contains_key(id))
        {
            return window::gain_focus(id);
        }
        let subject = page.subject;
        let Some(record) = page.record() else {
            return Task::none();
        };
        let Some(diagram) = &record.diagram else {
            return Task::none();
        };
        let source = record.source.content.clone();
        let chart = match diagram.chart(record.source.line_count, source.is_some()) {
            Ok(chart) => chart,
            Err(error) => {
                page.status = Some(error);
                return Task::none();
            }
        };
        let size = Size::new(1050.0, 660.0);
        let (diagram_id, opened) = self.open_placed_dialog(
            dialog_placement::Kind::Diagram,
            window::Settings {
                size,
                min_size: Some(Size::new(600.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.diagrams.insert(
            diagram_id,
            DiagramWindow::with_saved_chart(size, source.as_deref(), chart),
        );
        if let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.page_mut(subject))
        {
            page.diagram_window = Some(diagram_id);
        }
        opened.map(|_| Message::Noop)
    }

    pub(super) fn sync_saved_snippet_diagram(
        &mut self,
        id: window::Id,
        subject: Subject,
    ) -> Task<Message> {
        let Some(page) = self
            .snippets
            .get_mut(&id)
            .and_then(|surface| surface.analysis.page_mut(subject))
        else {
            return Task::none();
        };
        let record = page.record();
        let saved = record.and_then(|record| record.diagram.as_ref());
        let diagram_id = page
            .diagram_window
            .filter(|id| self.diagrams.contains_key(id));
        let rendered = saved
            .and_then(|diagram| diagram.parse().ok())
            .map(|diagram| {
                svg::Handle::from_memory(diagram.layout().to_svg(Theme::Light).into_bytes())
            });
        if let Some(id) = diagram_id
            && let Some(record) = record
            && let Some(saved) = saved
            && let Ok(chart) =
                saved.chart(record.source.line_count, record.source.content.is_some())
            && let Some(window) = self.diagrams.get_mut(&id)
        {
            window.replace_saved_chart(record.source.content.as_deref(), chart);
            page.diagram_preview = rendered;
            return Task::none();
        }
        page.diagram_window = None;
        page.diagram_preview = rendered;
        diagram_id.map_or_else(Task::none, |id| self.close_window(id))
    }

    pub(in crate::workspace) fn jump_from_saved_snippet_diagram(
        &mut self,
        diagram: window::Id,
    ) -> Option<Task<Message>> {
        if let Some(jump) = self.jump_from_library_diagram(diagram) {
            return Some(jump);
        }
        let range = self.diagrams.get(&diagram)?.pointed_source()?;
        let (owner, page) = self.snippets.iter().find_map(|(&id, surface)| {
            surface
                .analysis
                .pages
                .iter()
                .find(|page| page.diagram_window == Some(diagram))
                .map(|page| (id, page))
        })?;
        let Subject::Snippet(snippet) = page.subject else {
            return None;
        };
        let editor = self
            .snippet_editor_elsewhere(owner, snippet)
            .unwrap_or(owner);
        let surface = self.snippets.get(&editor)?;
        let record = page.record()?;
        if surface.analysis.export_open()
            || surface.transfer.is_some()
            || self.hosted_close_locks(editor)
            || record_hash(&surface.window, page, record).as_deref()
                != Some(record.source.sha256.as_str())
        {
            return None;
        }
        let open = self.update_snippets(owner, SnippetMessage::Open(snippet));
        let Some(surface) = self.snippets.get_mut(&editor) else {
            return Some(open);
        };
        if !surface.window.jump_to_analysis_range(
            snippet,
            range.start() as usize,
            range.end() as usize,
        ) {
            return Some(open);
        }
        surface.analysis.editor();
        let select = surface
            .window
            .focus_editor()
            .map(move |action| Message::Snippets(editor, action));
        let focus = self
            .focus_hosted_tool(editor)
            .unwrap_or_else(|| window::gain_focus(editor));
        Some(Task::batch([open, focus, select]))
    }
}
