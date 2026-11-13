//! Review a replacement and optional backup without touching the server worker.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::widget::{button, checkbox, column, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

use crate::sftp_window::worker::sync::replacement::draft::{self, Origin, Plan};

use super::Authority;

#[derive(Clone)]
pub enum Event {
    Open,
    Right,
    Path(String),
    Backup(bool),
    Review,
    Reviewed(u64, Result<Arc<Plan>, String>),
    Replace(u64),
    Replaced(u64, Result<Publication, String>),
    Back,
}

#[derive(Clone)]
pub struct Publication {
    pub(in crate::sftp_window::compare) path: PathBuf,
    pub(in crate::sftp_window::compare) note: String,
    pub(in crate::sftp_window::compare) digest: [u8; 32],
}

#[derive(Default)]
pub(super) struct State {
    phase: Phase,
    path: String,
    backup: bool,
    right: bool,
}

#[derive(Default)]
enum Phase {
    #[default]
    Hidden,
    Input,
    Checking {
        id: u64,
        _pending: Pending,
    },
    Review(u64, Arc<Plan>),
    Replacing(u64, Pending),
}

#[derive(Default)]
struct Pending(Arc<AtomicBool>);

impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl State {
    pub(super) fn busy(&self) -> bool {
        matches!(self.phase, Phase::Replacing(..))
    }

    pub(super) fn revoke(&self) {
        if let Phase::Replacing(_, pending) = &self.phase {
            pending.0.store(true, Ordering::Release);
        }
    }

    pub(super) fn publication_note<'a>(&self, event: &'a Event) -> Option<&'a str> {
        match (&self.phase, event) {
            (Phase::Replacing(current, _), Event::Replaced(id, result)) if current == id => {
                Some(match result {
                    Ok(receipt) => &receipt.note,
                    Err(note) => note,
                })
            }
            _ => None,
        }
    }

    pub(super) fn publication<'a>(&self, event: &'a Event) -> Option<&'a Publication> {
        match (&self.phase, event) {
            (Phase::Replacing(current, _), Event::Replaced(id, Ok(receipt))) if current == id => {
                Some(receipt)
            }
            _ => None,
        }
    }

    fn destination(&self, right: Option<&Origin>) -> Option<PathBuf> {
        if self.right {
            Some(right?.path().to_path_buf())
        } else {
            Some(PathBuf::from(&self.path))
        }
    }

    pub(super) fn update(
        &mut self,
        event: Event,
        authority: &Arc<Authority>,
        contents: &Arc<str>,
        right: Option<&Origin>,
        next: &mut u64,
        status: &mut Option<String>,
    ) -> Option<Task<Event>> {
        match event {
            Event::Open if matches!(self.phase, Phase::Hidden) => {
                self.phase = Phase::Input;
                self.backup = true;
                self.right = false;
                *status = None;
            }
            Event::Right if matches!(self.phase, Phase::Hidden) && right.is_some() => {
                self.phase = Phase::Input;
                self.backup = true;
                self.right = true;
                *status = None;
            }
            Event::Path(path)
                if matches!(self.phase, Phase::Input)
                    && !self.right
                    && path.len() <= 4096
                    && !path.chars().any(char::is_control) =>
            {
                self.path = path;
                *status = None;
            }
            Event::Backup(backup) if matches!(self.phase, Phase::Input) => {
                self.backup = backup;
                *status = None;
            }
            Event::Review if matches!(self.phase, Phase::Input) => {
                let path = self.destination(right)?;
                let origin = right.filter(|_| self.right).cloned();
                let contents = Arc::clone(contents);
                let backup = self.backup;
                let id = ticket(next);
                let (pending, task) = run(
                    move |stopped| {
                        let stopped = || stopped.load(Ordering::Acquire);
                        let plan = match origin {
                            Some(origin) => {
                                Plan::prepare_compared(&origin, contents, backup, &stopped)
                            }
                            None => Plan::prepare(path, contents, backup, &stopped),
                        };
                        plan.map(Arc::new).map_err(|error| error.message)
                    },
                    move |result| Event::Reviewed(id, result),
                );
                self.phase = Phase::Checking {
                    id,
                    _pending: pending,
                };
                *status = None;
                return Some(task);
            }
            Event::Reviewed(id, result) if matches!(self.phase, Phase::Checking { id: current, .. } if current == id) =>
            {
                self.phase = Phase::Input;
                match result {
                    Ok(plan)
                        if self
                            .destination(right)
                            .is_some_and(|path| plan.matches(&path, contents, self.backup)) =>
                    {
                        self.phase = Phase::Review(id, plan);
                    }
                    Ok(_) => *status = Some("The local destination or draft changed.".into()),
                    Err(error) => *status = Some(error),
                }
            }
            Event::Replace(id) => {
                let Phase::Review(current, plan) = &self.phase else {
                    return None;
                };
                if *current != id
                    || !self
                        .destination(right)
                        .is_some_and(|path| plan.matches(&path, contents, self.backup))
                {
                    return None;
                }
                let plan = Arc::clone(plan);
                let authority = Arc::clone(authority);
                let (pending, task) = run(
                    move |stopped| {
                        let stopped = || stopped.load(Ordering::Acquire);
                        plan.apply(&stopped, &|| authority.check())
                            .map(|note| Publication {
                                path: plan.destination().to_path_buf(),
                                note,
                                digest: plan.draft_digest(),
                            })
                            .map_err(|error| error.message)
                    },
                    move |result| Event::Replaced(id, result),
                );
                self.phase = Phase::Replacing(id, pending);
                *status = None;
                return Some(task);
            }
            Event::Replaced(id, result) if matches!(self.phase, Phase::Replacing(current, _) if current == id) =>
            {
                self.phase = if result.is_ok() {
                    Phase::Hidden
                } else {
                    Phase::Input
                };
                *status = Some(match result {
                    Ok(receipt) => receipt.note,
                    Err(note) => note,
                });
            }
            Event::Back => {
                self.phase = match &self.phase {
                    Phase::Replacing(_, pending) => {
                        pending.0.store(true, Ordering::Release);
                        *status = Some("Cancelling the local replacement…".into());
                        return None;
                    }
                    Phase::Review(..) | Phase::Checking { .. } => Phase::Input,
                    Phase::Input | Phase::Hidden => Phase::Hidden,
                };
                *status = None;
            }
            _ => {}
        }
        None
    }

    pub(super) fn view(&self, right: Option<&Origin>) -> Element<'_, Event> {
        match &self.phase {
            Phase::Hidden => row![
                right.map(|_| button("Replace the right local file…").on_press(Event::Right)),
                button("Replace existing local file…").on_press(Event::Open),
            ]
            .spacing(8)
            .into(),
            Phase::Input => {
                let destination: Element<'_, Event> = if self.right {
                    right
                        .map(|origin| {
                            text(format!("Right local file · {}", path_label(origin.path())))
                                .wrapping(text::Wrapping::WordOrGlyph)
                                .into()
                        })
                        .unwrap_or_else(|| {
                            text("The compared right local file is unavailable.").into()
                        })
                } else {
                    text_input("Absolute existing local file path", &self.path)
                        .on_input(Event::Path)
                        .into()
                };
                column![
                    destination,
                    checkbox(self.backup)
                        .label("Keep a .bak copy")
                        .on_toggle(Event::Backup),
                    row![
                        button("Review local replacement…").on_press_maybe(
                            self.destination(right)
                                .is_some_and(|path| draft::valid_path(&path))
                                .then_some(Event::Review),
                        ),
                        button("Back").on_press(Event::Back),
                    ]
                    .spacing(8),
                ]
                .spacing(6)
                .into()
            }
            Phase::Checking { .. } => row![
                text("Checking the local file…"),
                button("Cancel").on_press(Event::Back),
            ]
            .spacing(8)
            .into(),
            Phase::Review(id, plan) => {
                let [original, draft] = plan.digest_labels();
                let mut review = column![
                    text(format!(
                        "Replace local file · {}",
                        path_label(plan.destination())
                    ))
                    .wrapping(text::Wrapping::WordOrGlyph),
                    text(format!(
                        "{} original bytes · {} draft bytes",
                        plan.original_bytes(),
                        plan.draft_bytes()
                    )),
                    text(format!("Original SHA-256 · {original}"))
                        .wrapping(text::Wrapping::WordOrGlyph),
                    text(format!("Draft SHA-256 · {draft}")).wrapping(text::Wrapping::WordOrGlyph),
                ]
                .spacing(6);
                if let Some(backup) = plan.backup() {
                    review = review.push(
                        text(format!("Create backup · {}", path_label(backup)))
                            .wrapping(text::Wrapping::WordOrGlyph),
                    );
                }
                column![
                    scrollable(review).height(Length::Fixed(140.0)),
                    row![
                        button("Replace reviewed local file").on_press(Event::Replace(*id)),
                        button("Back").on_press(Event::Back),
                    ]
                    .spacing(8),
                ]
                .spacing(6)
                .into()
            }
            Phase::Replacing(..) => row![
                text("Replacing the reviewed local file…"),
                button("Cancel").on_press(Event::Back),
            ]
            .spacing(8)
            .into(),
        }
    }
}

fn path_label(path: &std::path::Path) -> String {
    let Some(path) = path.to_str() else {
        return path
            .as_os_str()
            .as_encoded_bytes()
            .escape_ascii()
            .to_string();
    };
    let mut label = String::with_capacity(path.len());
    for character in path.chars() {
        if character.is_control() || !crate::session_journal::is_display_character(character) {
            label.extend(character.escape_default());
        } else if character == '\\' {
            label.push_str("\\\\");
        } else {
            label.push(character);
        }
    }
    label
}

fn ticket(next: &mut u64) -> u64 {
    *next = next.wrapping_add(1);
    *next
}

fn run<T: Send + 'static>(
    work: impl FnOnce(Arc<AtomicBool>) -> Result<T, String> + Send + 'static,
    event: impl FnOnce(Result<T, String>) -> Event + Send + 'static,
) -> (Pending, Task<Event>) {
    let pending = Pending::default();
    let stopped = Arc::clone(&pending.0);
    let (send, reply) = async_channel::bounded(1);
    let started = std::thread::Builder::new()
        .name("kortty-local-draft".into())
        .spawn(move || {
            let result = work(stopped);
            let _ = send.try_send(result);
        });
    let task = Task::perform(
        async move {
            match started {
                Ok(_) => reply
                    .recv()
                    .await
                    .unwrap_or_else(|_| Err("The local file worker stopped.".into())),
                Err(error) => Err(format!("Could not start the local file worker ({error}).")),
            }
        },
        event,
    );
    (pending, task)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::path_label;

    #[test]
    fn sftp_ai_local_draft_review_labels_show_the_full_path_and_escape_hidden_characters() {
        let long = "/ordinary".repeat(130);
        let path = format!("{long}/café\u{202e}.txt");
        assert_eq!(
            path_label(Path::new(&path)),
            format!("{long}/café\\u{{202e}}.txt"),
        );
        assert_eq!(path_label(Path::new("C:\\folder")), "C:\\\\folder");
    }
}
