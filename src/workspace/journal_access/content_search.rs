//! One cancellable content search per journal manager.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::{Task, window};

use crate::session_journal::content_search::{SearchResult, scan};
use crate::session_journal::{Catalog, tree};

use super::{Desktop, Manager, Message as JournalMessage};

#[derive(Clone, Debug)]
pub enum Message {
    Finished(u64, Result<Arc<SearchResult>, String>),
}

#[derive(Default)]
pub(super) struct Search {
    pub(super) enabled: bool,
    generation: u64,
    active: Option<Request>,
    result: Option<Result<Arc<SearchResult>, String>>,
}

#[derive(Clone)]
struct Request {
    ticket: u64,
    root: PathBuf,
    catalog: Arc<Catalog>,
    filter: tree::Filter,
    query: String,
    cancelled: Arc<AtomicBool>,
}

impl Request {
    fn scan(&self) -> io::Result<SearchResult> {
        scan(&self.root, &self.catalog, self.filter, &self.query, &|| {
            self.cancelled.load(Ordering::Relaxed)
        })
    }
}

impl Search {
    pub(super) fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.result = None;
        self.cancel();
    }

    fn cancel(&self) {
        if let Some(request) = &self.active {
            request.cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn has_match(&self, index: usize) -> bool {
        self.result.as_ref().is_some_and(|result| {
            result
                .as_ref()
                .is_ok_and(|result| result.matching_indices.contains(&index))
        })
    }

    pub(super) fn pending(&self, query: &str) -> bool {
        self.enabled && !query.is_empty() && self.result.is_none()
    }

    pub(super) fn status(&self, query: &str) -> Option<String> {
        if self.pending(query) {
            return Some("Searching journal contents…".into());
        }
        match self.result.as_ref()? {
            Ok(result) if result.skipped > 0 => Some(format!(
                "Content search incomplete · {} journal(s) skipped",
                result.skipped
            )),
            Ok(result) if result.partial => {
                Some("Content search incomplete · unreadable files or search limits".into())
            }
            Ok(_) => None,
            Err(problem) => Some(format!(
                "Could not search journal contents ({})",
                super::label(problem, 220)
            )),
        }
    }

    fn start(
        &mut self,
        id: window::Id,
        root: &Path,
        catalog: &Arc<Catalog>,
        filter: tree::Filter,
        raw_query: &str,
    ) -> Task<Desktop> {
        if self.active.is_some() || !self.enabled || self.result.is_some() {
            return Task::none();
        }
        let query = tree::prepare_query(raw_query);
        if query.is_empty() {
            return Task::none();
        }
        self.generation = self.generation.wrapping_add(1);
        let request = Request {
            ticket: self.generation,
            root: root.to_owned(),
            catalog: Arc::clone(catalog),
            filter,
            query,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let ticket = request.ticket;
        self.active = Some(request.clone());
        super::read_task(
            id,
            move || request.scan(),
            move |result| JournalMessage::ContentSearch(Message::Finished(ticket, result)),
        )
    }

    fn finish(
        &mut self,
        ticket: u64,
        result: Result<Arc<SearchResult>, String>,
        catalog: Option<&Arc<Catalog>>,
        root: Option<&Path>,
        allowed: bool,
    ) -> bool {
        let Some(request) = self.active.take_if(|request| request.ticket == ticket) else {
            return false;
        };
        if !allowed {
            self.invalidate();
            return false;
        }
        if request.ticket != self.generation
            || request.cancelled.load(Ordering::Relaxed)
            || root != Some(request.root.as_path())
            || !catalog.is_some_and(|current| Arc::ptr_eq(&request.catalog, current))
        {
            return false;
        }
        self.result = Some(result);
        true
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl Manager {
    pub(super) fn start_content_search(&mut self, id: window::Id) -> Task<Desktop> {
        if self.loading {
            return Task::none();
        }
        let Some((root, catalog)) = self.root.as_deref().zip(self.catalog.as_ref()) else {
            return Task::none();
        };
        self.content_search
            .start(id, root, catalog, self.filter, &self.query)
    }

    pub(super) fn finish_content_search(
        &mut self,
        id: window::Id,
        ticket: u64,
        result: Result<Arc<SearchResult>, String>,
        allowed: bool,
    ) -> Task<Desktop> {
        let applied = self.content_search.finish(
            ticket,
            result,
            self.catalog.as_ref(),
            self.root.as_deref(),
            allowed,
        );
        if !allowed {
            return Task::none();
        }
        let scroll = if applied {
            self.rebuild_tree();
            self.sync_description();
            self.tree
                .take_scroll_task()
                .map(move |event| Desktop::SessionJournal(id, JournalMessage::Tree(event)))
        } else {
            Task::none()
        };
        // A superseding request uses the latest UI state. No queue of old scans is kept.
        Task::batch([scroll, self.start_content_search(id)])
    }
}

#[cfg(test)]
mod tests;
