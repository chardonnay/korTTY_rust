use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

use iced::futures::{StreamExt as _, stream::BoxStream};
use iced::{Subscription, Task, window};
#[cfg(target_os = "macos")]
use kortty_macos::text_drop::{Watch, watch};
#[cfg(target_os = "windows")]
use kortty_windowing::windows_desktop_drop::{Watch, watch_text as watch};
#[cfg(target_os = "linux")]
use kortty_windowing::x11_desktop_drop::{Watch, watch_text as watch};

use super::terminal_text_drop::{PaneHit, PaneTarget};
use super::{Message as Desktop, Workspace};

#[derive(Clone)]
pub enum Message {
    Registered(window::Id, u64, Option<Watch>),
    Dropped(window::Id, u64, (f64, f64), String),
    Located(window::Id, u64, u64, Option<PaneTarget>, String),
}

impl fmt::Debug for Message {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Native terminal text drop")
    }
}

struct Registration {
    ticket: u64,
    watch: Option<Watch>,
    pending: Option<u64>,
}

pub(super) struct State {
    registrations: HashMap<window::Id, Registration>,
    sender: async_channel::Sender<Message>,
    receiver: async_channel::Receiver<Message>,
}

impl Default for State {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(16);
        Self {
            registrations: HashMap::new(),
            sender,
            receiver,
        }
    }
}

#[derive(Clone)]
struct Listener(async_channel::Receiver<Message>);

impl Hash for Listener {
    fn hash<H: Hasher>(&self, _: &mut H) {}
}

fn listen(listener: &Listener) -> BoxStream<'static, Desktop> {
    listener.0.clone().map(Desktop::DesktopTextDrop).boxed()
}

impl State {
    pub(super) fn subscription(&self) -> Subscription<Desktop> {
        Subscription::run_with(Listener(self.receiver.clone()), listen)
    }

    pub(super) fn forget(&mut self, owner: window::Id) {
        self.registrations.remove(&owner);
    }
}

impl Workspace {
    pub(super) fn register_desktop_text_drop(&mut self, owner: window::Id) -> Task<Desktop> {
        let ticket = self.allocate();
        self.desktop_text_drop.registrations.insert(
            owner,
            Registration {
                ticket,
                watch: None,
                pending: None,
            },
        );
        let sender = self.desktop_text_drop.sender.clone();
        window::run(owner, move |native| {
            watch(native.window_handle().ok()?, move |position, text| {
                sender
                    .try_send(Message::Dropped(owner, ticket, position, text))
                    .is_ok()
            })
        })
        .map(move |watch| Desktop::DesktopTextDrop(Message::Registered(owner, ticket, watch)))
    }

    pub(super) fn revalidate_desktop_text_drop(&self) {
        for (&owner, registration) in &self.desktop_text_drop.registrations {
            if let Some(watch) = &registration.watch {
                watch.set_enabled(!self.desktop_text_drop_candidates(owner).is_empty());
            }
        }
    }

    fn desktop_text_drop_candidates(
        &self,
        owner: window::Id,
    ) -> Vec<(iced::advanced::widget::Id, PaneTarget)> {
        self.pane_drop_candidates(owner, |target| self.text_drop_target_current(owner, target))
    }

    pub(super) fn update_desktop_text_drop(&mut self, message: Message) -> Task<Desktop> {
        let (owner, ticket) = match &message {
            Message::Registered(owner, ticket, ..)
            | Message::Dropped(owner, ticket, ..)
            | Message::Located(owner, ticket, ..) => (*owner, *ticket),
        };
        let Some(registration) = self
            .desktop_text_drop
            .registrations
            .get(&owner)
            .filter(|registration| registration.ticket == ticket)
        else {
            return Task::none();
        };
        match message {
            Message::Registered(_, _, watch) => {
                if let Some(registration) = self.desktop_text_drop.registrations.get_mut(&owner) {
                    registration.watch = watch;
                }
            }
            Message::Dropped(_, _, point, text) => {
                let candidates = self.desktop_text_drop_candidates(owner);
                if candidates.is_empty() {
                    return Task::none();
                }
                let request = self.allocate();
                if let Some(registration) = self.desktop_text_drop.registrations.get_mut(&owner) {
                    registration.pending = Some(request);
                }
                #[cfg(target_os = "linux")]
                let point = window::NativePosition::Physical(point);
                #[cfg(not(target_os = "linux"))]
                let point = window::NativePosition::Surface(point);
                return window::operate_at(owner, point, move |position| {
                    Box::new(PaneHit::new(position, candidates))
                })
                .map(move |target| {
                    Desktop::DesktopTextDrop(Message::Located(
                        owner,
                        ticket,
                        request,
                        target,
                        text.clone(),
                    ))
                });
            }
            Message::Located(_, _, request, target, text) => {
                if registration.pending != Some(request) {
                    return Task::none();
                }
                if let Some(registration) = self.desktop_text_drop.registrations.get_mut(&owner) {
                    registration.pending = None;
                }
                if let Some(target) = target {
                    self.admit_terminal_text_drop(owner, target, text);
                }
            }
        }
        Task::none()
    }
}
