//! Editor ownership and the single reusable preview inside a snippet workspace.

use super::{Draft, Library, Snippet, SnippetId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Id(u64);

struct Parked {
    id: Id,
    preview: bool,
    draft: Draft,
}

pub(super) struct Tabs {
    active: Id,
    preview: bool,
    next: u64,
    empty: bool,
    parked: Vec<Parked>,
}

pub(super) struct Label<'a> {
    pub id: Id,
    pub name: &'a str,
    pub preview: bool,
    pub dirty: bool,
    pub active: bool,
    pub saved: Option<SnippetId>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self {
            active: Id(0),
            preview: true,
            next: 1,
            empty: false,
            parked: Vec::new(),
        }
    }
}

impl Tabs {
    pub(super) fn active(&self) -> Id {
        self.active
    }

    pub(super) fn contains(&self, id: Id) -> bool {
        (!self.empty && self.active == id) || self.parked.iter().any(|tab| tab.id == id)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.empty
    }

    pub(super) fn is_preview(&self) -> bool {
        self.preview
    }

    pub(super) fn pin(&mut self) {
        self.empty = false;
        self.preview = false;
    }

    pub(super) fn dirty(&self, draft: &Draft) -> bool {
        draft.dirty || self.parked.iter().any(|tab| tab.draft.dirty)
    }

    pub(super) fn labels<'a>(&'a self, draft: &'a Draft) -> Vec<Label<'a>> {
        if self.empty {
            return Vec::new();
        }
        let label = |id, preview, draft: &'a Draft, active| Label {
            id,
            name: &draft.name,
            preview,
            dirty: draft.dirty,
            active,
            saved: draft.id,
        };
        let mut labels = vec![label(self.active, self.preview, draft, true)];
        labels.extend(
            self.parked
                .iter()
                .map(|tab| label(tab.id, tab.preview, &tab.draft, false)),
        );
        labels.sort_by_key(|tab| tab.id);
        labels
    }

    pub(super) fn select(&mut self, draft: &mut Draft, id: Id) {
        if let Some(tab) = self.parked.iter_mut().find(|tab| tab.id == id) {
            std::mem::swap(draft, &mut tab.draft);
            std::mem::swap(&mut self.active, &mut tab.id);
            std::mem::swap(&mut self.preview, &mut tab.preview);
        }
    }

    pub(super) fn entry<'a>(&'a self, draft: &'a Draft, id: Id) -> Option<&'a Draft> {
        if !self.empty && id == self.active {
            Some(draft)
        } else {
            self.parked
                .iter()
                .find(|tab| tab.id == id)
                .map(|tab| &tab.draft)
        }
    }

    pub(super) fn editors<'a>(&'a self, draft: &'a Draft) -> impl Iterator<Item = &'a Draft> {
        (!self.empty && !self.preview)
            .then_some(draft)
            .into_iter()
            .chain(
                self.parked
                    .iter()
                    .filter(|tab| !tab.preview)
                    .map(|tab| &tab.draft),
            )
    }

    pub(super) fn editors_mut<'a>(
        &'a mut self,
        draft: &'a mut Draft,
    ) -> impl Iterator<Item = &'a mut Draft> {
        (!self.empty && !self.preview)
            .then_some(draft)
            .into_iter()
            .chain(
                self.parked
                    .iter_mut()
                    .filter(|tab| !tab.preview)
                    .map(|tab| &mut tab.draft),
            )
    }

    pub(super) fn select_saved(&mut self, draft: &mut Draft, id: SnippetId) {
        if let Some(tab) = self.parked.iter().find(|tab| tab.draft.id == Some(id)) {
            let id = tab.id;
            self.select(draft, id);
        }
    }

    pub(super) fn session(&self, draft: &Draft) -> super::session::State {
        super::session::State {
            ids: self
                .labels(draft)
                .into_iter()
                .filter(|tab| !tab.preview)
                .filter_map(|tab| tab.saved)
                .collect(),
            active: (!self.preview && !self.empty).then_some(draft.id).flatten(),
        }
    }

    pub(super) fn browse(&mut self, draft: &mut Draft, entry: &Snippet) {
        self.empty = false;
        if draft.id == Some(entry.id()) {
            return;
        }
        if let Some(tab) = self
            .parked
            .iter()
            .find(|tab| tab.draft.id == Some(entry.id()))
        {
            let id = tab.id;
            self.select(draft, id);
            return;
        }
        if let Some(tab) = self.parked.iter().find(|tab| tab.preview) {
            let id = tab.id;
            self.select(draft, id);
        }
        if self.preview {
            *draft = Draft::new(Some(entry));
        } else {
            self.push(draft, Draft::new(Some(entry)), true);
        }
    }

    pub(super) fn new_draft(&mut self, draft: &mut Draft) {
        if self.empty {
            *draft = Draft::new(None);
            self.empty = false;
            self.preview = false;
        } else {
            self.push(draft, Draft::new(None), false);
        }
    }

    fn push(&mut self, draft: &mut Draft, replacement: Draft, preview: bool) {
        let old = std::mem::replace(draft, replacement);
        self.parked.push(Parked {
            id: self.active,
            preview: self.preview,
            draft: old,
        });
        self.active = Id(self.next);
        self.next += 1;
        self.preview = preview;
    }

    pub(super) fn pinned(&self, draft: &Draft, id: SnippetId) -> bool {
        (!self.preview && draft.id == Some(id))
            || self
                .parked
                .iter()
                .any(|tab| !tab.preview && tab.draft.id == Some(id))
    }

    pub(super) fn close(&mut self, draft: &mut Draft, id: Id) {
        if id != self.active {
            self.parked.retain(|tab| tab.id != id);
            return;
        }
        if let Some(tab) = self.parked.pop() {
            self.active = tab.id;
            self.preview = tab.preview;
            *draft = tab.draft;
        } else {
            self.clear(draft);
        }
    }

    pub(super) fn clear(&mut self, draft: &mut Draft) {
        self.parked.clear();
        *draft = Draft::new(None);
        self.active = Id(self.next);
        self.next += 1;
        self.preview = true;
        self.empty = true;
    }

    pub(super) fn first_dirty(&self, draft: &Draft) -> Option<Id> {
        if draft.dirty {
            Some(self.active)
        } else {
            self.parked
                .iter()
                .find(|tab| tab.draft.dirty)
                .map(|tab| tab.id)
        }
    }

    pub(super) fn refresh(&mut self, draft: &mut Draft, library: &Library) {
        self.parked.retain(|tab| {
            tab.draft.dirty || tab.draft.id.is_none_or(|id| library.get(id).is_some())
        });
        if !draft.dirty && draft.id.is_some_and(|id| library.get(id).is_none()) {
            self.close(draft, self.active);
        }
        refresh(draft, library);
        for tab in &mut self.parked {
            refresh(&mut tab.draft, library);
        }
    }

    pub(super) fn reset(&mut self, draft: &mut Draft, library: &Library) {
        let selected = draft.id.and_then(|id| library.get(id));
        *draft = Draft::new(selected.or_else(|| library.entries().first()));
        self.parked.clear();
        self.preview = true;
        self.empty = false;
    }

    pub(super) fn remove(&mut self, draft: &mut Draft, id: SnippetId) {
        self.parked.retain(|tab| tab.draft.id != Some(id));
        if draft.id == Some(id) {
            self.close(draft, self.active);
        }
    }
}

fn refresh(draft: &mut Draft, library: &Library) {
    if !draft.dirty
        && let Some(id) = draft.id
        && let Some(entry) = library.get(id)
        && Some(entry) != draft.base.as_ref()
    {
        draft.replace_saved(entry);
    }
}
