use std::sync::Arc;

use iced::widget::{self, button, checkbox, row, text, text_input};
use iced::{Alignment, Element, Length, Task};
use kortty_terminal::{MAX_SEARCH_QUERY_BYTES, SearchMatch};

use crate::session::{Frame, SearchError, SearchResponse, Session};

#[derive(Debug, Clone)]
pub enum Message {
    Query(String),
    MatchCase(bool),
    Find,
    Previous,
    Next,
    Close,
}

pub struct TerminalSearch {
    input: widget::Id,
    query: String,
    match_case: bool,
    pending: Option<u64>,
    result: Option<Arc<SearchResponse>>,
    selected: usize,
    notice: Option<&'static str>,
}

impl TerminalSearch {
    pub fn new() -> Self {
        Self {
            input: widget::Id::unique(),
            query: String::new(),
            match_case: false,
            pending: None,
            result: None,
            selected: 0,
            notice: None,
        }
    }

    pub fn focus(&self) -> Task<Message> {
        widget::operation::focus(self.input.clone())
    }

    pub fn view(&self) -> Element<'_, Message> {
        let can_find = !self.query.is_empty() && self.pending.is_none();
        let can_move = self
            .result
            .as_ref()
            .is_some_and(|result| result.results.matches.len() > 1);
        row![
            text_input("Find in terminal", &self.query)
                .id(self.input.clone())
                .on_input(Message::Query)
                .on_submit_maybe(can_find.then_some(Message::Find))
                .width(Length::Fixed(220.0)),
            checkbox(self.match_case)
                .label("Match case")
                .on_toggle(Message::MatchCase),
            button("Find").on_press_maybe(can_find.then_some(Message::Find)),
            button("Previous").on_press_maybe(can_move.then_some(Message::Previous)),
            button("Next").on_press_maybe(can_move.then_some(Message::Next)),
            button("Close").on_press(Message::Close),
            text(self.status()),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .into()
    }

    /// Applies a panel action. The caller drops this panel when it returns true.
    pub fn update(&mut self, message: Message, session: &Session) -> bool {
        match message {
            Message::Close => return true,
            Message::Query(query) => {
                self.clear();
                if query.len() <= MAX_SEARCH_QUERY_BYTES {
                    self.query = query;
                } else {
                    self.notice = Some(error_text(SearchError::InvalidQuery));
                }
            }
            Message::MatchCase(enabled) => {
                self.clear();
                self.match_case = enabled;
            }
            Message::Find if self.pending.is_none() && !self.query.is_empty() => {
                self.clear();
                match session.search(&self.query, !self.match_case) {
                    Ok(ticket) => self.pending = Some(ticket),
                    Err(error) => self.notice = Some(error_text(error)),
                }
            }
            Message::Previous => self.step(session, -1),
            Message::Next => self.step(session, 1),
            Message::Find => {}
        }
        false
    }

    pub fn sync(&mut self, session: &Session, frame: &Frame) {
        if self
            .result
            .as_ref()
            .is_some_and(|result| result.revision != frame.search_revision)
        {
            self.invalidate();
        }
        let Some(result) = frame
            .search_result()
            .filter(|result| self.pending == Some(result.request))
        else {
            if self.pending.is_some() && !session.supports_search() {
                self.pending = None;
                self.notice = Some(error_text(SearchError::Closed));
            }
            return;
        };
        self.pending = None;
        if result.revision != frame.search_revision {
            self.invalidate();
            return;
        }
        self.notice = None;
        self.selected = result.results.matches.len().saturating_sub(1);
        self.result = Some(Arc::clone(result));
        self.reveal(session, self.selected);
    }

    pub fn matches(&self, frame: &Frame) -> &[SearchMatch] {
        self.result
            .as_ref()
            .filter(|result| result.revision == frame.search_revision)
            .map_or(&[], |result| &result.results.matches)
    }

    pub fn selected_match(&self, frame: &Frame) -> Option<&SearchMatch> {
        self.matches(frame).get(self.selected)
    }

    fn step(&mut self, session: &Session, direction: isize) {
        let frame = session.frame();
        let count = self.matches(&frame).len();
        if count == 0 {
            return;
        }
        let next = (self.selected as isize + direction).rem_euclid(count as isize) as usize;
        self.reveal(session, next);
    }

    fn reveal(&mut self, session: &Session, index: usize) {
        let Some(result) = &self.result else { return };
        let Some(hit) = result.results.matches.get(index) else {
            return;
        };
        match session.reveal_search_match(result.revision, hit.start.row) {
            Ok(()) => {
                self.selected = index;
                self.notice = None;
            }
            Err(error) => self.notice = Some(error_text(error)),
        }
    }

    fn invalidate(&mut self) {
        self.result = None;
        self.notice = Some("Terminal content changed. Search again.");
    }

    fn clear(&mut self) {
        self.pending = None;
        self.result = None;
        self.notice = None;
    }

    fn status(&self) -> String {
        if let Some(notice) = self.notice {
            return notice.to_owned();
        }
        if self.pending.is_some() {
            return "Searching…".into();
        }
        let Some(result) = &self.result else {
            return String::new();
        };
        let count = result.results.matches.len();
        if count == 0 {
            return "No matches".into();
        }
        let more = if result.results.truncated { "+" } else { "" };
        format!("{} / {count}{more}", self.selected + 1)
    }
}

fn error_text(error: SearchError) -> &'static str {
    match error {
        SearchError::InvalidQuery => "Enter up to 1024 bytes of search text.",
        SearchError::Unsupported => "Search is available in local terminals.",
        SearchError::Busy => "Terminal is busy. Try again.",
        SearchError::Closed => "The terminal has closed.",
    }
}
