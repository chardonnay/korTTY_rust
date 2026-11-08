use iced::widget::{button, row, text};
use iced::{Element, Padding};

use crate::session_journal::keywords::usable;

use super::{Manager, Message};

impl Manager {
    pub(super) fn keyword_labels(&self) -> impl Iterator<Item = &str> {
        let keywords = self
            .selected()
            .map(|journal| journal.keywords.as_slice())
            .filter(|keywords| !keywords.is_empty())
            .unwrap_or_else(|| {
                self.catalog
                    .as_deref()
                    .map(|catalog| catalog.top_keywords.as_slice())
                    .unwrap_or_default()
            });
        keywords
            .iter()
            .map(String::as_str)
            .filter(|keyword| !self.loading && usable(keyword))
    }

    pub(super) fn keyword_chips(&self) -> Option<Element<'_, Message>> {
        let mut keywords = self.keyword_labels().peekable();
        keywords.peek()?;
        let mut chips = row![].spacing(6);
        for keyword in keywords {
            chips = chips.push(
                button(text(keyword).size(12))
                    .padding(Padding::from([2, 10]))
                    .style(|theme, status| {
                        let mut style = button::secondary(theme, status);
                        style.border.radius = 999.0.into();
                        style
                    })
                    .on_press(Message::Keyword(self.revision, keyword.to_owned())),
            );
        }
        Some(chips.wrap().into())
    }
}

#[cfg(test)]
mod tests;
