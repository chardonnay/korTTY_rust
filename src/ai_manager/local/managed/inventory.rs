//! Installed model inventory shared by the llama.cpp and Apple MLX registries.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use iced::keyboard::{Key, Modifiers, key::Named};
use iced::widget::{Id, button, column, container, text};
use iced::{Element, Length, Point, Task};

use crate::ai_manager::search::{ManagedSetting, MlxSetting, Reveal, Setting};
use crate::local_ai::managed_models::Purpose;
use crate::workspace::{
    dialog_design::keyboard_table::{self, TableKey},
    native_popup,
};

use super::super::{Dialog, Message, knowledge_inventory};
use super::{
    Choice, Column, Message as Managed, Models, model_file, publication_age, publication_days,
};

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) enum ModelId {
    Gguf(String),
    Mlx(String),
}

impl ModelId {
    pub(super) fn as_str(&self) -> &str {
        match self {
            Self::Gguf(id) | Self::Mlx(id) => id,
        }
    }

    pub(super) fn is_gguf(&self, id: &str) -> bool {
        matches!(self, Self::Gguf(gguf) if gguf == id)
    }
}

#[derive(Default)]
pub(in crate::ai_manager::local) struct Selection {
    anchor: Option<ModelId>,
    lead: Option<ModelId>,
    default_menu: Option<DefaultMenu>,
}

struct DefaultMenu {
    row: ModelId,
    point: Point,
    mark: bool,
}

pub(in crate::ai_manager::local) struct Row {
    pub(in crate::ai_manager::local) id: ModelId,
    pub(in crate::ai_manager::local) name: String,
    pub(in crate::ai_manager::local) file: String,
    pub(in crate::ai_manager::local) purpose: String,
    pub(in crate::ai_manager::local) backend: String,
    pub(in crate::ai_manager::local) idle_minutes: u32,
    pub(in crate::ai_manager::local) published_at: Option<String>,
    pub(in crate::ai_manager::local) state: String,
    default: bool,
}

impl Row {
    fn gguf(choice: &Choice, models: &Models) -> Self {
        let model = &choice.model;
        Self {
            id: ModelId::Gguf(model.id.clone()),
            name: model.name.clone(),
            file: model_file(model),
            purpose: purpose_label(model.purpose).into(),
            backend: model.backend.to_string(),
            idle_minutes: model.idle_timeout_minutes,
            published_at: model.published_at.clone(),
            state: models.model_status(model).to_string().to_uppercase(),
            default: choice.is_default,
        }
    }

    pub(in crate::ai_manager::local) fn mlx(
        model: &crate::local_ai::mlx_models::Model,
        state: String,
    ) -> Self {
        Self {
            id: ModelId::Mlx(model.id.clone()),
            name: model.name.clone(),
            file: model.id.clone(),
            purpose: purpose_label(Purpose::Chat).into(),
            backend: "Metal".into(),
            idle_minutes: u32::from(model.idle_minutes),
            published_at: model.published_at.clone(),
            state: state.to_uppercase(),
            default: false,
        }
    }

    fn cmp(&self, other: &Self, models: &Models, now: jiff::Timestamp) -> Ordering {
        let minutes = |value| if value == 0 { u32::MAX } else { value };
        let age =
            |row: &Self| publication_days(row.published_at.as_deref(), now).unwrap_or(i64::MAX);
        let order = match models.sort_column {
            Column::Name => self.name.cmp(&other.name),
            Column::File => self.file.cmp(&other.file),
            Column::Purpose => self.purpose.cmp(&other.purpose),
            Column::Backend => self.backend.cmp(&other.backend),
            Column::Unload => minutes(self.idle_minutes).cmp(&minutes(other.idle_minutes)),
            Column::Age => age(self).cmp(&age(other)),
            Column::State => self.state.cmp(&other.state),
        };
        (if models.sort_descending {
            order.reverse()
        } else {
            order
        })
        .then_with(|| self.name.cmp(&other.name))
        .then_with(|| self.id.cmp(&other.id))
    }

    fn cells(self, now: jiff::Timestamp) -> [String; 7] {
        [
            if self.default {
                format!("★ {}", self.name)
            } else {
                self.name
            },
            self.file,
            self.purpose,
            self.backend,
            if self.idle_minutes == 0 {
                "Never".into()
            } else {
                format!("{} min", self.idle_minutes)
            },
            publication_age(self.published_at.as_deref(), now),
            self.state,
        ]
    }
}

impl Dialog {
    fn installed_rows(&self, now: jiff::Timestamp) -> Vec<Row> {
        let default = self.installed_default(None);
        let mut rows = self
            .managed
            .choices
            .iter()
            .map(|choice| Row::gguf(choice, &self.managed))
            .chain(self.mlx.inventory().map(|mut row| {
                row.default = default.as_ref() == Some(&row.id);
                row
            }))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| left.cmp(right, &self.managed, now));
        rows
    }

    fn installed_selected(&self, id: &ModelId) -> bool {
        match id {
            ModelId::Gguf(id) => self.managed.selected_ids.contains(id),
            ModelId::Mlx(id) => self.mlx.inventory_selected(id),
        }
    }

    fn installed_lead(&self, visible: &[ModelId]) -> Option<usize> {
        self.installed_selection
            .lead
            .as_ref()
            .filter(|id| self.installed_selected(id))
            .and_then(|lead| visible.iter().position(|id| id == lead))
            .or_else(|| visible.iter().position(|id| self.installed_selected(id)))
    }

    pub(in crate::ai_manager::local) fn installed_default(
        &self,
        legacy: Option<&str>,
    ) -> Option<ModelId> {
        self.managed.default.as_ref()?.resolve(legacy)
    }

    pub(in crate::ai_manager::local) fn installed_contains(&self, id: &ModelId) -> bool {
        match id {
            ModelId::Gguf(id) => self
                .managed
                .choices
                .iter()
                .any(|choice| &choice.model.id == id),
            ModelId::Mlx(id) => self.mlx.contains_model(id),
        }
    }

    pub(in crate::ai_manager::local) fn installed_current(&self) -> Option<ModelId> {
        if let Some(lead) = self.installed_selection.lead.as_ref()
            && self.installed_selected(lead)
            && self.installed_contains(lead)
        {
            return Some(lead.clone());
        }
        if self.managed.selected_ids.is_empty() && !self.mlx.has_inventory_selection() {
            return None;
        }
        self.installed_rows(jiff::Timestamp::now())
            .into_iter()
            .map(|row| row.id)
            .find(|id| self.installed_selected(id))
    }

    pub(in crate::ai_manager::local) fn preselect_installed_default(
        &mut self,
        legacy: Option<&str>,
    ) {
        if !self.managed.selected_ids.is_empty() || self.mlx.has_inventory_selection() {
            return;
        }
        if let Some(id) = self
            .installed_default(legacy)
            .filter(|id| self.installed_contains(id))
        {
            self.installed_selection.anchor = Some(id.clone());
            self.apply_installed_selection(&BTreeSet::from([id.clone()]), Some(&id));
        }
    }

    pub(in crate::ai_manager::local) fn default_menu_open(&self) -> bool {
        self.installed_selection.default_menu.is_some()
    }

    pub(in crate::ai_manager) fn close_default_menu(&mut self) {
        self.installed_selection.default_menu = None;
    }

    pub(super) fn move_default_menu(&mut self) {
        if let Some(menu) = &mut self.installed_selection.default_menu {
            menu.mark = !menu.mark;
        }
    }

    pub(super) fn open_default_menu(&mut self, row: ModelId, point: Point) {
        if !self.can_select_installed()
            || self.managed.default.is_none()
            || !self.installed_contains(&row)
        {
            return;
        }
        let selected = if self.installed_selected(&row) {
            self.installed_rows(jiff::Timestamp::now())
                .into_iter()
                .map(|row| row.id)
                .filter(|id| self.installed_selected(id))
                .collect()
        } else {
            self.installed_selection.anchor = Some(row.clone());
            BTreeSet::from([row.clone()])
        };
        self.apply_installed_selection(&selected, Some(&row));
        self.installed_selection.default_menu = Some(DefaultMenu {
            row,
            point,
            mark: true,
        });
    }

    pub(in crate::ai_manager::local) fn can_select_installed(&self) -> bool {
        !self.managed.has_action() && !self.mlx.has_review()
    }

    pub(in crate::ai_manager::local) fn select_installed(
        &mut self,
        id: Option<ModelId>,
        keyboard: Option<Modifiers>,
    ) -> Task<Message> {
        let visible = self
            .installed_rows(jiff::Timestamp::now())
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>();
        let mut selected: BTreeSet<_> = visible
            .iter()
            .filter(|id| self.installed_selected(id))
            .cloned()
            .collect();
        let previous = self.installed_lead(&visible).map(|index| &visible[index]);
        let next = if let Some(id) = id.as_ref() {
            if !visible.contains(id) {
                return Task::none();
            }
            if self
                .installed_selection
                .anchor
                .as_ref()
                .is_none_or(|anchor| !visible.contains(anchor))
            {
                self.installed_selection.anchor = previous.cloned();
            }
            crate::input::select_row(
                &mut selected,
                &mut self.installed_selection.anchor,
                id.clone(),
                &visible,
                keyboard.unwrap_or(self.selection_modifiers),
            );
            Some(id)
        } else {
            selected.extend(visible.iter().cloned());
            previous
        };
        let lead = next
            .filter(|id| selected.contains(*id))
            .or_else(|| previous.filter(|id| selected.contains(*id)))
            .or_else(|| visible.iter().find(|id| selected.contains(*id)));
        if id.is_none() && self.installed_selection.anchor.is_none() {
            self.installed_selection.anchor = lead.cloned();
        }
        self.close_default_menu();
        self.apply_installed_selection(&selected, lead);
        if keyboard.is_some() {
            crate::settings_search::scroll_to_row(list_id(), lead_id())
        } else {
            Task::none()
        }
    }

    fn apply_installed_selection(&mut self, selected: &BTreeSet<ModelId>, lead: Option<&ModelId>) {
        self.installed_selection.lead = lead.cloned();
        self.managed.selected_ids = selected
            .iter()
            .filter_map(|id| match id {
                ModelId::Gguf(id) => Some(id.clone()),
                ModelId::Mlx(_) => None,
            })
            .collect();
        self.managed.selected = lead
            .and_then(|id| match id {
                ModelId::Gguf(id) => self
                    .managed
                    .choices
                    .iter()
                    .find(|choice| &choice.model.id == id),
                ModelId::Mlx(_) => None,
            })
            .cloned();
        self.managed.update_status(&self.profile_dir);
        self.managed.update_reattachment(&self.profile_dir);
        self.mlx.select_inventory(selected, lead, &self.profile_dir);
    }

    pub(in crate::ai_manager::local) fn installed_models_table(
        &self,
        editable: bool,
        default: Option<&ModelId>,
        reveal: &Reveal,
    ) -> Element<'_, Message> {
        const WIDTHS: [u16; 7] = [29, 29, 13, 7, 10, 5, 8];
        let now = jiff::Timestamp::now();
        let enabled = editable && self.can_select_installed();
        let models = self.installed_rows(now);
        let visible = models.iter().map(|row| row.id.clone()).collect::<Vec<_>>();
        let lead = self.installed_lead(&visible);
        let rows = models.into_iter().enumerate().map(|(index, row)| {
            let selected = self.installed_selected(&row.id);
            let id = row.id.clone();
            let action = enabled
                .then(|| Message::Managed(super::Message::SelectInstalled(row.id.clone(), None)));
            let item = knowledge_inventory::inventory_row(
                row.cells(now),
                WIDTHS,
                selected,
                index % 2 != 0,
                action,
            );
            let item = if enabled && self.managed.default.is_some() {
                self.default_menu_row(item, id, default)
            } else {
                item
            };
            if lead == Some(index) {
                container(item).id(lead_id()).into()
            } else {
                item
            }
        });
        let table = knowledge_inventory::sortable_inventory(
            Column::ALL.map(|column| {
                let arrow = if self.managed.sort_column != column {
                    ""
                } else if self.managed.sort_descending {
                    " ↓"
                } else {
                    " ↑"
                };
                format!("{}{arrow}", column.label())
            }),
            WIDTHS,
            Column::ALL
                .map(|column| enabled.then_some(Message::Managed(super::Message::Sort(column)))),
            rows,
            "No installed local models",
            108.0,
            list_id(),
        );
        let table = if enabled {
            keyboard_table::view(table, move |key, modifiers, _| {
                table_key(&visible, lead, key, modifiers)
            })
        } else {
            table
        };
        let table = reveal.wrap(Setting::Managed(ManagedSetting::Select), table);
        reveal.wrap(Setting::Mlx(MlxSetting::Select), table)
    }

    fn default_menu_row<'a>(
        &self,
        base: Element<'a, Message>,
        row: ModelId,
        default: Option<&ModelId>,
    ) -> Element<'a, Message> {
        let can_mark = default != Some(&row);
        let can_clear = default.is_some();
        let mark = can_mark.then_some(Message::Managed(Managed::SetDefault));
        let clear = can_clear.then_some(Message::Managed(Managed::ClearDefault));
        let dismiss = Message::Managed(Managed::CloseDefaultMenu);
        let active = self
            .installed_selection
            .default_menu
            .as_ref()
            .filter(|menu| menu.row == row);
        let mark_first = active.is_some_and(|menu| menu.mark);
        let selected = if mark_first {
            mark.clone().or(clear.clone())
        } else {
            clear.clone().or(mark.clone())
        };
        let popup = active.map(|menu| {
            let item = |label, action, highlighted| {
                button(text(label).size(13))
                    .padding([6, 10])
                    .width(Length::Fill)
                    .style(if highlighted {
                        button::secondary
                    } else {
                        button::text
                    })
                    .on_press_maybe(action)
            };
            let mark_highlighted = (mark_first && can_mark) || !can_clear;
            let items = column![
                item("Set as default", mark, mark_highlighted),
                item("Clear default model", clear, !mark_highlighted),
            ]
            .spacing(2);
            (
                menu.point,
                container(items)
                    .padding(4)
                    .width(182)
                    .style(container::bordered_box)
                    .into(),
            )
        });
        native_popup::context_action_with_keyboard(
            base,
            move |point| Message::Managed(Managed::OpenDefaultMenu(row.clone(), point)),
            popup,
            dismiss.clone(),
            move |key, _| match key {
                Key::Named(Named::Enter) => selected.clone(),
                Key::Named(Named::ArrowDown | Named::ArrowUp) => {
                    Some(Message::Managed(Managed::MoveDefaultMenu))
                }
                Key::Named(Named::Escape) => Some(dismiss.clone()),
                _ => None,
            },
        )
    }
}

fn table_key(
    rows: &[ModelId],
    current: Option<usize>,
    key: &Key,
    modifiers: Modifiers,
) -> TableKey<Message> {
    if modifiers.command()
        && !modifiers.alt()
        && matches!(key, Key::Character(key) if key.eq_ignore_ascii_case("a"))
    {
        return TableKey::Handled(Some(Message::Managed(super::Message::SelectAll)));
    }
    if !(modifiers.is_empty() || modifiers == Modifiers::SHIFT) {
        return TableKey::Ignored;
    }
    let Key::Named(key) = key else {
        return TableKey::Ignored;
    };
    let destination = match key {
        Named::Home => rows.first(),
        Named::End => rows.last(),
        Named::ArrowDown => rows.get(current.map_or(0, |index| index + 1)),
        Named::ArrowUp => current.map_or_else(
            || rows.last(),
            |index| index.checked_sub(1).and_then(|index| rows.get(index)),
        ),
        _ => return TableKey::Ignored,
    };
    TableKey::Handled(
        destination.map(|id| {
            Message::Managed(super::Message::SelectInstalled(id.clone(), Some(modifiers)))
        }),
    )
}

fn list_id() -> Id {
    Id::new("ai-manager-installed-model-list")
}

fn lead_id() -> Id {
    Id::new("ai-manager-installed-model-current")
}

fn purpose_label(purpose: Purpose) -> &'static str {
    match purpose {
        Purpose::Chat => "Chat / generation",
        Purpose::Embedding => "RAG embeddings",
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::model;
    use super::*;

    #[test]
    fn model_ranges_cross_backends_and_keep_distinct_ids() {
        let profile = crate::snippets::tests::Profile::new();
        let root = profile.0.canonicalize().unwrap();
        let mut dialog = Dialog {
            profile_dir: root.clone(),
            ..Dialog::default()
        };
        let common_id = "11111111111111111111111111111111";
        let other_id = "22222222222222222222222222222222";
        let llama = ModelId::Gguf(common_id.into());
        let mlx = ModelId::Mlx(common_id.into());
        let other = ModelId::Gguf(other_id.into());
        let mut older = model(other_id, "Charlie", Purpose::Embedding);
        older.published_at = Some("2024-10-01T12:00:00Z".into());
        dialog.managed.set_models(
            vec![model(common_id, "Alpha", Purpose::Chat), older],
            None,
            None,
            &root,
        );
        let fixture = serde_json::from_value(serde_json::json!({
            "id": common_id, "name": "Bravo", "bytes": 0,
            "idle_minutes": 15, "published_at": "2025-02-01T12:00:00Z", "assets": {},
        }))
        .unwrap();
        dialog.mlx.add_hub_model(fixture, &root);
        let _ = dialog.select_installed(Some(llama.clone()), Some(Modifiers::empty()));
        assert!(dialog.installed_selected(&llama));
        assert!(!dialog.installed_selected(&mlx));
        let _ = dialog.select_installed(Some(other.clone()), Some(Modifiers::SHIFT));
        assert!(dialog.installed_selected(&mlx));
        assert!(dialog.installed_selected(&other));
        let rows = dialog.installed_rows(jiff::Timestamp::now());
        assert_eq!(rows[1].id, mlx);
        assert_eq!(rows[1].backend, "Metal");
        assert_eq!(rows[1].idle_minutes, 15);

        let command = if cfg!(target_os = "macos") {
            Modifiers::LOGO
        } else {
            Modifiers::CTRL
        };
        let _ = dialog.select_installed(Some(llama.clone()), Some(command));
        assert!(!dialog.installed_selected(&llama));
        assert!(dialog.installed_selected(&mlx));
        dialog.managed.sort_by(Column::Name);
        let rows = dialog.installed_rows(jiff::Timestamp::now());
        assert_eq!(rows[0].id, other);
        let _ = dialog.select_installed(Some(mlx.clone()), Some(Modifiers::empty()));
        assert_eq!(dialog.mlx.selected_models().count(), 1);
        assert!(dialog.managed.selected.is_none());
        assert!(!dialog.installed_selected(&other));

        let now = "2025-02-11T12:00:00Z".parse().unwrap();
        dialog.managed.sort_by(Column::Age);
        let rows = dialog.installed_rows(now);
        assert_eq!(rows[0].id, mlx);
        assert_eq!(
            publication_days(rows[0].published_at.as_deref(), now),
            Some(10)
        );
        assert_eq!(rows[1].id, other);
        assert_eq!(rows[2].id, llama);
        assert!(rows[2].published_at.is_none());
    }
}
