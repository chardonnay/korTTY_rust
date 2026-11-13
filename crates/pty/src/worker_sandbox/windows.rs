//! Launch a network-isolated LPAC with private control and ciphertext pipes.
use std::ffi::{OsStr, OsString};
use std::io::{self, PipeReader, PipeWriter};
use std::marker::PhantomData;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

use mio::windows::NamedPipe;
use windows_sys::Win32::Foundation::{
    DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{PSID, SECURITY_CAPABILITIES};
use windows_sys::Win32::System::JobObjects::{
    CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, GetProcessId, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY, PROC_THREAD_ATTRIBUTE_PARENT_PROCESS,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::WindowsRelayIo;

mod cli;
mod pipe;
mod policy;

pub use cli::WindowsCli;
pub(crate) use policy::with_private_pipe;

const REAP_TIMEOUT_MS: u32 = 5000;
const ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 1;
const WIN32K_SYSTEM_CALL_DISABLE: u64 = 1 << 28;

/// Windows sandbox process and its private control and overlapped ciphertext pipes.
pub struct WindowsWorker {
    pub process: WindowsWorkerProcess,
    pub input: PipeWriter,
    pub output: PipeReader,
    /// The desktop side of a connected overlapped duplex named pipe.
    pub ciphertext: WindowsRelayIo,
}

/// The only owner of a worker's native Job, lifetime pipe and AppContainer profile.
pub struct WindowsWorkerProcess {
    process: OwnedHandle,
    job: OwnedHandle,
    _lifetime: PipeWriter,
    _profile: policy::Profile,
}

impl WindowsWorker {
    /// Creates a confined worker with no network transport of its own.
    /// Wait for its readiness before dialing or sending login information.
    pub fn prepare(executable: &Path, argument: &str) -> io::Result<Self> {
        if !executable.is_absolute()
            || executable
                .as_os_str()
                .encode_wide()
                .any(|unit| matches!(unit, 0 | 34))
            || !argument.strip_prefix("--").is_some_and(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Windows SSH worker executable or argument",
            ));
        }
        let environment = policy::Environment::read()?;
        let (profile, sid) = policy::Profile::create()?;
        let security = SECURITY_CAPABILITIES {
            AppContainerSid: sid.raw(),
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let (input, child_input) = pipe::pair(pipe::DesktopEnd::Send)?;
        let (output, child_output) = pipe::pair(pipe::DesktopEnd::Receive)?;
        let (lifetime, child_lifetime) = pipe::pair(pipe::DesktopEnd::Send)?;
        let (ciphertext, child_ciphertext) = pipe::pair(pipe::DesktopEnd::Duplex)?;

        // Inheritable handles live only in a suspended helper's handle table. A
        // concurrent ordinary CreateProcess in the desktop cannot inherit them.
        let helper_job = job(JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK)?;
        let helper_jobs = [helper_job.as_raw_handle()];
        let mut attributes = Attributes::new(1)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &helper_jobs)?;
        let helper = launch(executable, None, &environment, None, attributes)?;
        let parent = helper.process.as_raw_handle();
        let mut pipes = [null_mut(); 4];
        for (source, target) in [
            child_input.as_raw_handle(),
            child_output.as_raw_handle(),
            child_lifetime.as_raw_handle(),
            child_ciphertext.as_raw_handle(),
        ]
        .into_iter()
        .zip(&mut pipes)
        {
            // SAFETY: both process handles and source pipes are live. The helper
            // never runs and its handles close when the helper Job is closed.
            bool_result(unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    source,
                    parent,
                    target,
                    0,
                    1,
                    DUPLICATE_SAME_ACCESS,
                )
            })?;
        }

        let job = job(JOB_OBJECT_LIMIT_ACTIVE_PROCESS)?;
        let jobs = [job.as_raw_handle()];
        let mut attributes = Attributes::new(6)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_PARENT_PROCESS, &parent)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &pipes)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &security)?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
            &ALL_APPLICATION_PACKAGES_OPT_OUT,
        )?;
        attributes.set(
            PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY,
            &WIN32K_SYSTEM_CALL_DISABLE,
        )?;
        let mut command = OsString::from("\"");
        command.push(executable);
        // Inheritance preserves the numeric handle value. Windows documents
        // that the lower 32 bits suffice for its cross-process representation.
        let descriptor = u32::try_from(pipes[3] as usize).map_err(|_| {
            io::Error::other("SSH pipe handle exceeds the startup descriptor range")
        })?;
        command.push(format!("\" {argument} {descriptor}"));
        let mut command = wide(&command);
        let child = launch(
            executable,
            Some(&mut command),
            &environment,
            Some([pipes[0], pipes[1], pipes[2]]),
            attributes,
        )?;
        let worker = WindowsWorkerProcess {
            process: child.process,
            job,
            _lifetime: lifetime.into(),
            _profile: profile,
        };
        drop((
            child_input,
            child_output,
            child_lifetime,
            child_ciphertext,
            helper_job,
        ));
        wait(&helper.process, REAP_TIMEOUT_MS)?;
        worker.verify(sid.raw())?;

        // SAFETY: child.thread is the primary thread of our verified suspended child.
        if unsafe { ResumeThread(child.thread.as_raw_handle()) } != 1 {
            return Err(io::Error::other("could not resume the confined SSH worker"));
        }
        Ok(Self {
            process: worker,
            input: input.into(),
            output: output.into(),
            ciphertext: WindowsRelayIo::pipe(ciphertext),
        })
    }
}

impl WindowsWorkerProcess {
    /// Returns the PID attached to the worker's owned process handle.
    pub fn id(&self) -> u32 {
        // SAFETY: the process handle remains live for this call.
        unsafe { GetProcessId(self.process.as_raw_handle()) }
    }

    /// Measures the confined worker using its owned process handle.
    pub fn usage(&self) -> crate::ProcessUsage {
        if matches!(self.exited(), Ok(false)) {
            crate::process_usage::handle_usage(self.process.as_raw_handle())
        } else {
            crate::ProcessUsage::default()
        }
    }

    fn verify(&self, sid: PSID) -> io::Result<()> {
        let mut member = 0;
        // SAFETY: both handles are live and member receives the kernel's result.
        bool_result(unsafe {
            IsProcessInJob(
                self.process.as_raw_handle(),
                self.job.as_raw_handle(),
                &mut member,
            )
        })?;
        if member == 0 {
            return Err(io::Error::other("SSH worker is outside its private Job"));
        }
        verify_job(self.job.as_raw_handle())?;
        policy::verify(self.process.as_raw_handle(), Some(sid))
    }

    /// Returns whether the child has exited.
    pub fn exited(&self) -> io::Result<bool> {
        signaled(&self.process, 0)
    }

    /// Terminates the Job and the worker within it.
    pub fn kill(&self) -> io::Result<()> {
        // SAFETY: the handle owns our private Job.
        bool_result(unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) })
    }

    /// Waits for termination with a bounded cleanup interval.
    pub fn wait(&self) -> io::Result<u32> {
        wait(&self.process, REAP_TIMEOUT_MS)?;
        let mut code = 0;
        // SAFETY: the owned process handle is live, and code is writable.
        bool_result(unsafe { GetExitCodeProcess(self.process.as_raw_handle(), &mut code) })?;
        Ok(code)
    }
}

impl Drop for WindowsWorkerProcess {
    fn drop(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }
}

impl WindowsWorker {
    /// Verifies the process and takes an owned handle to its inherited overlapped pipe.
    /// The worker may acknowledge startup only after this succeeds.
    pub fn enter(descriptor: u32) -> io::Result<NamedPipe> {
        // SAFETY: GetCurrentProcess returns the current process pseudo-handle.
        let current = unsafe { GetCurrentProcess() };
        policy::verify(current, None)?;
        verify_job(null_mut())?;
        pipe::receive(descriptor)
    }
}

pub(super) fn check_available() -> io::Result<()> {
    policy::Environment::read().map(|_| ())
}

fn verify_job(job: HANDLE) -> io::Result<()> {
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: job is a live Job handle, or NULL to query this process's job.
    // The fixed-size limits record is writable and matches the information class.
    bool_result(unsafe {
        QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::from_mut(&mut limits).cast(),
            size_of_val(&limits) as u32,
            null_mut(),
        )
    })?;
    let limits = limits.BasicLimitInformation;
    let required = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
    let breakaway = windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_BREAKAWAY_OK
        | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
    if limits.LimitFlags & required != required
        || limits.LimitFlags & breakaway != 0
        || limits.ActiveProcessLimit != 1
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "SSH worker Job permits process creation",
        ));
    }
    Ok(())
}

struct Suspended {
    process: OwnedHandle,
    thread: OwnedHandle,
}

fn launch(
    executable: &Path,
    command: Option<&mut [u16]>,
    environment: &policy::Environment,
    pipes: Option<[HANDLE; 3]>,
    attributes: Attributes<'_>,
) -> io::Result<Suspended> {
    launch_in(
        executable,
        command,
        &environment.block,
        &environment.system_root,
        pipes,
        attributes,
    )
}

fn launch_in(
    executable: &Path,
    command: Option<&mut [u16]>,
    environment: &[u16],
    directory: &[u16],
    pipes: Option<[HANDLE; 3]>,
    mut attributes: Attributes<'_>,
) -> io::Result<Suspended> {
    let path = wide(executable.as_os_str());
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    if let Some([input, output, lifetime]) = pipes {
        startup.StartupInfo.hStdInput = input;
        startup.StartupInfo.hStdOutput = output;
        startup.StartupInfo.hStdError = lifetime;
    }
    startup.lpAttributeList = attributes.as_mut_ptr();
    let mut result = PROCESS_INFORMATION::default();
    // SAFETY: the path, optional mutable command line and environment are valid
    // Windows strings; every attribute value outlives this call.
    bool_result(unsafe {
        CreateProcessW(
            path.as_ptr(),
            command.map_or(null_mut(), <[u16]>::as_mut_ptr),
            null(),
            null(),
            i32::from(pipes.is_some()),
            CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT,
            environment.as_ptr().cast(),
            directory.as_ptr(),
            &startup.StartupInfo,
            &mut result,
        )
    })?;
    // SAFETY: CreateProcessW succeeded and transferred both handles.
    Ok(unsafe {
        Suspended {
            process: OwnedHandle::from_raw_handle(result.hProcess),
            thread: OwnedHandle::from_raw_handle(result.hThread),
        }
    })
}

struct Attributes<'values>(Vec<usize>, PhantomData<&'values ()>);

impl<'values> Attributes<'values> {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        // SAFETY: a null list requests the allocation size.
        unsafe { InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
        // SAFETY: the aligned buffer has the requested capacity.
        bool_result(unsafe {
            InitializeProcThreadAttributeList(storage.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        Ok(Self(storage, PhantomData))
    }

    fn set<T>(&mut self, attribute: u32, value: &'values T) -> io::Result<()> {
        // SAFETY: callers use the exact Windows type and keep each value live
        // through the CreateProcessW call which consumes the attribute list.
        bool_result(unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                attribute as usize,
                std::ptr::from_ref(value).cast(),
                size_of::<T>(),
                null_mut(),
                null_mut(),
            )
        })
    }

    fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for Attributes<'_> {
    fn drop(&mut self) {
        // SAFETY: this guard exclusively owns the initialized attribute list.
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}

fn job(flags: u32) -> io::Result<OwnedHandle> {
    // SAFETY: null pointers create a new unnamed, non-inheritable Job.
    let handle = unsafe { CreateJobObjectW(null(), null()) };
    if handle.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateJobObjectW returned an owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = flags | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    // SAFETY: the initialized structure is the documented type for this class.
    bool_result(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            size_of_val(&limits) as u32,
        )
    })?;
    Ok(job)
}

fn signaled(process: &OwnedHandle, timeout_ms: u32) -> io::Result<bool> {
    // SAFETY: process is a live process handle.
    match unsafe { WaitForSingleObject(process.as_raw_handle(), timeout_ms) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(io::Error::last_os_error()),
    }
}

fn wait(process: &OwnedHandle, timeout_ms: u32) -> io::Result<()> {
    if !signaled(process, timeout_ms)? {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SSH worker did not exit",
        ));
    }
    Ok(())
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain([0]).collect()
}

fn bool_result(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
