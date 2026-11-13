use std::path::PathBuf;
use std::sync::Arc;

use iced::widget::{button, column, image, row, text, text_input};
use iced::{ContentFit, Element, Task};

use crate::terminal_view::background_image::{self, AssetId, Prepared};

#[derive(Clone, Debug)]
pub enum Action {
    Path(String),
    Preview,
    Loaded(u64, Result<Arc<Prepared>, String>),
    Remove,
}

#[derive(Default)]
pub(super) struct Draft {
    pub(super) asset: Option<AssetId>,
    pub(super) prepared: Option<Arc<Prepared>>,
    java_path: Option<String>,
    path: String,
    pending: Option<u64>,
    problem: Option<String>,
}

impl Draft {
    pub(super) fn new(asset: Option<AssetId>, java_path: Option<&str>) -> Self {
        Self {
            asset,
            java_path: java_path.map(str::to_owned),
            ..Self::default()
        }
    }

    pub(super) fn working(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn cancel_read(&mut self) {
        self.pending = None;
    }

    pub(super) fn update(&mut self, action: Action, revision: &mut u64) -> Task<Action> {
        match action {
            Action::Path(path) if path.len() <= 4096 && !self.working() => {
                self.path = path;
                self.problem = None;
            }
            Action::Preview if !self.working() => {
                let path = PathBuf::from(&self.path);
                if !path.is_absolute()
                    || path.file_name().is_none()
                    || crate::ssh_dialog::unsafe_text(&self.path)
                {
                    self.problem = Some("Enter an absolute path to an image file.".into());
                    return Task::none();
                }
                *revision = revision.wrapping_add(1);
                let current = *revision;
                self.pending = Some(current);
                self.problem = None;
                return Task::perform(
                    async move {
                        background_image::prepare(&path)
                            .map(Arc::new)
                            .map_err(|error| error.to_string())
                    },
                    move |result| Action::Loaded(current, result),
                );
            }
            Action::Loaded(revision, result) if self.pending == Some(revision) => {
                self.pending = None;
                match result {
                    Ok(prepared) => {
                        self.asset = Some(prepared.id());
                        self.prepared = Some(prepared);
                        self.path.clear();
                    }
                    Err(problem) => self.problem = Some(problem),
                }
            }
            Action::Remove if !self.working() => {
                self.asset = None;
                self.prepared = None;
                self.problem = None;
            }
            _ => {}
        }
        Task::none()
    }

    pub(super) fn view<'a>(&'a self, existing: Option<&image::Handle>) -> Element<'a, Action> {
        let mut controls = column![
            text("Terminal background image").size(16),
            text_input("Absolute path to an image file", &self.path)
                .on_input_maybe((!self.working()).then_some(Action::Path)),
            row![
                button(if self.working() {
                    "Loading image…"
                } else {
                    "Preview image"
                })
                .on_press_maybe(
                    (!self.working() && !self.path.is_empty()).then_some(Action::Preview)
                ),
                button("Remove image").on_press_maybe(
                    (!self.working() && self.asset.is_some()).then_some(Action::Remove)
                ),
            ]
            .spacing(8)
            .wrap(),
        ]
        .spacing(7);
        if let Some(java_path) = &self.java_path {
            controls = controls.push(
                text(format!("Path from Java · {java_path}"))
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .size(12),
            );
        }
        if let Some(handle) = self
            .prepared
            .as_ref()
            .map(|value| value.handle())
            .or(existing)
        {
            controls = controls.push(
                image(handle.clone())
                    .width(160)
                    .height(88)
                    .content_fit(ContentFit::Contain),
            );
        } else if self.asset.is_some() {
            controls = controls.push(text("Background image unavailable.").size(12));
        }
        if let Some(problem) = &self.problem {
            controls = controls.push(text(problem).style(text::danger).size(13));
        }
        controls.into()
    }
}
