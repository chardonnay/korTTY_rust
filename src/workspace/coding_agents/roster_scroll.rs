use iced::advanced::widget::Id;
use iced::{Task, window};

use crate::session::SessionId;
pub(in crate::workspace) use crate::workspace::dialog_design::scroll::{
    reveal_focused, reveal_row,
};

use super::Desktop;

pub(super) fn list_id(owner: window::Id) -> Id {
    Id::from(format!("coding-agents/{owner:?}"))
}

pub(super) fn row_id(owner: window::Id, session: SessionId) -> Id {
    Id::from(format!("coding-agents/{owner:?}/{}", session.0))
}

pub(super) fn reveal(owner: window::Id, session: SessionId) -> Task<Desktop> {
    reveal_row(list_id(owner), row_id(owner, session))
}
