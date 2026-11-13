//! Pointer transfers across SFTP panes and file drops from the desktop.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use iced::advanced::widget::{Operation, Tree, tree};
use iced::advanced::{Clipboard, Layout, Renderer as _, Shell, Widget, layout, overlay, renderer};
use iced::{
    Border, Color, Element, Event, Length, Point, Rectangle, Renderer, Size, Task, Theme, Vector,
    mouse, window,
};

use super::{
    Directory, Entry, EntryKind, EntryName, MAX_SELECTION, Message, Panel as FilePanel, SftpWindow,
    Side, make_transfers, worker,
};

const DRAG_DISTANCE: f32 = 6.0;
const DESKTOP_DROP_SETTLE: Duration = Duration::from_millis(100);

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(super) mod external;
pub(super) mod local_copy;
#[cfg(test)]
mod tests;

#[derive(Clone)]
struct Source {
    side: Side,
    listing: Arc<Directory>,
    opposite_listing: Arc<Directory>,
    names: Arc<[EntryName]>,
}

#[derive(Clone)]
pub(crate) struct DesktopDestination {
    side: Side,
    listing: Arc<Directory>,
    directory: Option<EntryName>,
}

type Destination = DesktopDestination;

#[derive(Clone)]
pub struct TransferDrop {
    source: Source,
    destination: Destination,
}

impl fmt::Debug for TransferDrop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SFTP pointer transfer")
            .field("source", &self.source.side)
            .field("count", &self.source.names.len())
            .finish_non_exhaustive()
    }
}

impl TransferDrop {
    fn transfers(
        &self,
        source: &FilePanel,
        destination: &FilePanel,
    ) -> Option<VecDeque<worker::Transfer>> {
        let items = &self.source.names;
        if !self.destination.accepts(&self.source)
            || items.is_empty()
            || items.len() > MAX_SELECTION
            || !source
                .directory
                .as_ref()
                .is_some_and(|listing| Arc::ptr_eq(listing, &self.source.listing))
            || !destination
                .directory
                .as_ref()
                .is_some_and(|listing| Arc::ptr_eq(listing, &self.destination.listing))
        {
            return None;
        }
        let target = self.destination.current_path(destination)?;
        let transfers = make_transfers(
            &self.source.listing,
            &target,
            source.entries().filter(|entry| items.contains(&entry.name)),
        );
        (transfers.len() == items.len()).then_some(transfers)
    }
}

impl SftpWindow {
    pub(super) fn drop_transfers(&mut self, drop: TransferDrop) -> Task<Message> {
        if !self.queue_available() || self.busy.is_some() {
            return Task::none();
        }
        let source = drop.source.side;
        let Some(transfers) = drop.transfers(self.panel(source), self.panel(source.opposite()))
        else {
            return Task::none();
        };
        self.note_activity(std::time::Instant::now());
        self.queue_transfers(transfers)
    }
}

impl Destination {
    #[cfg(target_os = "linux")]
    pub(crate) fn is_remote(&self) -> bool {
        self.side == Side::Remote
    }
    fn accepts(&self, source: &Source) -> bool {
        self.side == source.side.opposite() && Arc::ptr_eq(&self.listing, &source.opposite_listing)
    }

    fn current_path(&self, panel: &FilePanel) -> Option<super::Location> {
        if !panel
            .directory
            .as_ref()
            .is_some_and(|listing| Arc::ptr_eq(listing, &self.listing))
        {
            return None;
        }
        if let Some(name) = &self.directory {
            let folder = self
                .listing
                .entries
                .iter()
                .find(|entry| &entry.name == name && entry.kind == EntryKind::Directory)?;
            self.listing.path.with(&folder.name)
        } else {
            Some(self.listing.path.clone())
        }
    }

    fn same_drop(&self, other: &Self) -> bool {
        self.side == other.side
            && Arc::ptr_eq(&self.listing, &other.listing)
            && self.directory == other.directory
    }
}

#[derive(Clone)]
pub struct DesktopDrop {
    destination: Destination,
    path: PathBuf,
}

impl DesktopDrop {
    pub(crate) fn is_remote(&self) -> bool {
        self.destination.side == Side::Remote
    }
}

impl fmt::Debug for DesktopDrop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SFTP desktop drop")
    }
}

#[derive(Clone)]
pub struct DesktopDropReply {
    ticket: Arc<()>,
    side: Side,
    ready: Result<Ready, Arc<str>>,
    rejected: usize,
}

#[derive(Clone)]
enum Ready {
    Remote(Arc<[worker::Transfer]>),
    Local(Arc<local_copy::Prepared>),
}

impl DesktopDropReply {
    pub(crate) fn is_remote(&self) -> bool {
        self.side == Side::Remote
    }
}

impl fmt::Debug for DesktopDropReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SFTP desktop drop validation")
    }
}

#[derive(Default)]
pub(super) struct DesktopDrops {
    pending: Option<DesktopBatch>,
    pub(super) local_job: Option<(local_copy::Job, Arc<Directory>)>,
}

struct DesktopBatch {
    ticket: Arc<()>,
    destination: Destination,
    paths: Vec<PathBuf>,
    omitted: usize,
    ready: Option<async_channel::Sender<PathBuf>>,
    #[cfg(target_os = "linux")]
    admission: Option<Arc<DropAdmission>>,
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct WaylandDrop {
    destination: Destination,
    paths: Vec<PathBuf>,
    admission: Arc<DropAdmission>,
}

#[cfg(target_os = "linux")]
type AdmissionCallback = Box<dyn FnOnce(bool) + Send>;

#[cfg(target_os = "linux")]
struct DropAdmission {
    permit: kortty_windowing::wayland_file_drop::Permit,
    owner: Arc<std::sync::atomic::AtomicBool>,
    callback: std::sync::Mutex<Option<AdmissionCallback>>,
}

#[cfg(target_os = "linux")]
impl DropAdmission {
    fn is_live(&self) -> bool {
        self.owner.load(std::sync::atomic::Ordering::Acquire) && self.permit.is_live()
    }

    fn finish(&self, accepted: bool) {
        let callback = self
            .callback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(callback) = callback {
            callback(accepted);
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for DropAdmission {
    fn drop(&mut self) {
        self.finish(false);
    }
}

#[cfg(target_os = "linux")]
impl fmt::Debug for WaylandDrop {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Wayland SFTP desktop drop")
    }
}

#[cfg(target_os = "linux")]
impl WaylandDrop {
    pub(crate) fn new(
        destination: DesktopDestination,
        paths: Vec<PathBuf>,
        permit: kortty_windowing::wayland_file_drop::Permit,
        owner: Arc<std::sync::atomic::AtomicBool>,
        completion: AdmissionCallback,
    ) -> Self {
        Self {
            destination,
            paths,
            admission: Arc::new(DropAdmission {
                permit,
                owner,
                callback: std::sync::Mutex::new(Some(completion)),
            }),
        }
    }

    pub(crate) fn is_remote(&self) -> bool {
        self.destination.is_remote()
    }
}

impl SftpWindow {
    pub(crate) fn cancel_desktop_drop(&mut self) {
        self.desktop_drop.pending = None;
    }

    pub(super) fn cancel_remote_desktop_drop(&mut self) {
        if self
            .desktop_drop
            .pending
            .as_ref()
            .is_some_and(|batch| batch.destination.side == Side::Remote)
        {
            self.cancel_desktop_drop();
        }
    }

    pub(crate) fn stop_local_desktop_copy(&self) -> Option<async_channel::Receiver<()>> {
        self.desktop_drop
            .local_job
            .as_ref()
            .map(|(job, _)| job.stop())
    }

    pub(super) fn begin_desktop_drop(&mut self, drop: DesktopDrop) -> Task<Message> {
        let side = drop.destination.side;
        if !self.desktop_drop_available(side) {
            return Task::none();
        }
        let Some(target) = drop.destination.current_path(self.panel(side)) else {
            return Task::none();
        };
        if !ordinary_path(&drop.path) {
            self.status = Some("Drop regular files or folders with absolute local paths.".into());
            return Task::none();
        }
        if let Some(batch) =
            self.desktop_drop.pending.as_mut().filter(|batch| {
                batch.ready.is_some() && batch.destination.same_drop(&drop.destination)
            })
        {
            if batch.paths.contains(&drop.path) {
                return Task::none();
            }
            if batch.paths.len() == MAX_SELECTION {
                batch.omitted = batch.omitted.saturating_add(1);
                return Task::none();
            }
            if batch
                .ready
                .as_ref()
                .is_some_and(|ready| ready.try_send(drop.path.clone()).is_ok())
            {
                batch.paths.push(drop.path);
            } else {
                batch.omitted = batch.omitted.saturating_add(1);
            }
            return Task::none();
        }
        let first = drop.path;
        let (ready, paths) = async_channel::bounded(MAX_SELECTION);
        let pending = DesktopBatch {
            ticket: Arc::new(()),
            destination: drop.destination,
            paths: vec![first.clone()],
            omitted: 0,
            ready: Some(ready),
            #[cfg(target_os = "linux")]
            admission: None,
        };
        self.inspect_desktop_drop(pending, target, move || {
            std::thread::sleep(DESKTOP_DROP_SETTLE);
            if paths.is_closed() {
                return None;
            }
            paths.close();
            Some(
                std::iter::once(first)
                    .chain(std::iter::from_fn(|| paths.try_recv().ok()))
                    .collect(),
            )
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn begin_wayland_desktop_drop(&mut self, drop: WaylandDrop) -> Task<Message> {
        let side = drop.destination.side;
        if !drop.admission.is_live() || !self.desktop_drop_available(side) {
            return Task::none();
        }
        let Some(target) = drop.destination.current_path(self.panel(side)) else {
            return Task::none();
        };
        let mut seen = std::collections::HashSet::new();
        let paths: Vec<_> = drop
            .paths
            .into_iter()
            .filter(|path| ordinary_path(path) && seen.insert(path.clone()))
            .take(MAX_SELECTION)
            .collect();
        if paths.is_empty() {
            return Task::none();
        }
        let pending = DesktopBatch {
            ticket: Arc::new(()),
            destination: drop.destination,
            paths: Vec::new(),
            omitted: 0,
            ready: None,
            admission: Some(drop.admission),
        };
        self.inspect_desktop_drop(pending, target, move || Some(paths))
    }

    fn inspect_desktop_drop(
        &mut self,
        pending: DesktopBatch,
        target: super::Location,
        paths: impl FnOnce() -> Option<Vec<PathBuf>> + Send + 'static,
    ) -> Task<Message> {
        self.cancel_desktop_drop();
        let side = pending.destination.side;
        let (send, receive) = async_channel::bounded(1);
        let completion = Arc::clone(&pending.ticket);
        let failure = Arc::clone(&pending.ticket);
        #[cfg(target_os = "linux")]
        let wayland = pending.admission.is_some();
        let worker = std::thread::Builder::new()
            .name("kortty-sftp-desktop-drop".into())
            .spawn(move || {
                let Some(paths) = paths() else {
                    return;
                };
                let (ready, rejected) = match target {
                    super::Location::Remote(_) => {
                        let (transfers, rejected) = local_transfers(paths.into_iter(), &target);
                        (Ok(Ready::Remote(transfers.into())), rejected)
                    }
                    super::Location::Local(target) => {
                        #[cfg(target_os = "linux")]
                        let prepared = if wayland {
                            local_copy::Prepared::from_wayland(target, paths)
                        } else {
                            local_copy::Prepared::new(target, paths).map(|prepared| (prepared, 0))
                        };
                        #[cfg(not(target_os = "linux"))]
                        let prepared =
                            local_copy::Prepared::new(target, paths).map(|prepared| (prepared, 0));
                        match prepared {
                            Ok((prepared, rejected)) => {
                                (Ok(Ready::Local(Arc::new(prepared))), rejected)
                            }
                            Err(error) => (Err(Arc::from(error.to_string())), 0),
                        }
                    }
                };
                let _ = send.try_send(DesktopDropReply {
                    ticket: completion,
                    side,
                    ready,
                    rejected,
                });
            });
        if worker.is_err() {
            self.status = Some("Could not inspect the dropped files.".into());
            return Task::none();
        }
        self.note_activity(std::time::Instant::now());
        self.desktop_drop.pending = Some(pending);
        Task::future(async move {
            match receive.recv().await {
                Ok(reply) => Message::DesktopDropReady(reply),
                Err(_) => Message::DesktopDropReady(DesktopDropReply {
                    ticket: failure,
                    side,
                    ready: Err("Could not inspect the dropped files.".into()),
                    rejected: 0,
                }),
            }
        })
    }

    pub(super) fn finish_desktop_drop(&mut self, reply: DesktopDropReply) -> Task<Message> {
        let Some(batch) = self
            .desktop_drop
            .pending
            .take_if(|batch| Arc::ptr_eq(&batch.ticket, &reply.ticket))
        else {
            return Task::none();
        };
        #[cfg(target_os = "linux")]
        if batch
            .admission
            .as_ref()
            .is_some_and(|admission| !admission.is_live())
        {
            return Task::none();
        }
        let side = batch.destination.side;
        if !self.desktop_drop_available(side)
            || batch.destination.current_path(self.panel(side)).is_none()
        {
            return Task::none();
        }
        let skipped = reply.rejected.saturating_add(batch.omitted);
        let (task, accepted) = match reply.ready {
            Ok(Ready::Local(prepared)) => {
                let task = self.copy_desktop_locally(prepared, batch.destination.listing, skipped);
                (task, self.desktop_drop.local_job.is_some())
            }
            Ok(Ready::Remote(transfers)) => {
                if skipped > 0 {
                    self.status = Some(format!(
                        "{skipped} dropped files or folders could not be queued."
                    ));
                }
                self.queue_transfers_report(transfers.iter().cloned().collect())
            }
            Err(error) => {
                self.status = Some(error.to_string());
                (Task::none(), false)
            }
        };
        #[cfg(target_os = "linux")]
        if let Some(admission) = batch.admission {
            admission.finish(accepted);
        }
        let _ = accepted;
        task
    }

    pub(super) fn desktop_drop_available(&self, side: Side) -> bool {
        self.busy.is_none()
            && self.desktop_drop.local_job.is_none()
            && self.prompt.is_none()
            && self.comparison.is_none()
            && !self.showing_image()
            && match side {
                Side::Local => {
                    self.local.directory.is_some()
                        && self.local_listing_request.is_none()
                        && !self.parallel.active()
                }
                Side::Remote => self.queue_available(),
            }
    }
}

fn ordinary_path(path: &Path) -> bool {
    path.is_absolute()
        && path.file_name().is_some()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path.components().any(|part| part == Component::ParentDir)
}

#[cfg(target_os = "linux")]
pub(crate) fn desktop_drop_target(
    point: Option<Point>,
) -> Box<dyn Operation<Option<DesktopDestination>>> {
    Box::new(DropTarget {
        point,
        translation: Vector::ZERO,
        scrolled: None,
        destination: None,
    })
}

#[cfg(target_os = "linux")]
struct DropTarget {
    point: Option<Point>,
    translation: Vector,
    scrolled: Option<(Rectangle, Vector)>,
    destination: Option<DesktopDestination>,
}

#[cfg(target_os = "linux")]
impl Operation<Option<DesktopDestination>> for DropTarget {
    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn Operation<Option<DesktopDestination>>),
    ) {
        let Some(point) = self.point else { return };
        if let Some((bounds, translation)) = self.scrolled.take() {
            if bounds.contains(point + self.translation) {
                let original = self.translation;
                self.translation = original + translation;
                operate(self);
                self.translation = original;
            }
        } else {
            operate(self);
        }
    }

    fn scrollable(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        translation: Vector,
        _state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        self.scrolled = Some((bounds, translation));
    }

    fn custom(
        &mut self,
        _id: Option<&iced::advanced::widget::Id>,
        bounds: Rectangle,
        state: &mut dyn std::any::Any,
    ) {
        if self.destination.is_none()
            && self
                .point
                .is_some_and(|point| bounds.contains(point + self.translation))
            && let Some(destination) = state.downcast_ref::<DesktopDestination>()
        {
            self.destination = Some(destination.clone());
        }
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<Option<DesktopDestination>> {
        iced::advanced::widget::operation::Outcome::Some(self.destination.clone())
    }
}

fn local_transfers(
    paths: impl Iterator<Item = PathBuf>,
    target: &super::Location,
) -> (Vec<worker::Transfer>, usize) {
    let mut transfers = Vec::new();
    let mut rejected = 0;
    for path in paths.take(MAX_SELECTION) {
        if let Some(transfer) = local_transfer(path, target) {
            transfers.push(transfer);
        } else {
            rejected += 1;
        }
    }
    (transfers, rejected)
}

fn local_transfer(path: PathBuf, target: &super::Location) -> Option<worker::Transfer> {
    if !ordinary_path(&path) || !matches!(target, super::Location::Remote(_)) {
        return None;
    }
    let name = path.file_name()?.to_owned();
    super::files::remote_name(&name).ok()?;
    let parent = path.parent()?.canonicalize().ok()?;
    let path = parent.join(&name);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    let kind = super::files::local_kind(&metadata);
    if !kind.copyable() {
        return None;
    }
    Some(worker::Transfer {
        source: super::Location::Local(path),
        target_directory: target.clone(),
        name: EntryName::Local(name.clone()),
        kind,
        title: super::files::display(&name.to_string_lossy()),
        size: metadata.is_file().then_some(metadata.len()),
    })
}

pub(super) struct Panel {
    side: Side,
    listing: Arc<Directory>,
    opposite_listing: Arc<Directory>,
    selected: Arc<[EntryName]>,
}

impl Panel {
    pub(super) fn new(side: Side, panel: &FilePanel, other: &FilePanel) -> Option<Self> {
        Some(Self {
            side,
            listing: Arc::clone(panel.directory.as_ref()?),
            opposite_listing: Arc::clone(other.directory.as_ref()?),
            selected: panel.selected.iter().cloned().collect(),
        })
    }

    pub(super) fn background(&self) -> Item {
        Item {
            source: None,
            destination: Some(self.destination(None)),
            desktop: true,
        }
    }

    pub(super) fn row(&self, entry: &Entry) -> Item {
        Item {
            source: entry.kind.copyable().then(|| Source {
                side: self.side,
                listing: Arc::clone(&self.listing),
                opposite_listing: Arc::clone(&self.opposite_listing),
                names: if self.selected.contains(&entry.name) {
                    Arc::clone(&self.selected)
                } else {
                    Arc::new([entry.name.clone()])
                },
            }),
            destination: (entry.kind == EntryKind::Directory)
                .then(|| self.destination(Some(entry.name.clone()))),
            desktop: entry.kind == EntryKind::Directory,
        }
    }

    fn destination(&self, directory: Option<EntryName>) -> Destination {
        Destination {
            side: self.side,
            listing: Arc::clone(&self.listing),
            directory,
        }
    }
}

pub(super) struct Item {
    source: Option<Source>,
    destination: Option<Destination>,
    desktop: bool,
}

impl Item {
    pub(super) fn desktop(
        side: Side,
        listing: Option<&Arc<Directory>>,
        directory: Option<EntryName>,
    ) -> Self {
        Self {
            source: None,
            destination: listing.map(|listing| Destination {
                side,
                listing: Arc::clone(listing),
                directory,
            }),
            desktop: true,
        }
    }
}

pub(super) fn scope<'a>(build: impl FnOnce(Zone) -> Element<'a, Message>) -> Element<'a, Message> {
    let zone = Zone(Rc::new(RefCell::new(Context::default())));
    Element::new(Area {
        child: build(zone.clone()),
        zone,
        item: None,
    })
}

#[derive(Clone)]
pub(super) struct Zone(Rc<RefCell<Context>>);

impl Zone {
    pub(super) fn item<'a>(
        &self,
        child: impl Into<Element<'a, Message>>,
        item: Option<Item>,
    ) -> Element<'a, Message> {
        let child = child.into();
        match item {
            Some(item) => Element::new(Area {
                child,
                zone: self.clone(),
                item: Some(item),
            }),
            None => child,
        }
    }
}

#[derive(Default)]
struct Context {
    event: Dispatch,
    active: Option<Source>,
    desktop_hover: bool,
    #[cfg(target_os = "linux")]
    pointer: Option<Point>,
}

#[derive(Default)]
enum Dispatch {
    #[default]
    Idle,
    Press {
        origin: Point,
        source: Option<Source>,
    },
    Release {
        source: Source,
        destination: Option<Destination>,
    },
}

struct Press {
    origin: Point,
    source: Source,
    dragging: bool,
}

#[derive(Default)]
struct Gesture {
    pointer: Option<Point>,
    press: Option<Press>,
    desktop_hover: bool,
}

impl Gesture {
    fn active(&self) -> Option<&Source> {
        self.press
            .as_ref()
            .filter(|press| press.dragging)
            .map(|press| &press.source)
    }

    fn event(&mut self, event: &Event, cursor: &mut mouse::Cursor) -> Dispatch {
        match event {
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                self.pointer = Some(*position);
                if let Some(press) = self.press.as_mut() {
                    press.dragging |= press.origin.distance(*position) >= DRAG_DISTANCE;
                }
            }
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                self.press = None;
                if let Some(position) = cursor.position() {
                    let origin = self.pointer.unwrap_or(position);
                    *cursor = mouse::Cursor::Available(origin);
                    return Dispatch::Press {
                        origin,
                        source: None,
                    };
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if let Some(source) = self
                    .press
                    .take()
                    .filter(|press| press.dragging)
                    .map(|press| press.source)
                {
                    if let Some(position) = cursor.position() {
                        *cursor = mouse::Cursor::Available(self.pointer.unwrap_or(position));
                    }
                    return Dispatch::Release {
                        source,
                        destination: None,
                    };
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) | Event::Window(window::Event::Unfocused) => {
                *self = Self::default();
            }
            Event::Window(window::Event::FileHovered(_)) => self.desktop_hover = true,
            Event::Window(window::Event::FileDropped(_) | window::Event::FilesHoveredLeft) => {
                self.desktop_hover = false;
            }
            _ => {}
        }
        Dispatch::Idle
    }
}

struct Area<'a> {
    child: Element<'a, Message>,
    zone: Zone,
    item: Option<Item>,
}

impl Area<'_> {
    fn child_tree<'a>(&self, tree: &'a Tree) -> &'a Tree {
        if self.item.is_none() {
            &tree.children[0]
        } else {
            tree
        }
    }

    fn child_tree_mut<'a>(&self, tree: &'a mut Tree) -> &'a mut Tree {
        if self.item.is_none() {
            &mut tree.children[0]
        } else {
            tree
        }
    }
}

impl Widget<Message, Theme, Renderer> for Area<'_> {
    fn tag(&self) -> tree::Tag {
        if self.item.is_none() {
            tree::Tag::of::<Gesture>()
        } else {
            self.child.as_widget().tag()
        }
    }

    fn state(&self) -> tree::State {
        if self.item.is_none() {
            tree::State::new(Gesture::default())
        } else {
            self.child.as_widget().state()
        }
    }

    fn children(&self) -> Vec<Tree> {
        if self.item.is_none() {
            vec![Tree::new(&self.child)]
        } else {
            self.child.as_widget().children()
        }
    }

    fn diff(&self, tree: &mut Tree) {
        if self.item.is_none() {
            tree.diff_children(&[&self.child]);
        } else {
            self.child.as_widget().diff(tree);
        }
    }

    fn size(&self) -> Size<Length> {
        self.child.as_widget().size()
    }

    fn size_hint(&self) -> Size<Length> {
        self.child.as_widget().size_hint()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let child_tree = self.child_tree_mut(tree);
        self.child
            .as_widget_mut()
            .layout(child_tree, renderer, limits)
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        let child_tree = self.child_tree_mut(tree);
        self.child
            .as_widget_mut()
            .operate(child_tree, layout, renderer, operation);
        #[cfg(target_os = "linux")]
        if let Some(item) = &self.item
            && item.desktop
            && let Some(destination) = &item.destination
        {
            let mut destination = destination.clone();
            operation.custom(None, layout.bounds(), &mut destination);
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        mut cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        if self.item.is_none() {
            let gesture = tree.state.downcast_mut::<Gesture>();
            let before = gesture.active().is_some() || gesture.desktop_hover;
            let mut context = self.zone.0.borrow_mut();
            context.event = gesture.event(event, &mut cursor);
            #[cfg(target_os = "linux")]
            {
                context.pointer = cursor.position();
            }
            if before || gesture.active().is_some() || gesture.desktop_hover {
                shell.request_redraw();
            }
        }
        let over = cursor
            .position_over(layout.bounds())
            .is_some_and(|point| viewport.contains(point));
        let release = if let Some(item) = &self.item {
            let mut context = self.zone.0.borrow_mut();
            match &mut context.event {
                Dispatch::Press { source, .. }
                    if source.is_none() && !shell.is_event_captured() && over =>
                {
                    source.clone_from(&item.source);
                    false
                }
                Dispatch::Release { .. } => true,
                _ => false,
            }
        } else {
            false
        };
        let child_cursor =
            if release && self.item.as_ref().is_some_and(|item| item.source.is_some()) {
                cursor.levitate()
            } else {
                cursor
            };
        let child_tree = self.child_tree_mut(tree);
        self.child.as_widget_mut().update(
            child_tree,
            event,
            layout,
            child_cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );

        if let Some(item) = &self.item {
            if over
                && item.desktop
                && !shell.is_event_captured()
                && let Event::Window(window::Event::FileHovered(_) | window::Event::FileDropped(_)) =
                    event
            {
                if let Some(destination) = &item.destination
                    && let Event::Window(window::Event::FileDropped(path)) = event
                {
                    shell.publish(Message::DesktopDrop(DesktopDrop {
                        destination: destination.clone(),
                        path: path.clone(),
                    }));
                }
                shell.capture_event();
            }
            if over
                && let Some(target) = &item.destination
                && let Dispatch::Release {
                    source,
                    destination,
                } = &mut self.zone.0.borrow_mut().event
                && destination.is_none()
                && target.accepts(source)
            {
                *destination = Some(target.clone());
            }
        } else {
            if over && matches!(event, Event::Window(window::Event::FileDropped(_))) {
                shell.capture_event();
            }
            match std::mem::take(&mut self.zone.0.borrow_mut().event) {
                Dispatch::Press {
                    origin,
                    source: Some(source),
                } => {
                    tree.state.downcast_mut::<Gesture>().press = Some(Press {
                        origin,
                        source,
                        dragging: false,
                    });
                }
                Dispatch::Release {
                    source,
                    destination,
                } => {
                    if let Some(destination) = destination {
                        shell.publish(Message::DropTransfer(TransferDrop {
                            source,
                            destination,
                        }));
                    }
                    shell.capture_event();
                }
                _ => {}
            }
        }
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        if self.item.is_none()
            && tree.state.downcast_ref::<Gesture>().active().is_some()
            && cursor.position().is_some()
        {
            return mouse::Interaction::Grabbing;
        }
        self.child.as_widget().mouse_interaction(
            self.child_tree(tree),
            layout,
            cursor,
            viewport,
            renderer,
        )
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
        if self.item.is_none() {
            let gesture = tree.state.downcast_ref::<Gesture>();
            let mut context = self.zone.0.borrow_mut();
            context.active = gesture.active().cloned();
            context.desktop_hover = gesture.desktop_hover;
        }
        self.child.as_widget().draw(
            self.child_tree(tree),
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
        if let Some(destination) = self
            .item
            .as_ref()
            .and_then(|item| item.destination.as_ref())
            && {
                let context = self.zone.0.borrow();
                context
                    .active
                    .as_ref()
                    .is_some_and(|source| destination.accepts(source))
                    || context.desktop_hover
            }
            && let Some(bounds) = layout.bounds().intersection(viewport)
            && cursor.is_over(bounds)
        {
            renderer.fill_quad(
                renderer::Quad {
                    bounds,
                    border: Border {
                        color: theme.extended_palette().primary.strong.color,
                        width: 2.0,
                        ..Border::default()
                    },
                    ..renderer::Quad::default()
                },
                Color::TRANSPARENT,
            );
        }
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        let child_tree = self.child_tree_mut(tree);
        self.child
            .as_widget_mut()
            .overlay(child_tree, layout, renderer, viewport, translation)
    }
}
