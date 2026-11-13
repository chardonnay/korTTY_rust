//! The explicit desktop grip leaves the internal pane gesture available.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use iced::advanced::widget::{Tree, tree};
use iced::advanced::{Clipboard, Layout, Shell, Widget, layout, renderer};
use iced::{Element, Event, Length, Point, Rectangle, Renderer, Size, Theme, mouse, window};

use super::super::{
    Directory, EntryKind, EntryName, Location, MAX_SELECTION, Message, SftpWindow, Side,
    worker::drag_out,
};
use super::{DRAG_DISTANCE, Zone};

#[derive(Clone)]
pub struct Request {
    #[cfg(target_os = "linux")]
    pub(crate) origin: Point,
    side: Side,
    listing: Arc<Directory>,
    names: Arc<[EntryName]>,
}

impl Request {
    pub(crate) fn is_remote(&self) -> bool {
        self.side == Side::Remote
    }

    pub(crate) fn is_current(&self, state: &SftpWindow) -> bool {
        if self.is_remote() {
            return state
                .current_drag_selection()
                .is_some_and(|selection| selection.matches(&self.listing, &self.names));
        }
        state.desktop_drop_available(Side::Local)
            && state
                .local
                .directory
                .as_ref()
                .is_some_and(|listing| Arc::ptr_eq(listing, &self.listing))
            && self.names.len() == state.local.selected.len()
            && self
                .names
                .iter()
                .all(|name| state.local.selected.contains(name))
    }
}

#[derive(Default)]
pub(in crate::sftp_window) struct State {
    sequence: u64,
    pending: Option<(u64, Selection, drag_out::Preparation)>,
    prepared: Option<(Selection, Arc<drag_out::Prepared>, Instant)>,
}

#[derive(Clone)]
struct Selection {
    listing: Arc<Directory>,
    names: Arc<[EntryName]>,
}

impl Selection {
    fn matches(&self, listing: &Arc<Directory>, names: &[EntryName]) -> bool {
        Arc::ptr_eq(listing, &self.listing) && self.names.as_ref() == names
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SFTP external file drag")
    }
}

impl SftpWindow {
    const PREPARING_DRAG: &str = "Preparing selected files for desktop drag…";
    const PREPARED_DRAG: &str = "Server files are ready. Use Drag selection to desktop.";

    pub(crate) fn external_local_paths(&self, request: &Request) -> Option<Vec<PathBuf>> {
        if !self.desktop_drop_available(Side::Local)
            || !self
                .local
                .directory
                .as_ref()
                .is_some_and(|listing| Arc::ptr_eq(listing, &request.listing))
        {
            return None;
        }
        request
            .names
            .iter()
            .map(|name| {
                let entry = request
                    .listing
                    .entries
                    .iter()
                    .find(|entry| &entry.name == name && entry.kind.transferable())?;
                let Location::Local(path) = request.listing.path.with(&entry.name)? else {
                    return None;
                };
                Some(path)
            })
            .collect()
    }

    fn current_drag_selection(&self) -> Option<Selection> {
        if !self.desktop_drop_available(Side::Remote) || !self.transfer_allowed {
            return None;
        }
        let listing = self.remote.directory.as_ref()?;
        let mut names = Vec::new();
        let mut remaining = drag_out::MAX_BYTES;
        for entry in listing
            .entries
            .iter()
            .filter(|entry| self.remote.selected.contains(&entry.name))
        {
            let size = entry.size.filter(|&size| size <= remaining)?;
            let EntryName::Remote(name) = &entry.name else {
                return None;
            };
            if !matches!(entry.kind, EntryKind::File | EntryKind::Link)
                || drag_out::valid_name(name).is_none()
            {
                return None;
            }
            remaining -= size;
            names.push(entry.name.clone());
        }
        (names.len() == self.remote.selected.len()
            && (1..=drag_out::MAX_FILES).contains(&names.len()))
        .then(|| Selection {
            listing: Arc::clone(listing),
            names: names.into(),
        })
    }

    pub(crate) fn prepare_remote_desktop_drag(&mut self) -> iced::Task<Message> {
        if self.desktop_drag.pending.is_some() {
            return iced::Task::none();
        }
        let Some(selection) = self.current_drag_selection() else {
            return iced::Task::none();
        };
        let Some(worker) = self.worker.as_ref() else {
            return iced::Task::none();
        };
        let candidates: Option<Vec<_>> = selection
            .names
            .iter()
            .map(|name| {
                let entry = selection
                    .listing
                    .entries
                    .iter()
                    .find(|entry| &entry.name == name)?;
                drag_out::Candidate::for_entry(&selection.listing.path, entry)
            })
            .collect();
        let Some(candidates) = candidates else {
            return iced::Task::none();
        };
        let (preparation, receive) = match worker.prepare_desktop_drag(candidates) {
            Ok(work) => work,
            Err(error) => {
                self.set_status(error.to_string());
                return iced::Task::none();
            }
        };
        self.desktop_drag.prepared = None;
        self.desktop_drag.sequence = self.desktop_drag.sequence.wrapping_add(1);
        let ticket = self.desktop_drag.sequence;
        self.desktop_drag.pending = Some((ticket, selection, preparation));
        self.set_status(Self::PREPARING_DRAG.into());
        iced::Task::perform(
            async move {
                match receive.recv().await {
                    Ok(Ok(prepared)) => Ok(Arc::new(prepared)),
                    Ok(Err(error)) => Err(Arc::<str>::from(error.to_string())),
                    Err(_) => Err(Arc::<str>::from("Desktop drag preparation stopped.")),
                }
            },
            move |result| Message::ExternalFileDragPrepared(ticket, result),
        )
    }

    pub(crate) fn remote_desktop_drag_prepared(
        &mut self,
        ticket: u64,
        result: Result<Arc<drag_out::Prepared>, Arc<str>>,
    ) {
        let Some((_, selection, _)) = self
            .desktop_drag
            .pending
            .take_if(|(current, _, _)| *current == ticket)
        else {
            return;
        };
        self.clear_desktop_drag_status();
        let Some(current) = self.current_drag_selection() else {
            return;
        };
        if !selection.matches(&current.listing, &current.names) {
            return;
        }
        match result {
            Ok(prepared)
                if self
                    .worker
                    .as_ref()
                    .is_some_and(|worker| prepared.matches(worker)) =>
            {
                self.desktop_drag.prepared = Some((
                    selection,
                    prepared,
                    Instant::now() + drag_out::PREPARED_LIFETIME,
                ));
                self.set_status(Self::PREPARED_DRAG.into());
            }
            Ok(_) => {}
            Err(error) => self.set_status(error.to_string()),
        }
    }

    pub(crate) fn take_prepared_desktop_drag(
        &mut self,
        request: &Request,
    ) -> Option<Arc<drag_out::Prepared>> {
        let current = self.current_drag_selection()?;
        if !current.matches(&request.listing, &request.names) {
            return None;
        }
        let (selection, prepared, deadline) = self.desktop_drag.prepared.take()?;
        self.clear_desktop_drag_status();
        (Instant::now() < deadline
            && selection.matches(&request.listing, &request.names)
            && self
                .worker
                .as_ref()
                .is_some_and(|worker| prepared.matches(worker)))
        .then_some(prepared)
    }

    pub(crate) fn cancel_prepared_desktop_drag(&mut self) {
        self.desktop_drag.pending = None;
        self.desktop_drag.prepared = None;
        self.clear_desktop_drag_status();
    }

    fn clear_desktop_drag_status(&mut self) {
        if matches!(
            self.status.as_deref(),
            Some(Self::PREPARING_DRAG | Self::PREPARED_DRAG)
        ) {
            self.status = None;
        }
    }

    pub(crate) fn revalidate_prepared_desktop_drag(&mut self) {
        if self.desktop_drag.pending.is_none() && self.desktop_drag.prepared.is_none() {
            return;
        }
        let current = self.current_drag_selection();
        let is_current = |selection: &Selection| {
            current
                .as_ref()
                .is_some_and(|current| selection.matches(&current.listing, &current.names))
        };
        if self
            .desktop_drag
            .pending
            .as_ref()
            .is_some_and(|(_, selection, _)| !is_current(selection))
        {
            self.desktop_drag.pending = None;
            self.clear_desktop_drag_status();
        }
        if self
            .desktop_drag
            .prepared
            .as_ref()
            .is_some_and(|(selection, prepared, deadline)| {
                !is_current(selection)
                    || Instant::now() >= *deadline
                    || self
                        .worker
                        .as_ref()
                        .is_none_or(|worker| !prepared.matches(worker))
            })
        {
            self.desktop_drag.prepared = None;
            self.clear_desktop_drag_status();
        }
    }

    pub(crate) fn has_prepared_desktop_drag(&self) -> bool {
        self.desktop_drag.prepared.is_some()
    }

    pub(crate) fn has_desktop_drag_preparation(&self) -> bool {
        self.desktop_drag.pending.is_some() || self.has_prepared_desktop_drag()
    }

    pub(in crate::sftp_window) fn remote_drag_control<'a>(
        &self,
        zone: Zone,
    ) -> Option<Element<'a, Message>> {
        use iced::widget::{button, container, text};

        let selection = self.current_drag_selection()?;
        if self
            .desktop_drag
            .prepared
            .as_ref()
            .is_some_and(|(prepared, _, deadline)| {
                Instant::now() < *deadline && prepared.matches(&selection.listing, &selection.names)
            })
        {
            let handle = container(text("Drag selection to desktop").size(13))
                .padding([5, 8])
                .style(container::bordered_box);
            return Some(grip(
                handle.into(),
                &selection.listing,
                selection.names,
                Side::Remote,
                zone,
            ));
        }
        let preparing = self.desktop_drag.pending.is_some();
        Some(
            button(if preparing {
                "Preparing desktop drag…"
            } else {
                "Prepare desktop drag"
            })
            .on_press_maybe((!preparing).then_some(Message::PrepareExternalFileDrag))
            .into(),
        )
    }
}

pub(in crate::sftp_window) fn grip<'a>(
    child: Element<'a, Message>,
    listing: &Arc<Directory>,
    names: Arc<[EntryName]>,
    side: Side,
    _zone: Zone,
) -> Element<'a, Message> {
    if names.is_empty() || names.len() > MAX_SELECTION {
        return child;
    }
    file_drag_grip(
        child,
        (side, Arc::clone(listing), names),
        #[cfg(target_os = "linux")]
        move |_| _zone.0.borrow().pointer,
        #[cfg(target_os = "windows")]
        mouse::Cursor::position,
        |(side, listing, names), _origin| {
            Message::ExternalFileDrag(Request {
                #[cfg(target_os = "linux")]
                origin: _origin,
                side,
                listing,
                names,
            })
        },
    )
}

pub(crate) fn file_drag_grip<'a, M: 'a, Selection: Clone + 'static>(
    child: Element<'a, M>,
    selection: Selection,
    origin: impl Fn(mouse::Cursor) -> Option<Point> + 'a,
    start: fn(Selection, Point) -> M,
) -> Element<'a, M> {
    Element::new(Grip {
        child,
        selection,
        origin: Box::new(origin),
        start,
    })
}

struct Grip<'a, M, Selection> {
    child: Element<'a, M>,
    selection: Selection,
    origin: Box<dyn Fn(mouse::Cursor) -> Option<Point> + 'a>,
    start: fn(Selection, Point) -> M,
}

struct Press<Selection>(Option<(Point, Selection)>);

impl<M, Selection: Clone + 'static> Widget<M, Theme, Renderer> for Grip<'_, M, Selection> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Press<Selection>>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Press::<Selection>(None))
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.child.as_widget_mut().layout(tree, renderer, limits)
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        let press = &mut tree.state.downcast_mut::<Press<Selection>>().0;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                *press = cursor
                    .position_over(layout.bounds())
                    .filter(|point| viewport.contains(*point))
                    .and_then(|_| (self.origin)(cursor))
                    .map(|origin| (origin, self.selection.clone()));
                if press.is_some() {
                    shell.capture_event();
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                if let Some(position) = (self.origin)(cursor)
                    && let Some((origin, selection)) =
                        press.take_if(|(origin, _)| origin.distance(position) >= DRAG_DISTANCE)
                {
                    shell.publish((self.start)(selection, origin));
                    shell.capture_event();
                }
            }
            Event::Mouse(
                mouse::Event::ButtonReleased(mouse::Button::Left) | mouse::Event::CursorLeft,
            )
            | Event::Window(window::Event::Unfocused) => {
                *press = None;
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        _: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        _: &Renderer,
    ) -> mouse::Interaction {
        if cursor
            .position_over(layout.bounds())
            .is_some_and(|point| viewport.contains(point))
        {
            mouse::Interaction::Grab
        } else {
            mouse::Interaction::None
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.child
            .as_widget()
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }
}
