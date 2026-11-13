use std::{fmt, io, path::PathBuf, sync::Arc};

use iced::{Size, Task, window};

use super::{DialogKind, Message, Workspace, file_task};
use crate::diagram_window::{
    DiagramWindow,
    snippet::{Draft, SavedSource},
};
use crate::snippet_window::{
    Message as SnippetMessage,
    diagrams::{self as ui, Request},
};
use crate::snippets::{Library, Snippet, SnippetId, diagrams};

const SIZE: Size = Size::new(1050.0, 660.0);

#[derive(Clone, Copy)]
enum Target {
    Save(window::Id),
    Delete(window::Id),
}

struct Commit {
    baseline: Library,
    library: Library,
    saved: Option<diagrams::Saved>,
}

#[derive(Clone)]
pub struct Event {
    target: Target,
    result: Result<Arc<Commit>, String>,
}

impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Snippet diagram persistence completed")
    }
}

impl Workspace {
    pub(super) fn open_library_diagram(
        &mut self,
        owner: window::Id,
        request: Request,
    ) -> Task<Message> {
        let Some(surface) = self.snippets.get(&owner) else {
            return Task::none();
        };
        let library = surface.window.analysis_library();
        if let Request::Delete(snippet, id) = request {
            let Ok((_, saved)) = stored(library, snippet, id) else {
                return self.update_snippets(
                    owner,
                    delete_finished(Err("The diagram no longer exists.".into())),
                );
            };
            let saved = saved.clone();
            return change_task(
                Some(library.clone()),
                library.path().to_owned(),
                Target::Delete(owner),
                move |library| {
                    library.delete_diagram(snippet, &saved)?;
                    Ok(None)
                },
            );
        }
        let ai_allowed = self.ai_features_enabled();
        let result = match request {
            Request::Open(snippet, id) => {
                stored(library, snippet, id).and_then(|(entry, saved)| {
                    let (chart, source) = saved.restore(entry.content())?;
                    Ok(DiagramWindow::with_library_chart(
                        SIZE,
                        source,
                        chart,
                        SavedSource {
                            owner,
                            session: surface.window.diagram_session(),
                            path: library.path().to_owned(),
                            snippet,
                            diagram: saved.clone(),
                        },
                    ))
                })
            }
            Request::Create(snippet, scope) => capture(library, snippet, scope, None)
                .map(|draft| DiagramWindow::with_snippet_draft(SIZE, ai_allowed, draft)),
            Request::Regenerate(snippet, id, scope) => stored(library, snippet, id)
                .and_then(|(entry, saved)| {
                    if (saved.scope == diagrams::Scope::Whole || saved.is_current(entry.content()))
                        && saved.scope != scope
                    {
                        return Err(
                            "The saved source scope changed. Open regeneration again.".into()
                        );
                    }
                    capture(library, snippet, scope, Some(saved.clone()))
                })
                .map(|draft| DiagramWindow::with_snippet_draft(SIZE, ai_allowed, draft)),
            Request::Delete(..) => unreachable!("deletion dispatched above"),
        };
        match result {
            Ok(mut diagram) => {
                diagram.configure_ai(ai_allowed, self.ai_configuration());
                let (id, opened) = self.open_placed_dialog(
                    DialogKind::Diagram,
                    window::Settings {
                        size: SIZE,
                        min_size: Some(Size::new(600.0, 320.0)),
                        exit_on_close_request: false,
                        ..window::Settings::default()
                    },
                );
                self.diagrams.insert(id, diagram);
                opened.map(|_| Message::Noop)
            }
            Err(error) => {
                if let Some(surface) = self.snippets.get_mut(&owner) {
                    surface.window.analysis_notice(error);
                }
                Task::none()
            }
        }
    }

    pub(super) fn jump_from_library_diagram(&mut self, id: window::Id) -> Option<Task<Message>> {
        let window = self.diagrams.get(&id)?;
        let source = window.saved_snippet_source()?;
        let range = window.pointed_source()?;
        let owner = source.owner;
        if self.hosted_close_locks(owner) {
            return None;
        }
        let surface = self.snippets.get_mut(&owner)?;
        if surface.analysis.export_open()
            || surface.transfer.is_some()
            || surface.window.latest_library().path() != source.path.as_path()
            || !surface.window.jump_to_library_diagram_range(
                source.snippet,
                &source.session,
                &source.diagram,
                range,
            )
        {
            return None;
        }
        surface.analysis.editor();
        let select = surface
            .window
            .focus_editor()
            .map(move |action| Message::Snippets(owner, action));
        let focus = self
            .focus_hosted_tool(owner)
            .unwrap_or_else(|| window::gain_focus(owner));
        Some(Task::batch([focus, select]))
    }

    pub(in crate::workspace) fn save_library_diagram(&mut self, id: window::Id) -> Task<Message> {
        let Some(diagram) = self.diagrams.get_mut(&id) else {
            return Task::none();
        };
        if diagram.snippet_saving() {
            return Task::none();
        }
        let (draft, saved) = match diagram.begin_snippet_save() {
            Ok(request) => request,
            Err(error) => {
                diagram.finish_snippet_save(Err(error));
                return Task::none();
            }
        };
        let baseline = self
            .snippets
            .values()
            .map(|surface| surface.window.latest_library())
            .find(|library| library.path() == draft.path)
            .cloned();
        change_task(baseline, draft.path, Target::Save(id), move |library| {
            library.save_diagram(
                draft.snippet,
                &draft.source,
                saved.clone(),
                draft.previous.as_ref(),
            )?;
            Ok(Some(saved))
        })
    }

    pub(in crate::workspace) fn snippet_diagram_event(&mut self, event: Event) -> Task<Message> {
        if let Ok(commit) = &event.result {
            for surface in self.snippets.values_mut() {
                if surface
                    .window
                    .latest_library()
                    .same_revision(&commit.baseline)
                {
                    surface.window.refresh_shared(commit.library.clone());
                }
            }
        }
        match event.target {
            Target::Save(id) => {
                if let Some(diagram) = self
                    .diagrams
                    .get_mut(&id)
                    .filter(|diagram| diagram.snippet_saving())
                {
                    diagram.finish_snippet_save(event.result.and_then(|commit| {
                        commit
                            .saved
                            .clone()
                            .ok_or_else(|| "The saved diagram is unavailable.".into())
                    }));
                }
                Task::none()
            }
            Target::Delete(id) => {
                self.update_snippets(id, delete_finished(event.result.map(|_| ())))
            }
        }
    }

    pub(in crate::workspace) fn snippet_diagram_pending_window(&self) -> Option<window::Id> {
        self.diagrams
            .iter()
            .find_map(|(&id, diagram)| diagram.snippet_saving().then_some(id))
            .or_else(|| {
                self.snippets
                    .iter()
                    .find_map(|(&id, surface)| surface.window.diagram_deleting().then_some(id))
            })
    }
}

fn delete_finished(result: Result<(), String>) -> SnippetMessage {
    SnippetMessage::DiagramLibrary(ui::Message::DeleteFinished(result))
}

fn change_task(
    baseline: Option<Library>,
    path: PathBuf,
    target: Target,
    apply: impl FnOnce(&mut Library) -> io::Result<Option<diagrams::Saved>> + Send + 'static,
) -> Task<Message> {
    file_task(
        move || {
            let baseline = match baseline {
                Some(library) => library,
                None => Library::open(path)?,
            };
            let mut library = baseline.clone();
            let saved = apply(&mut library)?;
            Ok(Arc::new(Commit {
                baseline,
                library,
                saved,
            }))
        },
        move |result| Message::SnippetDiagram(Event { target, result }),
    )
}

fn stored(
    library: &Library,
    snippet: SnippetId,
    id: diagrams::Id,
) -> Result<(&Snippet, &diagrams::Saved), String> {
    let snippet = library
        .get(snippet)
        .ok_or("The snippet no longer exists.")?;
    let diagram = snippet
        .metadata()
        .diagrams
        .iter()
        .find(|saved| saved.id == id)
        .ok_or("The diagram no longer exists.")?;
    Ok((snippet, diagram))
}

fn capture(
    library: &Library,
    snippet: SnippetId,
    scope: diagrams::Scope,
    previous: Option<diagrams::Saved>,
) -> Result<Draft, String> {
    let entry = library
        .get(snippet)
        .ok_or("The snippet no longer exists.")?;
    if previous.is_none() && entry.metadata().diagrams.len() >= diagrams::MAX_DIAGRAMS {
        return Err("A snippet can have at most 16 saved diagrams.".into());
    }
    let source = diagrams::Source::capture(entry.content(), scope)?;
    Ok(Draft {
        path: library.path().to_owned(),
        snippet,
        source,
        title: previous
            .as_ref()
            .map_or_else(String::new, |saved| saved.title.clone()),
        instructions: previous
            .as_ref()
            .map_or_else(String::new, |saved| saved.instructions.clone()),
        previous,
    })
}

#[cfg(test)]
mod tests {
    use iced::futures::{StreamExt as _, executor::block_on};
    use iced::widget::text_editor;
    use kortty_diagram::SourceRange;

    use super::*;
    use crate::diagram_window;
    use crate::snippet_window::SnippetWindow;
    use crate::snippets::{Input, tests::Profile};

    fn output(task: Task<Message>) -> Message {
        let mut actions = iced_runtime::task::into_stream(task).expect("diagram task");
        while let Some(action) = block_on(actions.next()) {
            if let iced_runtime::Action::Output(message) = action {
                return message;
            }
        }
        panic!("diagram task produced no result");
    }

    #[test]
    fn saved_diagram_click_requires_original_editor_and_current_scoped_source() {
        let profile = Profile::new();
        let snippet = Library::open(profile.path())
            .unwrap()
            .save(
                None,
                Input {
                    name: "Shell".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "echo outside\necho selected\necho tail".into(),
                },
            )
            .unwrap();
        let owner = window::Id::unique();
        let mut editor = SnippetWindow::open(profile.path()).unwrap();
        assert!(editor.jump_to_analysis_range(snippet, 2, 2));
        let mut workspace = Workspace {
            snippets: [(owner, super::super::Surface::new(editor, owner))].into(),
            ..Workspace::default()
        };
        let route = SnippetMessage::DiagramLibrary;
        let _ = workspace.update_snippets(owner, route(ui::Message::Toggle));
        let _ = workspace.update_snippets(owner, route(ui::Message::Create(true)));
        let &draft = workspace.diagrams.keys().next().unwrap();
        let rendered =
            output(workspace.update(Message::Diagram(draft, diagram_window::Message::Render)));
        let _ = workspace.update(rendered);
        let saved = output(workspace.update(Message::Diagram(
            draft,
            diagram_window::Message::SaveToSnippet,
        )));
        let _ = workspace.update(saved);
        workspace.diagrams.clear();

        let selection = |workspace: &Workspace, editor| {
            workspace.snippets[&editor].window.diagram_selection_text()
        };
        let select_outside = |workspace: &mut Workspace, editor| {
            assert!(
                workspace
                    .snippets
                    .get_mut(&editor)
                    .unwrap()
                    .window
                    .jump_to_analysis_range(snippet, 1, 1)
            );
        };
        select_outside(&mut workspace, owner);
        let _ = workspace.update_snippets(owner, route(ui::Message::Open));
        let &id = workspace.diagrams.keys().next().unwrap();
        let point = workspace.diagrams[&id]
            .linked_node_center(SourceRange::new(1, 1).unwrap())
            .unwrap();
        let _ = workspace.update(Message::Diagram(
            id,
            diagram_window::Message::PreviewPointer(Some(point)),
        ));
        let click = || Message::Diagram(id, diagram_window::Message::JumpToSource);
        let _ = workspace.update(click());
        assert_eq!(
            selection(&workspace, owner).as_deref(),
            Some("echo selected")
        );

        let _ = workspace.update_snippets(
            owner,
            SnippetMessage::Edit(text_editor::Action::Edit(text_editor::Edit::Paste(
                Arc::new("echo changed".into()),
            ))),
        );
        select_outside(&mut workspace, owner);
        let _ = workspace.update(click());
        assert_eq!(
            selection(&workspace, owner).as_deref(),
            Some("echo outside")
        );

        workspace.snippets.remove(&owner);
        for replacement in [window::Id::unique(), owner] {
            let editor = SnippetWindow::open(profile.path()).unwrap();
            workspace
                .snippets
                .insert(replacement, super::super::Surface::new(editor, replacement));
            select_outside(&mut workspace, replacement);
            let _ = workspace.update(click());
            assert_eq!(
                selection(&workspace, replacement).as_deref(),
                Some("echo outside")
            );
            workspace.snippets.remove(&replacement);
        }
    }

    #[test]
    fn selected_diagram_uses_only_selected_lines_and_reopens_from_disk() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let snippet = library
            .save(
                None,
                Input {
                    name: "Shell".into(),
                    language: "bash".into(),
                    category: String::new(),
                    content: "echo private\r\necho selected\r\necho private again".into(),
                },
            )
            .unwrap();
        let owner = window::Id::unique();
        let mut editor = SnippetWindow::open(profile.path()).unwrap();
        let original = library.get(snippet).unwrap().shared_content();
        let old_draft = crate::snippet_window::draft_store::Snapshot {
            key: snippet,
            saved_id: Some(snippet),
            saved_at: 1,
            name: "Shell".into(),
            language: "bash".into(),
            category: String::new(),
            metadata: crate::snippets::Metadata {
                description: "Recovered description".into(),
                ..Default::default()
            },
            content: original.clone(),
            base_hash: Some(crate::snippet_window::draft_store::hash_content(&original).unwrap()),
        };
        assert!(editor.jump_to_analysis_range(snippet, 2, 2));
        let mut workspace = Workspace {
            snippets: [(owner, super::super::Surface::new(editor, owner))].into(),
            ..Workspace::default()
        };
        let route = SnippetMessage::DiagramLibrary;
        let _ = workspace.update_snippets(owner, route(ui::Message::Toggle));
        let _ = workspace.update_snippets(owner, route(ui::Message::Create(true)));
        let &id = workspace.diagrams.keys().next().unwrap();
        assert_eq!(workspace.diagrams[&id].source(), "echo selected");
        let rendered =
            output(workspace.update(Message::Diagram(id, diagram_window::Message::Render)));
        let _ = workspace.update(rendered);
        assert!(
            workspace.diagrams[&id]
                .mermaid()
                .unwrap()
                .contains("echo selected")
        );

        let saving = workspace.update(Message::Diagram(id, diagram_window::Message::SaveToSnippet));
        assert_eq!(workspace.snippet_diagram_pending_window(), Some(id));
        let _ = workspace.update(Message::Diagram(id, diagram_window::Message::Close));
        assert!(workspace.diagrams.contains_key(&id));
        let _ = workspace.update(output(saving));
        assert_eq!(workspace.snippet_diagram_pending_window(), None);
        workspace
            .snippets
            .get_mut(&owner)
            .unwrap()
            .window
            .apply_recovered(&old_draft, false)
            .unwrap();
        let _ = workspace.update_snippets(owner, SnippetMessage::Save);
        let mut reopened = Library::open(profile.path()).unwrap();
        let entry = reopened.get(snippet).unwrap();
        assert_eq!(entry.metadata().description, "Recovered description");
        let diagram = &entry.metadata().diagrams[0];
        assert_eq!(diagram.scope, diagrams::Scope::Lines { start: 2, end: 2 });
        assert!(diagram.mermaid.contains("echo selected"));
        assert!(!diagram.mermaid.contains("private"));

        let original = entry.shared_content();
        let replacement = "# inserted\r\necho private\r\necho selected\r\necho private again";
        reopened
            .replace_contents([(snippet, original.as_ref(), replacement)])
            .unwrap();
        workspace.diagrams.clear();
        workspace
            .snippets
            .get_mut(&owner)
            .unwrap()
            .window
            .refresh_shared(reopened);
        let _ = workspace.update_snippets(owner, route(ui::Message::Open));
        assert_eq!(
            workspace.diagrams.values().next().unwrap().source(),
            "echo selected"
        );
        workspace.diagrams.clear();
        let _ = workspace.update_snippets(owner, route(ui::Message::Regenerate));
        assert_eq!(
            workspace.diagrams.values().next().unwrap().source(),
            "echo selected"
        );

        let _ = workspace.update_snippets(owner, route(ui::Message::Delete));
        let deleting = workspace.update_snippets(owner, route(ui::Message::ConfirmDelete(true)));
        assert_eq!(workspace.snippet_diagram_pending_window(), Some(owner));
        let _ = workspace.update(output(deleting));
        assert_eq!(workspace.snippet_diagram_pending_window(), None);
        let reopened = Library::open(profile.path()).unwrap();
        assert!(
            reopened
                .get(snippet)
                .unwrap()
                .metadata()
                .diagrams
                .is_empty()
        );
    }
}
