use std::path::PathBuf;
use std::sync::Weak;

use iced::{Size, window};

use super::{Chart, DiagramWindow, ai};
use crate::snippets::SnippetId;
use crate::snippets::diagrams::{Family, Saved, Source};

pub(crate) struct SavedSource {
    pub(crate) owner: window::Id,
    pub(crate) session: Weak<()>,
    pub(crate) path: PathBuf,
    pub(crate) snippet: SnippetId,
    pub(crate) diagram: Saved,
}

#[derive(Clone)]
pub(crate) struct Draft {
    pub(crate) path: PathBuf,
    pub(crate) snippet: SnippetId,
    pub(crate) source: Source,
    pub(crate) previous: Option<Saved>,
    pub(crate) title: String,
    pub(crate) instructions: String,
}

impl DiagramWindow {
    pub(crate) fn with_snippet_draft(size: Size, ai_allowed: bool, draft: Draft) -> Self {
        let mut window = Self::with_script(size, ai_allowed, &draft.source.text);
        if let Some(previous) = &draft.previous {
            window.ai.family = match previous.family {
                Family::LogicalStructure => ai::Family::Flowchart,
                Family::Sequence => ai::Family::Sequence,
                Family::State => ai::Family::State,
                Family::Class => ai::Family::Class,
                Family::Er => ai::Family::EntityRelationship,
            };
            window.show_ai = ai_allowed && previous.family != Family::LogicalStructure;
        }
        window.snippet = Some(draft);
        window
    }

    pub(crate) fn with_library_chart(
        size: Size,
        source: Option<&str>,
        chart: Chart,
        navigation: SavedSource,
    ) -> Self {
        let mut window = Self::with_saved_chart(size, source, chart);
        let saved = &navigation.diagram;
        window.saved_snippet_title = Some(format!("{} · {}", saved.title, saved.scope));
        if source.is_none() {
            window.status = Some("Source changed. Regenerate to restore source links.".into());
        } else {
            window.saved_snippet_source = Some(navigation);
        }
        window
    }

    pub(crate) fn saved_snippet_source(&self) -> Option<&SavedSource> {
        self.saved_snippet_source.as_ref()
    }

    pub(super) fn regenerating_snippet(&self) -> bool {
        self.snippet
            .as_ref()
            .is_some_and(|draft| draft.previous.is_some())
    }

    pub(super) fn can_render_locally(&self) -> bool {
        !self.saved
            && self
                .snippet
                .as_ref()
                .and_then(|draft| draft.previous.as_ref())
                .is_none_or(|previous| previous.family == Family::LogicalStructure)
    }

    pub(super) fn snippet_instructions(&self) -> &str {
        self.snippet
            .as_ref()
            .map_or("", |draft| &draft.instructions)
    }

    pub(crate) fn snippet_saving(&self) -> bool {
        self.snippet_saving
    }

    pub(crate) fn begin_snippet_save(&mut self) -> Result<(Draft, Saved), String> {
        let draft = self
            .snippet
            .as_ref()
            .ok_or("This window has no snippet diagram draft.")?;
        let chart = self
            .chart
            .as_ref()
            .ok_or("Generate a diagram before saving.")?;
        let saved = Saved::capture(
            chart,
            &draft.source,
            &draft.title,
            &draft.instructions,
            draft.previous.as_ref(),
        )?;
        self.snippet_saving = true;
        self.status = Some("Saving diagram to the snippet…".into());
        Ok((draft.clone(), saved))
    }

    pub(crate) fn finish_snippet_save(&mut self, result: Result<Saved, String>) {
        self.snippet_saving = false;
        match result {
            Ok(saved) => {
                self.saved = true;
                self.show_ai = false;
                self.ai_allowed = false;
                self.saved_snippet_title = Some(format!("{} · {}", saved.title, saved.scope));
                self.snippet = None;
                self.status = Some("Diagram saved to the snippet".into());
            }
            Err(error) => self.status = Some(error),
        }
    }
}
