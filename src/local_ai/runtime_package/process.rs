//! Bounded process supervision for an authenticated llama.cpp installation.

use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::Value;
use zeroize::Zeroizing;

use crate::clipboard_policy::ClipboardPolicy;

use super::super::managed_models::ModelBackend;
use super::super::runtime_index::PackageBackend;
use super::super::{
    Error, PolicyModelAuthority, Profile, ProfileOptions, RequestContent, bearer_header,
    managed_models, mlx_models, mlx_runtime, policy_models, start,
};
use super::{BackendUse, check_cancelled, install::Runtime, online::Choice, pool};

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_VERSION_OUTPUT: usize = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const STARTUP_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const MODEL_TEST_TIMEOUT: Duration = Duration::from_secs(60);

fn command(executable: &Path, payload: &Path) -> Command {
    let mut command = Command::new(executable);
    command
        .current_dir(payload)
        .env_clear()
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
        for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    command
}

fn model_options(
    command: &mut Command,
    model: &managed_models::Model,
    idle_timeout: Option<Duration>,
    cpu_fallback: bool,
) {
    command
        .arg("--ctx-size")
        .arg(model.context_tokens.to_string());
    if model.cpu_threads != 0 {
        command.arg("--threads").arg(model.cpu_threads.to_string());
    }
    let layers = if cpu_fallback || model.backend == ModelBackend::Cpu {
        0
    } else if model.gpu_layers == managed_models::AUTO_GPU_LAYERS
        && model.backend.required_package().is_some()
    {
        managed_models::MAX_GPU_LAYERS
    } else {
        model.gpu_layers
    };
    if layers >= 0 {
        command.arg("--n-gpu-layers").arg(layers.to_string());
    }
    if let Some(timeout) = idle_timeout {
        command
            .arg("--sleep-idle-seconds")
            .arg(timeout.as_secs().to_string());
    }
    if model.purpose == managed_models::Purpose::Embedding {
        command.args(["--embedding", "--pooling", "mean"]);
    }
}

#[derive(Clone, Copy)]
struct BackendMismatch {
    requested: ModelBackend,
    active: PackageBackend,
}

impl BackendMismatch {
    fn detect(requested: ModelBackend, active: PackageBackend) -> Option<Self> {
        (!requested.available(Some(active))).then_some(Self { requested, active })
    }

    fn message(self) -> String {
        if !self.requested.supported() {
            return format!(
                "The configured {} backend is unavailable on this platform.",
                self.requested
            );
        }
        format!(
            "This model requires the {} backend. The active signed runtime package is {}. Install the matching signed package or choose Auto or CPU in Configure model.",
            self.requested, self.active
        )
    }
}

fn require_backend(mismatch: Option<BackendMismatch>, backend_use: BackendUse) -> io::Result<()> {
    if let Some(mismatch) = mismatch
        && backend_use == BackendUse::RequireMatch
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            mismatch.message(),
        ));
    }
    Ok(())
}

struct OwnedChild {
    child: Child,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl OwnedChild {
    fn spawn(command: &mut Command) -> io::Result<Self> {
        #[cfg(windows)]
        if !Path::new(command.get_program())
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(io::Error::other(
                "The signed Windows runtime must name an .exe entrypoint",
            ));
        }
        let child = command.spawn()?;
        #[cfg(windows)]
        {
            let mut owned = Self { child, job: None };
            match kortty_pty::contain_child(&owned.child) {
                Ok(job) => owned.job = Some(job),
                Err(_) if matches!(owned.child.try_wait(), Ok(Some(_))) => {}
                Err(error) => {
                    owned.stop()?;
                    return Err(error);
                }
            }
            Ok(owned)
        }
        #[cfg(not(windows))]
        Ok(Self { child })
    }

    fn stop(&mut self) -> io::Result<()> {
        if !matches!(self.child.try_wait(), Ok(Some(_)))
            && let Err(error) = self.child.kill()
            && !matches!(self.child.try_wait(), Ok(Some(_)))
        {
            return Err(error);
        }
        #[cfg(windows)]
        drop(self.job.take());
        self.child.wait().map(|_| ())
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub(super) fn version(executable: &Path, payload: &Path, cancelled: &AtomicBool) -> io::Result<()> {
    check_cancelled(cancelled)?;
    let (reader, writer) = io::pipe()?;
    let stderr = writer.try_clone()?;
    let mut child = OwnedChild::spawn(
        command(executable, payload)
            .arg("--version")
            .stdout(writer)
            .stderr(stderr),
    )?;
    let (send, result) = mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("kortty-runtime-version-output".into())
        .spawn(move || {
            let _ = send.send(version_output(reader));
        })?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let mut output = None;
    let outcome = loop {
        if let Err(error) = check_cancelled(cancelled) {
            break Err(error);
        }
        if output.is_none() {
            match result.try_recv() {
                Ok(Ok(content)) => output = Some(content),
                Ok(Err(error)) => break Err(error),
                Err(mpsc::TryRecvError::Disconnected) => {
                    break Err(io::Error::other("Runtime version output reader stopped"));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        match child.child.try_wait() {
            Ok(Some(status)) if !status.success() => {
                break Err(io::Error::other(format!(
                    "llama-server --version failed ({status})"
                )));
            }
            Ok(Some(_)) => {
                if let Some(content) = output {
                    break if content {
                        Ok(())
                    } else {
                        Err(io::Error::other("llama-server --version printed nothing"))
                    };
                }
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        if Instant::now() >= deadline {
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "llama-server --version exceeded its time limit",
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    let stopped = child.stop();
    if worker.is_finished() {
        let _ = worker.join();
    }
    outcome.and(stopped)
}

fn version_output(mut reader: impl Read) -> io::Result<bool> {
    let mut buffer = [0; 4096];
    let mut bytes = 0;
    let mut content = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            return Ok(content);
        }
        bytes += count;
        if bytes > MAX_VERSION_OUTPUT {
            return Err(io::Error::other(
                "llama-server --version exceeded its output limit",
            ));
        }
        content |= buffer[..count]
            .iter()
            .any(|byte| !byte.is_ascii_whitespace());
    }
}

pub(in crate::local_ai) struct Sidecar {
    // The process is reaped before the runtime and weights leases are released.
    child: Mutex<OwnedChild>,
    assets: Assets,
    address: SocketAddrV4,
    pid: u32,
    authorization: ureq::http::HeaderValue,
    idle_timeout: Option<Duration>,
    backend_mismatch: Option<BackendMismatch>,
    policy_model: Option<PolicyModelAuthority>,
}

enum Assets {
    Gguf(Box<GgufAssets>),
    Mlx(Box<MlxAssets>),
}

struct GgufAssets {
    runtime: Mutex<Runtime>,
    model: managed_models::OpenedModel,
}

struct MlxAssets {
    runtime: Mutex<mlx_runtime::Runtime>,
    model: mlx_models::Opened,
    environment: mlx_runtime::KeyFile,
}

impl Sidecar {
    pub(super) fn start(
        profile: &Path,
        id: &str,
        authority: Option<&PolicyModelAuthority>,
        purpose: Option<managed_models::Purpose>,
        backend_use: BackendUse,
        request_deadline: Instant,
        cancelled: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
        let deadline = request_deadline.min(startup_deadline);
        check_start(deadline, cancelled)?;
        let model = if let Some(authority) = authority {
            authority.require_current(&ClipboardPolicy::load())?;
            policy_models::open_gguf(
                profile,
                authority.definition(),
                purpose.unwrap_or_default(),
                authority.identity(),
                deadline,
                cancelled,
            )?
        } else {
            managed_models::Registry::new(profile.to_owned()).open(id)?
        };
        model.model.purpose.require(purpose)?;
        let idle_minutes = model.model.idle_timeout_minutes;
        let idle_timeout =
            (idle_minutes != 0).then(|| Duration::from_secs(u64::from(idle_minutes) * 60));
        let runtime = loop {
            check_start(deadline, cancelled)?;
            match Runtime::open(profile, cancelled) {
                Ok(runtime) => break runtime,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_INTERVAL)
                }
                Err(error) => return Err(error),
            }
        };
        let backend_mismatch = BackendMismatch::detect(model.model.backend, runtime.backend());
        require_backend(backend_mismatch, backend_use)?;
        let mut random = Zeroizing::new([0_u8; 32]);
        getrandom::fill(&mut *random).map_err(io::Error::other)?;
        let token = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_slice()));
        let authorization =
            bearer_header(&token).map_err(|error| io::Error::other(error.message()))?;
        let key = runtime.key_file(profile, &token)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let SocketAddr::V4(address) = listener.local_addr()? else {
            return Err(io::Error::other("Expected an IPv4 loopback listener"));
        };
        model.revalidate()?;
        runtime.revalidate()?;
        check_start(deadline, cancelled)?;

        let mut launch = command(runtime.executable(), runtime.payload());
        model.configure_launch(&mut launch)?;
        launch
            .args(["--host", "127.0.0.1", "--port"])
            .arg(address.port().to_string())
            .arg("--alias")
            .arg(&model.model.id)
            .arg("--api-key-file")
            .arg(&key.path)
            .args([
                "--parallel",
                "1",
                "--offline",
                "--no-ui",
                "--no-agent",
                "--no-ui-mcp-proxy",
                "--no-slots",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        model_options(
            &mut launch,
            &model.model,
            idle_timeout.filter(|_| !model.stops_when_idle()),
            backend_mismatch.is_some(),
        );
        // Only a failed process launch needs recovery, after the pool lock is released.
        let mut failed_runtime = None;
        let sidecar = pool::register_child(|| {
            if let Some(authority) = authority {
                authority.require_current(&ClipboardPolicy::load())?;
            }
            check_start(deadline, cancelled)?;
            drop(listener);
            let child = match OwnedChild::spawn(&mut launch) {
                Ok(child) => child,
                Err(error) => {
                    failed_runtime = Some(runtime);
                    return Err(error);
                }
            };
            let pid = child.child.id();
            Ok(Arc::new(Self {
                child: Mutex::new(child),
                assets: Assets::Gguf(Box::new(GgufAssets {
                    runtime: Mutex::new(runtime),
                    model,
                })),
                address,
                pid,
                authorization,
                idle_timeout,
                backend_mismatch,
                policy_model: authority.cloned(),
            }))
        })
        .map_err(|error| match failed_runtime {
            Some(runtime) => startup_failure(&runtime, profile, cancelled, error),
            None => error,
        })?;
        let Assets::Gguf(assets) = &sidecar.assets else {
            unreachable!("started GGUF sidecar")
        };
        if let Err(error) = sidecar.wait_ready(id, deadline, cancelled) {
            sidecar.stop()?;
            check_cancelled(cancelled)?;
            if sidecar.requires_model_revalidation() {
                assets.model.revalidate()?;
            }
            if error.kind() == io::ErrorKind::TimedOut && request_deadline < startup_deadline {
                // A shorter request budget does not establish a runtime failure.
                return Err(error);
            }
            let runtime = assets
                .runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            return Err(startup_failure(&runtime, profile, cancelled, error));
        }
        check_start(deadline, cancelled)?;
        if sidecar.requires_model_revalidation()
            && let Err(error) = assets.model.revalidate()
        {
            sidecar.stop()?;
            return Err(error);
        }
        assets
            .runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ready()?;
        check_start(deadline, cancelled)?;
        // The server has read the key. Remove its on-disk copy while the process is live.
        drop(key);
        if let Some(mismatch) = backend_mismatch {
            eprintln!(
                "The managed model uses CPU settings for profile requests. Configured backend: {}. Active signed package: {}.",
                mismatch.requested, mismatch.active,
            );
        }
        Ok(sidecar)
    }

    pub(super) fn start_mlx(
        profile: &Path,
        id: &str,
        authority: Option<&PolicyModelAuthority>,
        request_deadline: Instant,
        cancelled: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        if !mlx_runtime::supported() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Managed MLX requires Apple Silicon and macOS 14 or later",
            ));
        }
        let startup_deadline = Instant::now() + STARTUP_TIMEOUT;
        let deadline = startup_deadline.min(request_deadline);
        check_start(deadline, cancelled)?;
        let model = if let Some(authority) = authority {
            authority.require_current(&ClipboardPolicy::load())?;
            policy_models::open_mlx(authority.definition(), authority.identity(), cancelled)?
        } else {
            mlx_models::Registry::new(profile).open(id)?
        };
        let idle_minutes = model.model.idle_minutes;
        let idle_timeout =
            (idle_minutes != 0).then(|| Duration::from_secs(u64::from(idle_minutes) * 60));
        let runtime = mlx_runtime::Runtime::open(profile, cancelled)?;
        let mut random = Zeroizing::new([0_u8; 32]);
        getrandom::fill(&mut *random).map_err(io::Error::other)?;
        let token = Zeroizing::new(URL_SAFE_NO_PAD.encode(random.as_slice()));
        let authorization =
            bearer_header(&token).map_err(|error| io::Error::other(error.message()))?;
        let key = runtime.key_file(&token)?;
        let home = key
            .path()
            .parent()
            .ok_or_else(|| io::Error::other("Missing private MLX session directory"))?;
        let model_path = model.path.canonicalize()?;
        let model_id = model_path
            .to_str()
            .ok_or_else(|| io::Error::other("The MLX model path must be Unicode"))?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let SocketAddr::V4(address) = listener.local_addr()? else {
            return Err(io::Error::other("Expected an IPv4 loopback listener"));
        };
        model.revalidate()?;
        runtime.revalidate()?;
        let mut launch = command(runtime.executable(), runtime.payload());
        launch
            .args(["-I", "-B"])
            .arg(runtime.launcher())
            .arg("--model")
            .arg(&model_path)
            .args(["--host", "127.0.0.1", "--port"])
            .arg(address.port().to_string())
            .arg("--api-key-file")
            .arg(key.path())
            .args(["--max-idle-seconds", "0"])
            .env("HOME", home)
            .env("HF_HOME", home.join("huggingface"))
            .env("HF_HUB_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut runtime = Some(runtime);
        let sidecar = pool::register_child(|| {
            if let Some(authority) = authority {
                authority.require_current(&ClipboardPolicy::load())?;
            }
            check_start(deadline, cancelled)?;
            drop(listener);
            let child = OwnedChild::spawn(&mut launch)?;
            let pid = child.child.id();
            Ok(Arc::new(Self {
                child: Mutex::new(child),
                assets: Assets::Mlx(Box::new(MlxAssets {
                    runtime: Mutex::new(runtime.take().expect("prepared MLX runtime")),
                    model,
                    environment: key,
                })),
                address,
                pid,
                authorization,
                idle_timeout,
                backend_mismatch: None,
                policy_model: authority.cloned(),
            }))
        })
        .map_err(|error| match runtime.as_mut() {
            Some(runtime) => mlx_startup_failure(runtime, cancelled, error),
            None => error,
        })?;
        let Assets::Mlx(assets) = &sidecar.assets else {
            unreachable!("started MLX sidecar")
        };
        if let Err(error) = sidecar.wait_ready(model_id, deadline, cancelled) {
            sidecar.stop()?;
            check_cancelled(cancelled)?;
            assets.model.revalidate()?;
            if error.kind() == io::ErrorKind::TimedOut && request_deadline < startup_deadline {
                return Err(error);
            }
            let mut runtime = assets
                .runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            return Err(mlx_startup_failure(&mut runtime, cancelled, error));
        }
        assets.environment.clear_token()?;
        assets
            .runtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .ready()?;
        check_start(deadline, cancelled)?;
        Ok(sidecar)
    }

    pub(super) fn require_backend(&self, backend_use: BackendUse) -> io::Result<()> {
        require_backend(self.backend_mismatch, backend_use)
    }

    fn require_runtime(&self, expected: &Choice) -> Result<(), String> {
        if let Assets::Gguf(assets) = &self.assets
            && assets
                .runtime
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .choice()
                == expected
        {
            Ok(())
        } else {
            Err("The llama.cpp runtime changed before the model test. Review it again.".into())
        }
    }

    pub(super) fn require_policy_model(
        &self,
        expected: Option<&PolicyModelAuthority>,
    ) -> io::Result<()> {
        if self.policy_model.as_ref() == expected {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The policy-provided model changed before the request. Review it again.",
            ))
        }
    }

    pub(in crate::local_ai) fn address(&self) -> SocketAddrV4 {
        self.address
    }

    pub(super) fn pid(&self) -> u32 {
        self.pid
    }

    pub(super) fn idle_timeout(&self) -> Option<Duration> {
        self.idle_timeout
    }

    pub(super) fn purpose(&self) -> managed_models::Purpose {
        match &self.assets {
            Assets::Gguf(assets) => assets.model.model.purpose,
            Assets::Mlx(_) => managed_models::Purpose::Chat,
        }
    }

    pub(super) fn stops_when_idle(&self) -> bool {
        match &self.assets {
            Assets::Gguf(assets) => assets.model.stops_when_idle(),
            Assets::Mlx(_) => true,
        }
    }

    pub(in crate::local_ai) fn authorization(&self) -> &ureq::http::HeaderValue {
        &self.authorization
    }

    pub(in crate::local_ai) fn agent(self: &Arc<Self>, timeout: Duration) -> ureq::Agent {
        super::loopback::agent(self, timeout)
    }

    pub(in crate::local_ai) fn validate(&self) -> io::Result<()> {
        let trust = self.revalidate_assets();
        if let Err(error) = trust {
            self.stop()?;
            return Err(error);
        }
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        match child.child.try_wait()? {
            None => Ok(()),
            Some(status) => Err(io::Error::other(format!(
                "The managed model process exited ({status})"
            ))),
        }
    }

    fn requires_model_revalidation(&self) -> bool {
        matches!(&self.assets, Assets::Gguf(assets) if self.policy_model.is_some() || assets.model.model.external.is_some() || self.has_vision())
    }

    fn revalidate_assets(&self) -> io::Result<()> {
        if let Some(authority) = &self.policy_model {
            authority.require_current(&ClipboardPolicy::load())?;
        }
        match &self.assets {
            Assets::Gguf(assets) => {
                if self.requires_model_revalidation() {
                    assets.model.revalidate()?;
                }
                assets
                    .runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .revalidate()
            }
            Assets::Mlx(assets) => {
                assets.model.revalidate()?;
                assets
                    .runtime
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .revalidate()
            }
        }
    }

    pub(in crate::local_ai) fn has_vision(&self) -> bool {
        matches!(&self.assets, Assets::Gguf(assets) if assets.model.model.projector_bytes.is_some())
    }

    pub(super) fn stop(&self) -> io::Result<()> {
        self.child
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop()
    }

    fn wait_ready(
        self: &Arc<Self>,
        model: &str,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> io::Result<()> {
        let agent = self.agent(PROBE_TIMEOUT);
        let models_url = format!("http://{}/v1/models", self.address);
        let health_url = format!("http://{}/health", self.address);
        let probe = |url: &str, authenticated: bool| {
            check_start(deadline, cancelled)?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            let mut request = agent.get(url);
            if authenticated {
                request = request.header("authorization", self.authorization.clone());
            }
            Ok::<_, io::Error>(
                request
                    .config()
                    .timeout_global(Some(remaining.min(PROBE_TIMEOUT)))
                    .build()
                    .call(),
            )
        };
        loop {
            check_start(deadline, cancelled)?;
            self.validate()?;
            if kortty_pty::owns_loopback_listener(self.pid, self.address)?
                && matches!(
                    probe(&models_url, false)?,
                    Err(ureq::Error::StatusCode(401 | 403))
                )
                && probe(&health_url, true)?.is_ok()
                && let Ok(mut response) = probe(&models_url, true)?
                && let Ok(bytes) = response
                    .body_mut()
                    .with_config()
                    .limit(64 * 1024)
                    .read_to_vec()
                && let Ok(models) = serde_json::from_slice::<Value>(&bytes)
                && models
                    .get("data")
                    .and_then(Value::as_array)
                    .is_some_and(|entries| {
                        entries
                            .iter()
                            .any(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
                    })
            {
                check_start(deadline, cancelled)?;
                return self.validate();
            }
            check_start(deadline, cancelled)?;
            std::thread::sleep(
                Duration::from_millis(150).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
}

fn check_start(deadline: Instant, cancelled: &AtomicBool) -> io::Result<()> {
    check_cancelled(cancelled)?;
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "The managed model did not become ready",
        ));
    }
    Ok(())
}

fn startup_failure(
    runtime: &Runtime,
    profile: &Path,
    cancelled: &AtomicBool,
    error: io::Error,
) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted || check_cancelled(cancelled).is_err() {
        return io::Error::new(
            io::ErrorKind::Interrupted,
            "Managed model startup cancelled",
        );
    }
    let note = match runtime.rollback(profile, cancelled) {
        Ok(note) => note.into(),
        Err(error) if error.kind() == io::ErrorKind::Interrupted => return error,
        Err(error) => format!(" Runtime recovery failed ({error})."),
    };
    io::Error::new(error.kind(), format!("{error}.{note}"))
}

fn mlx_startup_failure(
    runtime: &mut mlx_runtime::Runtime,
    cancelled: &AtomicBool,
    error: io::Error,
) -> io::Error {
    if error.kind() == io::ErrorKind::Interrupted || check_cancelled(cancelled).is_err() {
        return io::Error::new(io::ErrorKind::Interrupted, "Managed MLX startup cancelled");
    }
    match runtime.recover(cancelled) {
        Ok(false) => error,
        Ok(true) => io::Error::new(
            error.kind(),
            format!("{error}. Restored the previous verified MLX runtime."),
        ),
        Err(recovery) => io::Error::new(
            error.kind(),
            format!("{error}. MLX runtime recovery failed ({recovery})."),
        ),
    }
}

pub(super) fn test_model(
    profile_dir: &Path,
    id: &str,
    runtime: Option<Choice>,
    cancelled: &AtomicBool,
) -> Result<String, String> {
    let models = managed_models::Registry::new(profile_dir.to_owned())
        .list()
        .map_err(|error| error.to_string())?;
    let model = models
        .into_iter()
        .find(|model| model.id == id)
        .ok_or("The managed model was removed")?;
    let purpose = model.purpose;
    let mut profile = Profile::managed_for(
        profile_dir.to_owned(),
        id,
        purpose,
        ProfileOptions::default(),
    )?
    .requiring_managed_backend();
    profile.options.max_output_tokens = Some(24);
    profile.options.timeout = Some(MODEL_TEST_TIMEOUT);
    let pending = start(profile, move |client| {
        let deadline = Instant::now() + MODEL_TEST_TIMEOUT;
        if let Some(expected) = &runtime {
            let transport = client.transport(Some(deadline)).map_err(Error::message)?;
            let lease = transport
                .lease
                .as_ref()
                .ok_or("The tested runtime is unavailable.")?;
            lease.sidecar().require_runtime(expected)?;
        }
        if purpose == managed_models::Purpose::Embedding {
            return client
                .embed(&["This is a local embedding connectivity test."])
                .map(|_| ());
        }
        client
            .complete_request(
                "Reply briefly.",
                RequestContent::Text("This is a local model connectivity test. Reply OK."),
                false,
                Some(deadline),
                false,
            )
            .map_err(Error::message)
            .and_then(|reply| {
                if reply.trim().is_empty() {
                    Err("The managed model returned an empty reply".into())
                } else {
                    Ok(())
                }
            })
    })?;
    loop {
        check_cancelled(cancelled).map_err(|_| "Managed model test cancelled.")?;
        match pending.reply.try_recv() {
            Ok(result) => return result.map(|()| model.name),
            Err(async_channel::TryRecvError::Closed) => {
                return Err("The managed model worker stopped".into());
            }
            Err(async_channel::TryRecvError::Empty) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_model_launch_arguments_include_only_explicit_cpu_threads_and_gpu_layers() {
        let mut model: managed_models::Model = serde_json::from_value(serde_json::json!({
            "id": "0123456789abcdef0123456789abcdef",
            "name": "Fixture model",
            "bytes": 1024,
            "context_tokens": 4096
        }))
        .unwrap();
        let args = |model: &managed_models::Model, idle, cpu_fallback| {
            let mut command = Command::new("llama-server");
            model_options(&mut command, model, idle, cpu_fallback);
            command
                .get_args()
                .map(|value| value.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(args(&model, None, false), ["--ctx-size", "4096"]);

        model.gpu_layers = 0;
        assert_eq!(
            args(&model, None, false),
            ["--ctx-size", "4096", "--n-gpu-layers", "0"]
        );

        model.cpu_threads = 1024;
        model.gpu_layers = 10_000;
        model.purpose = managed_models::Purpose::Embedding;
        assert_eq!(
            args(&model, Some(Duration::from_secs(60)), false),
            [
                "--ctx-size",
                "4096",
                "--threads",
                "1024",
                "--n-gpu-layers",
                "10000",
                "--sleep-idle-seconds",
                "60",
                "--embedding",
                "--pooling",
                "mean",
            ]
        );
    }

    #[test]
    fn managed_backend_mismatch_refuses_manual_launch_and_forces_safe_profile_fallback() {
        let mut model: managed_models::Model = serde_json::from_value(serde_json::json!({
            "id": "0123456789abcdef0123456789abcdef",
            "name": "Fixture model",
            "bytes": 1024,
            "context_tokens": 4096
        }))
        .unwrap();
        let args = |model: &managed_models::Model, cpu_fallback| {
            let mut command = Command::new("llama-server");
            model_options(&mut command, model, None, cpu_fallback);
            command
                .get_args()
                .map(|value| value.to_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(model.backend, ModelBackend::Auto);
        model.backend = ModelBackend::Cpu;
        model.gpu_layers = 23;
        assert!(BackendMismatch::detect(model.backend, PackageBackend::Vulkan).is_none());
        assert_eq!(
            args(&model, false),
            ["--ctx-size", "4096", "--n-gpu-layers", "0"]
        );

        model.backend = if cfg!(target_os = "macos") {
            ModelBackend::Metal
        } else {
            ModelBackend::Vulkan
        };
        let package = model.backend.required_package().unwrap();
        if model.backend.supported() {
            assert!(BackendMismatch::detect(model.backend, package).is_none());
        }
        let mismatch = BackendMismatch::detect(model.backend, PackageBackend::Cpu);
        assert!(mismatch.is_some());
        assert_eq!(
            require_backend(mismatch, BackendUse::RequireMatch)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(require_backend(mismatch, BackendUse::AllowCpuFallback).is_ok());
        assert_eq!(
            args(&model, true),
            ["--ctx-size", "4096", "--n-gpu-layers", "0"]
        );
        assert_eq!(
            args(&model, false),
            ["--ctx-size", "4096", "--n-gpu-layers", "23"]
        );
        model.gpu_layers = managed_models::AUTO_GPU_LAYERS;
        assert_eq!(
            args(&model, false),
            ["--ctx-size", "4096", "--n-gpu-layers", "10000"]
        );
    }

    #[test]
    fn version_probe_distinguishes_empty_and_excessive_output() {
        assert!(!version_output(b" \t\r\n".as_slice()).unwrap());
        assert!(version_output(b"version 12040\n".as_slice()).unwrap());
        let excessive = io::repeat(b'x').take(MAX_VERSION_OUTPUT as u64 + 1);
        assert!(
            version_output(excessive)
                .unwrap_err()
                .to_string()
                .contains("output limit")
        );
    }
}
