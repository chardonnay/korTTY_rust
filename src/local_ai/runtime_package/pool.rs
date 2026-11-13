//! Resident managed model processes and exclusive lifecycle operations.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::clipboard_policy::PolicyModelRuntime;

use super::super::PolicyModelAuthority;
use super::super::managed_models::{MAX_MODELS, Purpose, valid_id};
use super::process::Sidecar;
use super::{BackendUse, check_cancelled};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Family {
    Gguf,
    Mlx,
}

#[derive(Eq, Hash, PartialEq)]
struct Key {
    profile: PathBuf,
    family: Family,
    id: String,
    policy_embedding: bool,
}

struct ModelRequest<'a> {
    profile: &'a Path,
    id: &'a str,
    authority: Option<&'a PolicyModelAuthority>,
}

type Slot = Mutex<State>;

const POLL: Duration = Duration::from_millis(25);
static POOL: LazyLock<Mutex<Pool>> = LazyLock::new(|| Mutex::new(Pool::default()));
static WAKE_MONITOR: Condvar = Condvar::new();
static CLOSING: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct Pool {
    slots: HashMap<Key, Arc<Slot>>,
    maintenance: HashSet<(PathBuf, Family)>,
    children: Vec<Weak<Sidecar>>,
    monitor: Option<JoinHandle<()>>,
}

#[derive(Default)]
enum State {
    #[default]
    Stopped,
    Running(Resident),
    Failed,
}

struct Resident {
    sidecar: Arc<Sidecar>,
    leases: usize,
    idle_since: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelStatus {
    Stopped,
    Starting,
    Ready,
    Busy,
    Sleeping,
    Failed,
}

impl fmt::Display for ModelStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Ready => "Ready",
            Self::Busy => "Busy",
            Self::Sleeping => "Sleeping",
            Self::Failed => "Failed",
        })
    }
}

pub(in crate::local_ai) struct Lease {
    sidecar: Option<Arc<Sidecar>>,
    slot: Arc<Slot>,
}

impl Lease {
    pub(in crate::local_ai) fn sidecar(&self) -> &Arc<Sidecar> {
        self.sidecar.as_ref().expect("active managed model lease")
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut state = lock(&self.slot);
        let sidecar = self.sidecar.take().expect("active managed model lease");
        if let State::Running(resident) = &mut *state
            && Arc::ptr_eq(&sidecar, &resident.sidecar)
        {
            resident.leases -= 1;
            if resident.leases == 0 {
                resident.idle_since = Instant::now();
            }
        }
    }
}

pub(in crate::local_ai) fn acquire(
    profile: &Path,
    id: &str,
    authority: Option<&PolicyModelAuthority>,
    purpose: Option<Purpose>,
    backend_use: BackendUse,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<Lease> {
    acquire_for(
        ModelRequest {
            profile,
            id,
            authority,
        },
        Family::Gguf,
        purpose,
        backend_use,
        deadline,
        cancelled,
    )
}

pub(in crate::local_ai) fn acquire_mlx(
    profile: &Path,
    id: &str,
    authority: Option<&PolicyModelAuthority>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<Lease> {
    acquire_for(
        ModelRequest {
            profile,
            id,
            authority,
        },
        Family::Mlx,
        Some(Purpose::Chat),
        BackendUse::RequireMatch,
        deadline,
        cancelled,
    )
}

fn acquire_for(
    ModelRequest {
        profile,
        id,
        authority,
    }: ModelRequest<'_>,
    family: Family,
    purpose: Option<Purpose>,
    backend_use: BackendUse,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<Lease> {
    let valid = match authority {
        Some(authority) => {
            let expected = match family {
                Family::Gguf => PolicyModelRuntime::Gguf,
                Family::Mlx => PolicyModelRuntime::Mlx,
            };
            authority.definition().id == id && authority.definition().runtime == expected
        }
        None => valid_id(id),
    };
    if !valid {
        return Err(invalid_model());
    }
    let policy_embedding = authority.is_some() && purpose == Some(Purpose::Embedding);
    let slot = slot_for(profile, family, id, policy_embedding)?;
    let mut state = loop {
        check_cancelled(cancelled)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "The managed model is busy starting",
            ));
        }
        match slot.try_lock() {
            Ok(state) => break state,
            Err(TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(TryLockError::WouldBlock) => std::thread::sleep(remaining.min(POLL)),
        }
    };
    require_available(profile, family)?;

    if let State::Running(resident) = &mut *state {
        resident.sidecar.purpose().require(purpose)?;
        match resident.sidecar.validate() {
            Ok(()) => {
                resident.sidecar.require_policy_model(authority)?;
                resident.sidecar.require_backend(backend_use)?;
                resident.leases += 1;
                let sidecar = Arc::clone(&resident.sidecar);
                drop(state);
                return Ok(Lease {
                    sidecar: Some(sidecar),
                    slot,
                });
            }
            Err(error) if resident.leases > 0 => return Err(error),
            Err(_) => *state = State::Failed,
        }
    }

    let started = match family {
        Family::Gguf => Sidecar::start(
            profile,
            id,
            authority,
            purpose,
            backend_use,
            deadline,
            cancelled,
        ),
        Family::Mlx => Sidecar::start_mlx(profile, id, authority, deadline, cancelled),
    };
    let sidecar = match started {
        Ok(sidecar) => sidecar,
        Err(error) => {
            *state = if matches!(
                error.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::InvalidInput
            ) {
                State::Stopped
            } else {
                State::Failed
            };
            return Err(error);
        }
    };
    *state = State::Running(Resident {
        sidecar: Arc::clone(&sidecar),
        leases: 1,
        idle_since: Instant::now(),
    });
    drop(state);
    Ok(Lease {
        sidecar: Some(sidecar),
        slot,
    })
}

pub(crate) fn model_status(profile: &Path, id: &str) -> ModelStatus {
    status_for(profile, Family::Gguf, id)
}

pub(crate) fn mlx_model_status(profile: &Path, id: &str) -> ModelStatus {
    status_for(profile, Family::Mlx, id)
}

fn status_for(profile: &Path, family: Family, id: &str) -> ModelStatus {
    let slot = lock(&POOL)
        .slots
        .get(&Key {
            profile: profile.to_owned(),
            family,
            id: id.to_owned(),
            policy_embedding: false,
        })
        .cloned();
    let Some(slot) = slot else {
        return ModelStatus::Stopped;
    };
    let state = match slot.try_lock() {
        Ok(state) => state,
        Err(TryLockError::Poisoned(error)) => error.into_inner(),
        Err(TryLockError::WouldBlock) => return ModelStatus::Starting,
    };
    match &*state {
        State::Stopped => ModelStatus::Stopped,
        State::Failed => ModelStatus::Failed,
        State::Running(resident) if resident.leases > 0 => ModelStatus::Busy,
        State::Running(resident) => {
            if resident
                .sidecar
                .idle_timeout()
                .is_some_and(|timeout| resident.idle_since.elapsed() >= timeout)
            {
                ModelStatus::Sleeping
            } else {
                ModelStatus::Ready
            }
        }
    }
}

pub(crate) fn with_stopped_model<T>(
    profile: &Path,
    id: &str,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    with_stopped(profile, Family::Gguf, id, operation)
}

pub(crate) fn with_stopped_mlx_model<T>(
    profile: &Path,
    id: &str,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    with_stopped(profile, Family::Mlx, id, operation)
}

fn with_stopped<T>(
    profile: &Path,
    family: Family,
    id: &str,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    if !valid_id(id) {
        return Err(invalid_model());
    }
    let slot = slot_for(profile, family, id, false)?;
    let mut state = control_lock(&slot)?;
    require_available(profile, family)?;
    stop_idle(&mut state)?;
    // Keep the reservation through the registry operation so another request
    // cannot open the old configuration between stopping and saving it.
    operation()
}

pub(crate) fn stop_model(profile: &Path, id: &str) -> io::Result<()> {
    with_stopped_model(profile, id, || Ok(()))
}

fn stop_idle(state: &mut State) -> io::Result<()> {
    if let State::Running(resident) = state {
        if resident.leases > 0 {
            return Err(model_in_use());
        }
        resident.sidecar.stop()?;
    }
    *state = State::Stopped;
    Ok(())
}

pub(super) struct Maintenance((PathBuf, Family));

impl Drop for Maintenance {
    fn drop(&mut self) {
        lock(&POOL).maintenance.remove(&self.0);
    }
}

pub(super) fn require_idle_for_install(profile: &Path) -> io::Result<()> {
    let slots: Vec<_> = {
        let pool = lock(&POOL);
        require_open()?;
        pool.slots
            .iter()
            .filter(|(key, _)| key.profile == profile && key.family == Family::Gguf)
            .map(|(_, slot)| Arc::clone(slot))
            .collect()
    };
    for slot in &slots {
        let state = control_lock(slot)?;
        if matches!(&*state, State::Running(resident) if resident.leases > 0) {
            return Err(model_in_use());
        }
    }
    Ok(())
}

pub(super) fn prepare_install(profile: &Path) -> io::Result<Maintenance> {
    prepare_for_install(profile, Family::Gguf)
}

pub(crate) fn with_mlx_maintenance<T>(
    profile: &Path,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    with_mlx_maintenance_io(profile, || operation().map_err(io::Error::other))
        .map_err(|error| error.to_string())
}

pub(in crate::local_ai) fn with_mlx_maintenance_io<T>(
    profile: &Path,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let _maintenance = prepare_for_install(profile, Family::Mlx)?;
    operation()
}

fn prepare_for_install(profile: &Path, family: Family) -> io::Result<Maintenance> {
    let (maintenance, slots) = {
        let mut pool = lock(&POOL);
        require_open()?;
        let key = (profile.to_owned(), family);
        if !pool.maintenance.insert(key.clone()) {
            return Err(io::Error::other(
                "A runtime installation is already in progress",
            ));
        }
        let maintenance = Maintenance(key);
        let slots: Vec<_> = pool
            .slots
            .iter()
            .filter(|(key, _)| key.profile == profile && key.family == family)
            .map(|(_, slot)| Arc::clone(slot))
            .collect();
        (maintenance, slots)
    };

    // All slots are reserved before touching an idle process. In-flight leases
    // can finish naturally and an active model makes installation retryable.
    let mut states: Vec<_> = slots
        .iter()
        .map(|slot| control_lock(slot))
        .collect::<io::Result<_>>()?;
    if states
        .iter()
        .any(|state| matches!(&**state, State::Running(resident) if resident.leases > 0))
    {
        return Err(model_in_use());
    }
    for state in &mut states {
        stop_idle(state)?;
    }
    Ok(maintenance)
}

pub(in crate::local_ai) fn trust_changed() {
    WAKE_MONITOR.notify_all();
}

pub(super) fn register_child(
    spawn: impl FnOnce() -> io::Result<Arc<Sidecar>>,
) -> io::Result<Arc<Sidecar>> {
    let mut pool = lock(&POOL);
    require_open()?;
    if pool.monitor.is_none() {
        pool.monitor = Some(
            std::thread::Builder::new()
                .name("kortty-model-monitor".into())
                .spawn(monitor)?,
        );
    }
    pool.children.retain(|child| child.strong_count() != 0);
    // The application cannot start its final shutdown between spawning a child
    // and recording the handle needed to reap it.
    let child = spawn()?;
    pool.children.push(Arc::downgrade(&child));
    Ok(child)
}

fn monitor() {
    loop {
        let slots = {
            let pool = lock(&POOL);
            if is_closing() {
                return;
            }
            let (pool, _) = WAKE_MONITOR
                .wait_timeout(pool, Duration::from_secs(1))
                .unwrap_or_else(PoisonError::into_inner);
            if is_closing() {
                return;
            }
            pool.slots.values().cloned().collect::<Vec<_>>()
        };
        for slot in slots {
            if let Ok(mut state) = slot.try_lock()
                && let State::Running(resident) = &*state
            {
                if resident.leases == 0
                    && resident.sidecar.stops_when_idle()
                    && resident
                        .sidecar
                        .idle_timeout()
                        .is_some_and(|timeout| resident.idle_since.elapsed() >= timeout)
                {
                    *state = if resident.sidecar.stop().is_ok() {
                        State::Stopped
                    } else {
                        State::Failed
                    };
                } else if resident.sidecar.validate().is_err() && resident.leases == 0 {
                    *state = State::Failed;
                }
            }
        }
    }
}

pub(crate) fn shutdown() {
    let (children, monitor) = {
        let mut pool = lock(&POOL);
        CLOSING.store(true, Ordering::Release);
        let children: Vec<_> = pool
            .children
            .drain(..)
            .filter_map(|child| child.upgrade())
            .collect();
        pool.slots.clear();
        (children, pool.monitor.take())
    };
    WAKE_MONITOR.notify_all();
    for child in children {
        let _ = child.stop();
    }
    if let Some(monitor) = monitor {
        let _ = monitor.join();
    }
}

pub(super) fn is_closing() -> bool {
    CLOSING.load(Ordering::Acquire)
}

fn require_open() -> io::Result<()> {
    if is_closing() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Managed models are shutting down",
        ))
    } else {
        Ok(())
    }
}

fn require_available(profile: &Path, family: Family) -> io::Result<()> {
    require_open()?;
    if lock(&POOL)
        .maintenance
        .contains(&(profile.to_owned(), family))
    {
        Err(io::Error::other(match family {
            Family::Gguf => "The llama.cpp runtime is being changed",
            Family::Mlx => "The MLX runtime is being changed",
        }))
    } else {
        Ok(())
    }
}

fn slot_for(
    profile: &Path,
    family: Family,
    id: &str,
    policy_embedding: bool,
) -> io::Result<Arc<Slot>> {
    require_open()?;
    if !profile.is_absolute() {
        return Err(invalid_model());
    }
    let mut pool = lock(&POOL);
    pool.slots.retain(|_, slot| {
        Arc::strong_count(slot) > 1
            || !matches!(slot.try_lock(), Ok(state) if matches!(*state, State::Stopped))
    });
    let key = Key {
        profile: profile.to_owned(),
        family,
        id: id.to_owned(),
        policy_embedding,
    };
    if let Some(slot) = pool.slots.get(&key) {
        return Ok(Arc::clone(slot));
    }
    if pool.slots.len() >= MAX_MODELS {
        pool.slots.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || !matches!(slot.try_lock(), Ok(state) if matches!(*state, State::Failed))
        });
    }
    if pool.slots.len() >= MAX_MODELS {
        return Err(io::Error::other(
            "The managed model process limit has been reached",
        ));
    }
    let slot = Arc::new(Slot::default());
    pool.slots.insert(key, Arc::clone(&slot));
    Ok(slot)
}

fn invalid_model() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Select a registered managed model",
    )
}

fn control_lock(slot: &Slot) -> io::Result<MutexGuard<'_, State>> {
    match slot.try_lock() {
        Ok(state) => Ok(state),
        Err(TryLockError::Poisoned(error)) => Ok(error.into_inner()),
        Err(TryLockError::WouldBlock) => Err(model_in_use()),
    }
}

fn model_in_use() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "The managed model is starting or handling a request",
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
