//! Keep the fitted dialog size through initial Wayland placement.

use std::sync::{Arc, OnceLock};

use iced::{Size, Subscription, Task, window};
use iced_runtime::futures::subscription::{self, Event};

use crate::workspace::Message;

#[derive(Clone, Default)]
pub(in crate::workspace::dialog_placement) struct Opening {
    limits: Arc<OnceLock<Limits>>,
    first_redraw: bool,
}

#[derive(Clone, Copy)]
struct Limits {
    min: Option<Size>,
    max: Option<Size>,
}

impl Opening {
    pub(in crate::workspace::dialog_placement) fn new(
        remembered: bool,
        settings: &window::Settings,
    ) -> Option<Self> {
        (remembered
            && settings.visible
            && settings.resizable
            && !settings.maximized
            && !settings.fullscreen)
            .then(Self::default)
    }

    pub(super) fn constrain(&self, settings: &mut window::Settings) {
        let size = settings.size;
        // Preserve the dialog's normal resize limits and Winit's minimum surface size.
        if size.width < 2.0
            || size.height < 1.0
            || settings
                .min_size
                .is_some_and(|min| size.width < min.width || size.height < min.height)
            || settings
                .max_size
                .is_some_and(|max| size.width > max.width || size.height > max.height)
        {
            return;
        }
        // Mutter only auto-maximizes resizable windows on initial placement.
        let _ = self.limits.set(Limits {
            min: settings.min_size,
            max: settings.max_size,
        });
        settings.min_size = Some(size);
        settings.max_size = Some(size);
    }

    pub(in crate::workspace::dialog_placement) fn placed(&mut self) -> bool {
        // The first redraw is announced before the first commit. Winit waits for
        // Wayland's frame callback before delivering another redraw for this window.
        !self.constrained() || std::mem::replace(&mut self.first_redraw, true)
    }

    pub(in crate::workspace::dialog_placement) fn subscription(
        id: window::Id,
    ) -> Subscription<Message> {
        #[derive(Hash)]
        struct OpeningRedraw;

        subscription::filter_map((OpeningRedraw, id), move |event| match event {
            Event::Interaction {
                window,
                event: iced::Event::Window(window::Event::RedrawRequested(_)),
                ..
            } if window == id => Some(Message::DialogOpeningRedraw(id)),
            _ => None,
        })
    }

    pub(in crate::workspace::dialog_placement) fn constrained(&self) -> bool {
        self.limits.get().is_some()
    }

    pub(in crate::workspace::dialog_placement) fn release(self, id: window::Id) -> Task<Message> {
        let Some(&Limits { min, max }) = self.limits.get() else {
            return Task::none();
        };
        window::set_min_size(id, min)
            .chain(window::set_max_size(id, max))
            .chain(iced_runtime::task::effect(iced_runtime::Action::Window(
                iced_runtime::window::Action::RedrawAll,
            )))
    }
}
