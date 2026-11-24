use iced::keyboard::{Key, key::Named};
use iced::widget::{Id, button, checkbox, column, container, row, rule, scrollable, text};
use iced::{Alignment, Element, Font, Length, Task};

use crate::connection_store::{MAX_TUNNELS, TunnelConfig, TunnelTarget};
use crate::workspace::dialog_design::{self, keyboard_table, scroll};

use super::{Edit, Message};

mod editor;
pub(crate) use editor::{Editor, Event};

#[derive(Clone, Copy, Debug)]
pub(crate) enum Request {
    Add,
    Edit,
}

pub(super) const HELP: &str = "Local (-L) forwards a local port to a service through the SSH server.\nRemote (-R) forwards a port on the SSH server to a service from this computer.\nDynamic (-D) creates a local SOCKS proxy.";

pub(super) struct List {
    id: Id,
    rows: Vec<TunnelConfig>,
    selected: Option<usize>,
}

impl List {
    pub(super) fn new(rows: Vec<TunnelConfig>) -> Self {
        Self {
            id: Id::unique(),
            rows,
            selected: None,
        }
    }

    pub(super) fn configs(&self) -> &[TunnelConfig] {
        &self.rows
    }

    pub(super) fn enable_all(&mut self, enabled: bool) -> bool {
        let changed = self.rows.iter().any(|tunnel| tunnel.enabled != enabled);
        for tunnel in &mut self.rows {
            tunnel.enabled = enabled;
        }
        changed
    }

    pub(super) fn remove_selected(&mut self) -> bool {
        let Some(index) = self.selected else {
            return false;
        };
        self.rows.remove(index);
        self.selected = self.rows.len().checked_sub(1).map(|last| index.min(last));
        true
    }

    pub(super) fn begin(&self, request: Request, source: Id) -> Option<Editor> {
        let original = match request {
            Request::Add if self.rows.len() < MAX_TUNNELS => None,
            Request::Edit => {
                let index = self.selected?;
                Some((index, self.rows[index].clone()))
            }
            Request::Add => return None,
        };
        Some(Editor::new(source, original))
    }

    pub(super) fn commit(&mut self, editor: &Editor, source: &Id) -> bool {
        if &editor.source != source {
            return false;
        }
        let Ok(config) = editor.config() else {
            return false;
        };
        match &editor.original {
            Some((index, original))
                if self.rows.get(*index) == Some(original) && *original != config =>
            {
                self.rows[*index] = config;
                self.selected = Some(*index);
            }
            None if self.rows.len() < MAX_TUNNELS => {
                self.selected = Some(self.rows.len());
                self.rows.push(config);
            }
            _ => return false,
        }
        true
    }

    pub(super) fn select(&mut self, index: usize) -> Task<Message> {
        if index >= self.rows.len() {
            return Task::none();
        }
        self.selected = Some(index);
        scroll::reveal_row(self.id.clone(), self.row_id(index))
    }

    fn row_id(&self, index: usize) -> Id {
        Id::from(format!("connection-tunnel/{:?}/{index}", self.id))
    }

    pub(super) fn view(&self) -> Element<'_, Message> {
        use keyboard_table::TableKey;

        let enabled = self.rows.iter().filter(|tunnel| tunnel.enabled).count();
        let all = enabled != 0 && enabled == self.rows.len();
        let mixed = enabled != 0 && !all;
        let toggle = (!self.rows.is_empty()).then_some(Message::Edit(Edit::EnableTunnels(!all)));
        let on_toggle = toggle.clone().map(|message| move |_| message.clone());
        let mut aggregate = checkbox(enabled != 0)
            .label("Enable SSH tunnels")
            .text_size(13)
            .on_toggle_maybe(on_toggle);
        if mixed {
            aggregate = aggregate.icon(checkbox::Icon {
                font: Font::DEFAULT,
                code_point: '−',
                size: Some(16.into()),
                line_height: text::LineHeight::default(),
                shaping: text::Shaping::Basic,
            });
        }

        let rows = column(self.rows.iter().enumerate().map(|(index, tunnel)| {
            let selected = self.selected == Some(index);
            let entry = button(text(label(tunnel)).size(13))
                .width(Length::Fill)
                .style(super::view::selectable(selected))
                .on_press(Message::SelectTunnel(index));
            container(entry).id(self.row_id(index)).into()
        }));
        let list = container(scrollable(rows).id(self.id.clone()))
            .width(Length::Fill)
            .height(330)
            .style(dialog_design::sidebar);
        let list = if self.rows.is_empty() {
            list.into()
        } else {
            keyboard_table::view(list.into(), move |key, modifiers, repeat| {
                if !modifiers.is_empty() {
                    return TableKey::Ignored;
                }
                let last = self.rows.len() - 1;
                let index = match key {
                    Key::Named(Named::ArrowDown) => self.selected.map_or(0, |i| (i + 1).min(last)),
                    Key::Named(Named::ArrowUp) => {
                        self.selected.map_or(last, |i| i.saturating_sub(1))
                    }
                    Key::Named(Named::Home) => 0,
                    Key::Named(Named::End) => last,
                    Key::Named(Named::Enter) if self.selected.is_some() => {
                        return TableKey::Handled(
                            (!repeat).then_some(Message::OpenTunnelEditor(Request::Edit)),
                        );
                    }
                    _ => return TableKey::Ignored,
                };
                TableKey::Handled(Some(Message::SelectTunnel(index)))
            })
        };
        let selected = self.selected.is_some();
        column![
            keyboard_table::action(aggregate.into(), toggle, false),
            rule::horizontal(1),
            text("Configured tunnels").size(14),
            list,
            row![
                action(
                    "Add",
                    (self.rows.len() < MAX_TUNNELS)
                        .then_some(Message::OpenTunnelEditor(Request::Add))
                ),
                action(
                    "Edit",
                    selected.then_some(Message::OpenTunnelEditor(Request::Edit))
                ),
                action(
                    "Remove",
                    selected.then_some(Message::Edit(Edit::RemoveSelectedTunnel))
                ),
            ]
            .spacing(10)
            .align_y(Alignment::Center),
            text(HELP).size(13).style(text::secondary),
        ]
        .spacing(10)
        .into()
    }
}

fn action(label: &'static str, action: Option<Message>) -> Element<'static, Message> {
    let control = button(label)
        .style(dialog_design::secondary_button)
        .on_press_maybe(action.clone());
    keyboard_table::action(control.into(), action, true)
}

fn label(tunnel: &TunnelConfig) -> String {
    let marker = if tunnel.enabled { "✓" } else { "○" };
    let kind = match tunnel.rule.target {
        TunnelTarget::Local { .. } => "L",
        TunnelTarget::Remote { .. } => "R",
        TunnelTarget::Dynamic => "D",
    };
    let mut label = format!("{marker} {kind} {}", tunnel.rule.label());
    if !tunnel.description.is_empty() {
        label.push_str(" · ");
        label.push_str(&tunnel.description);
    }
    label
}

#[cfg(test)]
mod tests {
    use super::editor::Edit as TunnelEdit;
    use super::*;

    #[test]
    fn list_edits_are_transactional_and_aggregate_toggle_keeps_rules() {
        let source = Id::unique();
        let mut list = List::new(Vec::new());
        let mut add = list.begin(Request::Add, source.clone()).unwrap();
        add.update(TunnelEdit::Description("First".into()));
        assert!(!list.commit(&add, &Id::unique()));
        assert!(list.configs().is_empty());
        assert!(list.commit(&add, &source));
        let mut add_disabled = list.begin(Request::Add, source.clone()).unwrap();
        add_disabled.update(TunnelEdit::Enabled(false));
        add_disabled.update(TunnelEdit::LocalPort("8081".into()));
        assert!(list.commit(&add_disabled, &source));
        assert_eq!(list.configs().iter().filter(|row| row.enabled).count(), 1);
        assert!(list.enable_all(true));
        assert_eq!(list.configs()[1].rule.bind.port(), 8081);
        let mut cancelled = list.begin(Request::Edit, source.clone()).unwrap();
        cancelled.update(TunnelEdit::Description("Cancelled".into()));
        drop(cancelled);
        assert!(list.configs()[1].description.is_empty());
        assert!(list.remove_selected());
        assert_eq!(list.configs()[0].description, "First");
        assert!(!list.enable_all(true));
    }
}
