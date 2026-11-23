//! Routes keyboard actions from the focused module inventory.

use iced::Element;
use iced::keyboard::{Key, key::Named};

use crate::workspace::dialog_design::keyboard_table::{self, TableKey};

use super::{Desktop, Edit, Module};

pub(super) fn view<'a>(
    content: Element<'a, Desktop>,
    selected: Option<Module>,
    activate: bool,
    combination: &'a str,
    message: impl Fn(Edit) -> Desktop + 'a,
) -> Element<'a, Desktop> {
    keyboard_table::view(content, move |key, modifiers, repeat| {
        if !modifiers.is_empty() {
            return TableKey::Ignored;
        }
        let edit = match key {
            Key::Named(key @ (Named::ArrowDown | Named::ArrowUp | Named::Home | Named::End)) => {
                navigate(*key, selected, &Module::choices()).map(Edit::NavigateModule)
            }
            Key::Named(Named::Space) if selected.is_some() => selected
                .filter(|_| activate && !repeat)
                .map(|module| Edit::Module(combination.into(), module)),
            _ => return TableKey::Ignored,
        };
        TableKey::Handled(edit.map(&message))
    })
}

fn navigate(key: Named, selected: Option<Module>, modules: &[Module]) -> Option<Module> {
    let index = selected.and_then(|module| modules.iter().position(|entry| *entry == module));
    match key {
        Named::ArrowDown => modules.get(index.map_or(0, |index| index + 1)).copied(),
        Named::ArrowUp => index.map_or_else(
            || modules.first().copied(),
            |index| {
                index
                    .checked_sub(1)
                    .and_then(|index| modules.get(index).copied())
            },
        ),
        Named::Home => modules.first().copied(),
        Named::End => modules.last().copied(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use iced::advanced::widget::operation::focusable;
    use iced::widget::text;
    use iced::{Font, Renderer, Size, Theme, window};
    use iced_runtime::{Action, UserInterface, task, user_interface::Cache};

    use crate::settings_search::test_input;
    use crate::terminal_agent::design::History;

    use super::super::Message;
    use super::*;

    #[test]
    fn focused_inventory_previews_with_arrows_and_only_activates_with_space() {
        let id = window::Id::unique();
        let current = Module::History(History::InlineBlocks);
        let table = view(
            text("Modules").into(),
            Some(current),
            true,
            "standard",
            move |edit| Desktop::Settings(Message::AgentDesignCatalog(id, edit)),
        );
        let mut renderer = Renderer::new(Font::DEFAULT, 16.0.into());
        let mut interface = UserInterface::build(
            table,
            Size::new(600.0, 400.0),
            Cache::default(),
            &mut renderer,
        );
        test_input::apply(
            &mut interface,
            &renderer,
            task::effect(Action::widget(focusable::focus_next())),
        );
        let press =
            |interface: &mut UserInterface<'_, Desktop, Theme, Renderer>,
             renderer: &mut Renderer,
             key| { test_input::press(interface, renderer, Key::Named(key), None) };
        let preview = press(&mut interface, &mut renderer, Named::ArrowDown);
        assert!(matches!(
            preview.as_slice(),
            [Desktop::Settings(Message::AgentDesignCatalog(
                _,
                Edit::NavigateModule(Module::History(History::Timeline))
            ))]
        ));
        let activation = press(&mut interface, &mut renderer, Named::Space);
        assert!(
            matches!(activation.as_slice(), [Desktop::Settings(Message::AgentDesignCatalog(_, Edit::Module(combination, module)))] if combination == "standard" && *module == current)
        );
    }
}
