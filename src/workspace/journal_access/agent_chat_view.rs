//! Internal chat actions are independent of external web-link preferences.

use iced::Element;
use iced::widget::button;

use super::super::journal_appearance::Content;
use super::super::journal_appearance_access::page_text;
use super::link_text::journal_keyed_action;

pub(in crate::workspace) fn action<'a, Key, Message>(
    key: Key,
    message: Option<Message>,
    appearance: Content,
) -> Element<'a, Message>
where
    Key: Clone + Eq + 'static,
    Message: Clone + 'a,
{
    let enabled = message.is_some();
    journal_keyed_action(
        (key, enabled),
        button(page_text(appearance, "Open in AI Chat", 13.0))
            .on_press_maybe(message)
            .padding([4, 8])
            .into(),
    )
}

#[cfg(test)]
mod tests;
