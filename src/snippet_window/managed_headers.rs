//! Read-only view of installation-managed script headers.

use iced::widget::{button, column, row, scrollable, text};
use iced::{Element, Font, Length};

use crate::clipboard_policy::ManagedScriptHeader;

use super::Message;

pub(super) fn view<'a>(
    headers: &'a [ManagedScriptHeader],
    selected: &'a ManagedScriptHeader,
) -> Element<'a, Message> {
    let choices = headers
        .iter()
        .fold(column![].spacing(4), |choices, header| {
            choices.push(
                button(text(&header.name).wrapping(text::Wrapping::WordOrGlyph))
                    .width(Length::Fill)
                    .style(if header.id == selected.id {
                        button::primary
                    } else {
                        button::secondary
                    })
                    .on_press(Message::SelectManagedHeader(header.id)),
            )
        });
    column![
        text("Managed script headers").size(20),
        text("Read-only templates provided by your organization."),
        row![
            scrollable(choices).width(Length::FillPortion(1)),
            scrollable(
                text(&selected.content)
                    .font(Font::MONOSPACE)
                    .wrapping(text::Wrapping::WordOrGlyph),
            )
            .width(Length::FillPortion(2)),
        ]
        .spacing(10)
        .height(Length::Fill),
        button("Back").on_press(Message::CloseManagedHeaders),
    ]
    .spacing(8)
    .padding(16)
    .height(Length::Fill)
    .into()
}

#[cfg(test)]
mod tests {
    use crate::clipboard_policy::{ManagedHeaderId, parse};
    use crate::snippets::{Input, Library, tests::Profile};

    use super::super::{Message, SnippetWindow};

    const POLICY: &str = r##"
[meta]
schema-version = 1
[[rule]]
[rule.snippets]
allow-custom-script-headers = false
[[script-header]]
name = "Managed A"
content = "# First"
[[script-header]]
name = "Managed B"
content = "# Second"
"##;

    #[test]
    fn managed_script_header_preview_is_read_only_and_separate_from_the_library() {
        let profile = Profile::new();
        let mut window = SnippetWindow::open(profile.path()).unwrap();
        window.snippet_policy = parse(POLICY).unwrap().snippets;
        let first = window.managed_script_headers()[0].id;
        let second = window.managed_script_headers()[1].id;
        window.managed_header = Some(first);
        let bogus = ManagedHeaderId::try_from("0".repeat(64)).unwrap();
        let _ = window.update(Message::SelectManagedHeader(bogus));
        assert_eq!(window.managed_header, Some(first));
        let _ = window.update(Message::SelectManagedHeader(second));
        assert_eq!(window.managed_header, Some(second));
        let _ = window.update(Message::Name("Ignored while previewing".into()));
        assert!(!window.dirty());
        assert!(matches!(window.escape(), Message::CloseManagedHeaders));
        let _ = window.update(Message::CloseManagedHeaders);
        assert!(window.managed_header.is_none());
        assert!(!window.dirty());
        assert!(window.latest_library().entries().is_empty());
        assert!(!profile.path().exists());
    }

    #[test]
    fn managed_script_header_creation_controls_allow_existing_header_edits() {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let save = |library: &mut Library, name: &str, category: &str| {
            library
                .save(
                    None,
                    Input {
                        name: name.into(),
                        language: "bash".into(),
                        category: category.into(),
                        content: "# Test".into(),
                    },
                )
                .unwrap()
        };
        let header = save(&mut library, "Existing header", "Script-Header");
        let ordinary = save(&mut library, "Ordinary", "Other");
        let mut window = SnippetWindow::open(profile.path()).unwrap();
        window.snippet_policy = parse(POLICY).unwrap().snippets;
        let _ = window.update(Message::Select(header));
        assert!(!window.custom_header_save_blocked(Some(header)));
        assert!(window.custom_header_save_blocked(None));
        let _ = window.update(Message::Select(ordinary));
        assert!(!window.custom_header_save_blocked(Some(ordinary)));
        let _ = window.update(Message::Category(" SCRIPT-header ".into()));
        assert!(window.custom_header_save_blocked(Some(ordinary)));
    }
}
