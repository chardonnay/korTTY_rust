use std::collections::HashMap;
use std::io::{Error, Result};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{SendError, Sender};
use std::sync::{Arc, LazyLock, Mutex, Weak};

use sctk::reexports::calloop::channel::Channel;
use sctk::reexports::calloop::{EventLoop, channel};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::Connection;
use sctk::reexports::client::globals::registry_queue_init;

use crate::state::{SelectionTarget, State};
use crate::{drag, file_drag, file_drop, selection};

static DISPLAYS: LazyLock<Mutex<HashMap<usize, Weak<Commands>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct Commands {
    sender: channel::Sender<Command>,
    pub(crate) png_pending: Arc<AtomicBool>,
}

impl Commands {
    pub(crate) fn send(&self, command: Command) -> std::result::Result<(), SendError<Command>> {
        self.sender.send(command)
    }
}

pub(crate) fn commands(display: usize) -> Option<Arc<Commands>> {
    DISPLAYS.lock().ok()?.get(&display)?.upgrade()
}

pub(crate) struct Registration {
    display: usize,
    commands: Arc<Commands>,
}

impl Registration {
    pub(crate) fn new(display: usize, sender: channel::Sender<Command>) -> Option<Self> {
        let commands = Arc::new(Commands {
            sender,
            png_pending: Arc::new(AtomicBool::new(false)),
        });
        DISPLAYS
            .lock()
            .ok()?
            .insert(display, Arc::downgrade(&commands));
        Some(Self { display, commands })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let Ok(mut displays) = DISPLAYS.lock() else {
            return;
        };
        if displays
            .get(&self.display)
            .is_some_and(|entry| entry.ptr_eq(&Arc::downgrade(&self.commands)))
        {
            displays.remove(&self.display);
        }
    }
}

/// Spawn a clipboard worker, which dispatches its own `EventQueue` and handles
/// clipboard requests.
pub fn spawn(
    name: String,
    display: Connection,
    rx_chan: Channel<Command>,
    worker_replier: Sender<Result<String>>,
) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            worker_impl(display, rx_chan, worker_replier);
        })
        .ok()
}

/// Clipboard worker thread command.
pub enum Command {
    /// Store data to a clipboard.
    Store(String),
    StorePng(selection::Request),
    /// Store data to a primary selection.
    StorePrimary(String),
    /// Load data from a clipboard.
    Load,
    /// Load primary selection.
    LoadPrimary,
    StartDrag(drag::Request),
    CancelDrag(Arc<Mutex<drag::DragProgress>>),
    StartFileDrag(file_drag::Request),
    CancelFileDrag(Arc<AtomicBool>),
    WatchFileDrops(Arc<file_drop::Registration>),
    UnwatchFileDrops(Arc<file_drop::Registration>),
    AcceptFileDrop(Arc<file_drop::Registration>, u64),
    FinishFileDrop(Arc<file_drop::Registration>, u64, bool),
    /// Shutdown the worker.
    Exit,
}

/// Handle clipboard requests.
fn worker_impl(
    connection: Connection,
    rx_chan: Channel<Command>,
    reply_tx: Sender<Result<String>>,
) {
    let (globals, event_queue) = match registry_queue_init(&connection) {
        Ok(data) => data,
        Err(_) => return,
    };

    let mut event_loop = EventLoop::<State>::try_new().unwrap();
    let loop_handle = event_loop.handle();

    let mut state = match State::new(
        &globals,
        &event_queue.handle(),
        loop_handle.clone(),
        reply_tx,
    ) {
        Some(state) => state,
        None => return,
    };

    let clipboard_connection = connection.clone();
    loop_handle
        .insert_source(rx_chan, move |event, _, state| {
            if let channel::Event::Msg(event) = event {
                match event {
                    Command::StorePrimary(contents) => {
                        state.store_selection(SelectionTarget::Primary, contents);
                    }
                    Command::Store(contents) => {
                        state.store_selection(SelectionTarget::Clipboard, contents);
                    }
                    Command::StorePng(request) => state.store_png(&clipboard_connection, request),
                    Command::Load if state.data_device_manager_state.is_some() => {
                        if let Err(err) = state.load_selection(SelectionTarget::Clipboard) {
                            let _ = state.reply_tx.send(Err(err));
                        }
                    }
                    Command::LoadPrimary if state.data_device_manager_state.is_some() => {
                        if let Err(err) = state.load_selection(SelectionTarget::Primary) {
                            let _ = state.reply_tx.send(Err(err));
                        }
                    }
                    Command::Load | Command::LoadPrimary => {
                        let _ = state
                            .reply_tx
                            .send(Err(Error::other("requested selection is not supported")));
                    }
                    Command::StartDrag(request) => state.request_drag(request),
                    Command::CancelDrag(progress) => state.cancel_drag(&progress),
                    Command::StartFileDrag(request) => state.request_file_drag(request),
                    Command::CancelFileDrag(live) => state.cancel_file_drag(&live),
                    Command::WatchFileDrops(registration) => state.watch_file_drops(registration),
                    Command::UnwatchFileDrops(registration) => {
                        state.unwatch_file_drops(&registration)
                    }
                    Command::AcceptFileDrop(registration, id) => {
                        state.accept_file_drop(&registration, id)
                    }
                    Command::FinishFileDrop(registration, id, accepted) => {
                        state.finish_file_drop(&registration, id, accepted)
                    }
                    Command::Exit => state.exit = true,
                }
            }
        })
        .unwrap();

    WaylandSource::new(connection, event_queue)
        .insert(loop_handle)
        .unwrap();

    loop {
        let timeout = [
            state.png_transfer_timeout(),
            state.file_drop_timeout(),
            state.file_drag_timeout(),
        ]
        .into_iter()
        .flatten()
        .min();
        if event_loop.dispatch(timeout, &mut state).is_err() || state.exit {
            break;
        }
        state.expire_png_transfers();
        state.expire_file_drops();
        state.expire_file_drag();
    }
    state.stop_file_drops();
    state.stop_file_drag();
}
