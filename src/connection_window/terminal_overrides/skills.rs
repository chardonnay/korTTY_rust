use iced::keyboard::{Key, key::Named};
use iced::widget::{Id, checkbox, column, container, scrollable, text};
use iced::{Element, Length, Task};

use crate::workspace::dialog_design::{
    self,
    keyboard_table::{self, TableKey},
};

use super::Edit;

pub(super) struct Row {
    pub id: u64,
    pub name: String,
    pub checked: bool,
}

impl Row {
    fn checkbox(self) -> checkbox::Checkbox<'static, Edit> {
        let id = self.id;
        checkbox(self.checked)
            .label(self.name)
            .on_toggle(move |checked| Edit::AiSkill(id, checked))
    }
}

pub(super) fn local(rows: Vec<Row>) -> Element<'static, Edit> {
    if rows.is_empty() {
        return empty();
    }
    column(rows.into_iter().map(|row| row.checkbox().into()))
        .spacing(5)
        .into()
}

pub(super) fn ssh(list: Id, rows: Vec<Row>, cursor: Option<u64>) -> Element<'static, Edit> {
    if rows.is_empty() {
        return panel(list, empty());
    }
    let entries: Vec<_> = rows.iter().map(|row| (row.id, row.checked)).collect();
    let selected = cursor
        .and_then(|id| entries.iter().position(|entry| entry.0 == id))
        .unwrap_or(0);
    let items = rows.into_iter().enumerate().map(|(index, row)| {
        let id = row_id(&list, row.id);
        let active = index == selected;
        container(row.checkbox().width(Length::Fill))
            .id(id)
            .padding([3, 4])
            .width(Length::Fill)
            .style(move |theme| dialog_design::table_row(theme, active, index % 2 != 0))
            .into()
    });
    let content = column(items).into();
    let panel = panel(list, content);
    keyboard_table::view(panel, move |key, modifiers, repeat| {
        if !modifiers.is_empty() {
            return TableKey::Ignored;
        }
        let index = match key {
            Key::Named(Named::ArrowDown) => (selected + 1).min(entries.len() - 1),
            Key::Named(Named::ArrowUp) => selected.saturating_sub(1),
            Key::Named(Named::Home) => 0,
            Key::Named(Named::End) => entries.len() - 1,
            Key::Named(Named::Space) => {
                let (id, checked) = entries[selected];
                return TableKey::Handled((!repeat).then_some(Edit::AiSkill(id, !checked)));
            }
            _ => return TableKey::Ignored,
        };
        TableKey::Handled((index != selected).then_some(Edit::AiSkillNavigate(entries[index].0)))
    })
}

fn empty() -> Element<'static, Edit> {
    text("No matching skills").style(text::secondary).into()
}

fn panel(list: Id, content: Element<'static, Edit>) -> Element<'static, Edit> {
    container(
        scrollable(container(content).width(Length::Fill).padding(8))
            .id(list)
            .height(200),
    )
    .width(Length::Fill)
    .style(container::bordered_box)
    .into()
}

fn row_id(list: &Id, id: u64) -> Id {
    Id::from(format!("connection-ai-skill/{list:?}/{id}"))
}

pub(super) fn reveal<Message>(list: Id, id: u64) -> Task<Message> {
    let row = row_id(&list, id);
    dialog_design::scroll::reveal_row(list, row)
}
