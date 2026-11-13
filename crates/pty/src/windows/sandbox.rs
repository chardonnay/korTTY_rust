//! Local terminal confinement using Windows Process Security Environments.

use std::env;
use std::ffi::{CStr, OsStr, OsString, c_void};
use std::fs;
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::null_mut;

use kortty_files::tree::Directory;
use windows_sys::Win32::Foundation::{FARPROC, FreeLibrary, HANDLE, HMODULE, LocalFree};
use windows_sys::Win32::Globalization::{CSTR_EQUAL, CompareStringOrdinal};
use windows_sys::Win32::Security::{
    CreateWellKnownSid, DeriveCapabilitySidsFromName, EqualSid, GetTokenInformation, PSID,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_QUERY, TokenCapabilities, TokenIsAppContainer,
    WinCapabilityInternetClientSid,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::Win32::System::SystemServices::PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY;
use windows_sys::Win32::System::Threading::{
    GetProcessMitigationPolicy, OpenProcessToken, ProcessSystemCallDisablePolicy,
};

use super::{bool_result, owned_handle};

#[path = "sandbox/spec.rs"]
mod spec;

pub(super) const BACKEND: &str = "windows-psec";
pub(super) const PROCESS_ATTRIBUTE: usize = 35 | 0x0002_0000;

pub(super) struct Session {
    environment: Environment,
    scratch: PathBuf,
    workspace: PathBuf,
    _workspace_anchors: Vec<Directory>,
}

impl Session {
    pub(super) fn new(workspace: &Path, profile: &Path, scratch: &Path) -> io::Result<Self> {
        check_local_path(workspace)?;
        let mut anchors = Vec::new();
        for parent in workspace.ancestors().filter(|p| p.parent().is_some()) {
            anchors.push(Directory::open_reviewed_removal_parent(parent)?);
        }
        let workspace = canonical(workspace)?;
        let profile = canonical_or_missing(profile)?;
        let home = env::var_os("USERPROFILE")
            .filter(|path| !path.is_empty())
            .ok_or_else(|| io::Error::other("The Windows user profile could not be determined"))?;
        let home = canonical(Path::new(&home))?;
        let mut protected = vec![profile, installation_directory(&env::current_exe()?)?];
        for name in [".ssh", ".gnupg"] {
            protected.push(canonical_or_missing(&home.join(name))?);
        }
        for (name, default) in [
            ("APPDATA", r"AppData\Roaming"),
            ("LOCALAPPDATA", r"AppData\Local"),
        ] {
            let root = env::var_os(name).map_or_else(|| home.join(default), PathBuf::from);
            protected.push(canonical_or_missing(&root.join(r"Microsoft\Credentials"))?);
            protected.push(canonical_or_missing(&root.join(r"Microsoft\Vault"))?);
        }
        if contains(&workspace, &home) || protected.iter().any(|path| overlaps(&workspace, path)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The sandbox workspace overlaps the user profile or a protected directory",
            ));
        }
        let cmd = super::shell_program(crate::LocalShell::CommandPrompt)?;
        let windows =
            canonical(cmd.parent().and_then(Path::parent).ok_or_else(|| {
                io::Error::other("The Windows directory could not be determined")
            })?)?;
        if overlaps(&workspace, &windows) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The sandbox workspace overlaps the Windows system directory",
            ));
        }

        let scratch = canonical(scratch)?;
        if protected.iter().any(|path| overlaps(&scratch, path)) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The Windows temporary directory overlaps a protected directory",
            ));
        }
        protected.extend([home, scratch.join("outside")]);
        let deny = protected
            .iter()
            .map(|path| policy_path(path))
            .collect::<io::Result<Vec<_>>>()?;
        let write = [policy_path(&workspace)?, policy_path(&scratch)?];
        let environment = Environment::new(&spec::encode(&write, &deny))?;
        Ok(Self {
            environment,
            scratch,
            workspace,
            _workspace_anchors: anchors,
        })
    }

    pub(super) fn handle(&self) -> HANDLE {
        self.environment.handle()
    }

    pub(super) fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub(super) fn environment(
        &self,
        probe: bool,
    ) -> io::Result<Vec<(&'static str, Option<OsString>)>> {
        let mut environment = system_environment()?;
        environment.push(("KORTTY_SANDBOX", Some(BACKEND.into())));
        let writable = self.scratch.join("session").into_os_string();
        for key in [
            "APPDATA",
            "HOME",
            "LOCALAPPDATA",
            "TEMP",
            "TMP",
            "USERPROFILE",
        ] {
            environment.push((key, Some(writable.clone())));
        }
        if probe {
            environment.push((
                "KORTTY_PROBE_DENIED",
                Some(self.scratch.join("outside").into_os_string()),
            ));
            environment.push(("KORTTY_PROBE_TEMP", Some(writable)));
            environment.push((
                "KORTTY_PROBE_WORKSPACE",
                Some(self.workspace.as_os_str().to_owned()),
            ));
        }
        Ok(environment)
    }

    pub(super) fn verify_probe(&self) -> io::Result<()> {
        let writable = self.scratch.join("session");
        if fs::read(writable.join("probe"))? != b"k\0o\0r\0t\0t\0y\0\r\0\n\0"
            || fs::read(self.scratch.join("outside"))? != b"outside"
            || !fs::read(writable.join("blocked"))?.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Windows sandbox filesystem probe failed",
            ));
        }
        let marker = writable.join("native-temp");
        let bytes = fs::read(&marker)?;
        let (units, tail) = bytes.as_chunks::<2>();
        let units: Vec<_> = units.iter().copied().map(u16::from_le_bytes).collect();
        let native_temp = String::from_utf16(&units).map_err(io::Error::other)?;
        let invalid_temp =
            || io::Error::other("The Windows sandbox did not report its temporary directories");
        let mut lines = native_temp.lines();
        let mut paths = Vec::with_capacity(2);
        for expected in ["TEMP", "TMP"] {
            let (name, path) = lines
                .next()
                .and_then(|line| line.split_once('='))
                .filter(|(name, _)| name.eq_ignore_ascii_case(expected))
                .ok_or_else(invalid_temp)?;
            let path = Path::new(path);
            check_local_path(path)?;
            if !contains(&writable, path) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("The Windows sandbox {name} directory is outside the private session"),
                ));
            }
            paths.push(path);
        }
        if !tail.is_empty() || lines.next().is_some() {
            return Err(invalid_temp());
        }
        // Windows may name a container TEMP directory without creating it.
        let scratch = Directory::open_reviewed_removal_parent(&writable)?;
        let prefix_length = writable.components().count();
        for path in paths {
            let mut directory = None;
            for component in path.components().skip(prefix_length) {
                let parent = directory.as_ref().unwrap_or(&scratch);
                let name = component.as_os_str();
                directory = Some(match parent.read_child(name) {
                    Ok(Some(file)) => Directory::from_file(file)?,
                    Ok(None) => return Err(invalid_temp()),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        parent.create_transfer_child(name)?
                    }
                    Err(error) => return Err(error),
                });
            }
        }
        for file in [writable.join("probe"), writable.join("blocked"), marker] {
            fs::remove_file(file)?;
        }
        Ok(())
    }
}

pub(super) fn system_environment() -> io::Result<Vec<(&'static str, Option<OsString>)>> {
    let cmd = super::shell_program(crate::LocalShell::CommandPrompt)?;
    let system = cmd.parent().expect("system command has a parent");
    let windows = system.parent().expect("system directory has a parent");
    Ok(vec![
        ("ComSpec", Some(cmd.clone().into_os_string())),
        ("PATH", Some(system.as_os_str().to_owned())),
        ("PATHEXT", Some(".COM;.EXE;.BAT;.CMD".into())),
        ("SystemRoot", Some(windows.as_os_str().to_owned())),
        ("WINDIR", Some(windows.as_os_str().to_owned())),
    ])
}

pub(super) struct Scratch {
    root: PathBuf,
    anchor: Option<Directory>,
}

impl Scratch {
    pub(super) fn new() -> io::Result<Self> {
        let temp = canonical(&env::temp_dir())?;
        let parent = Directory::open_path(&temp)?;
        let nonce = super::pipe_name()?;
        // The pipe helper's suffix is a system-generated 128-bit nonce.
        let name = OsString::from(String::from_utf16_lossy(&nonce[9..nonce.len() - 1]));
        let directory = parent.create_backup_directory(&name)?;
        let mut scratch = Self {
            root: temp.join(name),
            anchor: None,
        };
        let initialized = (|| {
            directory.create_transfer_child(OsStr::new("session"))?;
            directory
                .create_ordinary_file(OsStr::new("outside"))?
                .write_all(b"outside")
        })();
        drop(directory);
        initialized?;
        scratch.anchor = Some(Directory::open_reviewed_removal_parent(&scratch.root)?);
        Ok(scratch)
    }

    pub(super) fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.anchor.take();
        // Windows remove_dir_all removes reparse points without traversing them.
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn check_local_path(path: &Path) -> io::Result<()> {
    let mut components = path.components();
    let local = matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && matches!(components.next(), Some(Component::RootDir))
        && components.all(|part| matches!(part, Component::Normal(name) if !name.encode_wide().any(|unit| unit == 0 || unit == b':' as u16)));
    if !local || path.parent().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows sandbox paths must be local, absolute, non-root directories or files",
        ));
    }
    Ok(())
}

pub(super) fn canonical(path: &Path) -> io::Result<PathBuf> {
    check_local_path(path)?;
    let resolved = fs::canonicalize(path)?;
    check_local_path(&resolved)?;
    let path = policy_path(&resolved)?;
    Ok(PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(path)))
}

fn canonical_or_missing(path: &Path) -> io::Result<PathBuf> {
    check_local_path(path)?;
    match canonical(path) {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or(error)?;
            let parent = path.parent().expect("non-root path has a parent");
            Ok(canonical_or_missing(parent)?.join(name))
        }
        Err(error) => Err(error),
    }
}

fn policy_path(path: &Path) -> io::Result<&str> {
    path.to_str()
        .filter(|path| path.len() <= 32_767)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "The Windows sandbox path cannot be represented in the native policy",
            )
        })
}

pub(super) fn overlaps(first: &Path, second: &Path) -> bool {
    contains(first, second) || contains(second, first)
}

pub(super) fn installation_directory(executable: &Path) -> io::Result<PathBuf> {
    canonical(executable.parent().ok_or_else(|| {
        io::Error::other(
            "The Windows sandbox broker installation directory could not be determined",
        )
    })?)
}

fn contains(parent: &Path, child: &Path) -> bool {
    let mut child = child.components();
    parent.components().all(|part| {
        child.next().is_some_and(|next| {
            let left: Vec<_> = part.as_os_str().encode_wide().collect();
            let right: Vec<_> = next.as_os_str().encode_wide().collect();
            // SAFETY: both pointers address their stated UTF-16 buffer lengths.
            unsafe {
                CompareStringOrdinal(
                    left.as_ptr(),
                    left.len() as i32,
                    right.as_ptr(),
                    right.len() as i32,
                    1,
                ) == CSTR_EQUAL
            }
        })
    })
}

type Create = unsafe extern "system" fn(*const c_void, u32, u32, *mut HANDLE) -> i32;
type Query = unsafe extern "system" fn(*mut u64) -> i32;
type Version = unsafe extern "system" fn(u32, *mut u8, *mut u32) -> i32;
type Close = unsafe extern "system" fn(HANDLE);
pub(super) type Export = unsafe extern "system" fn() -> isize;

pub(super) struct Module(NonZeroUsize);

impl Module {
    pub(super) fn load_system(name: &str) -> io::Result<Self> {
        let name: Vec<_> = name.encode_utf16().chain([0]).collect();
        // SAFETY: the DLL name is terminated and the search is restricted to System32.
        let module =
            unsafe { LoadLibraryExW(name.as_ptr(), null_mut(), LOAD_LIBRARY_SEARCH_SYSTEM32) };
        Ok(Self(
            NonZeroUsize::new(module as usize).ok_or_else(io::Error::last_os_error)?,
        ))
    }

    pub(super) fn export(&self, name: &CStr) -> io::Result<Export> {
        // SAFETY: the module is loaded and the name is a terminated C string.
        let export: FARPROC =
            unsafe { GetProcAddress(self.0.get() as HMODULE, name.as_ptr().cast()) };
        export.ok_or_else(io::Error::last_os_error)
    }
}

impl Drop for Module {
    fn drop(&mut self) {
        // SAFETY: this guard owns the reference from LoadLibraryExW.
        unsafe { FreeLibrary(self.0.get() as HMODULE) };
    }
}

struct Environment {
    handle: NonZeroUsize,
    close: Close,
    _module: Module,
}

impl Environment {
    fn new(policy: &[u8]) -> io::Result<Self> {
        let unavailable = |_| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Process Security Environment 1.1 is unavailable",
            )
        };
        let module = Module::load_system("processmodel.dll").map_err(unavailable)?;
        // SAFETY: each named processmodel export has the corresponding native ABI.
        let (create, query, version, close) = unsafe {
            (
                std::mem::transmute::<Export, Create>(
                    module
                        .export(c"CreateProcessSecurityEnvironment")
                        .map_err(unavailable)?,
                ),
                std::mem::transmute::<Export, Query>(
                    module
                        .export(c"QueryProcessSecurityEnvironmentSupport")
                        .map_err(unavailable)?,
                ),
                std::mem::transmute::<Export, Version>(
                    module
                        .export(c"IsProcessSecurityEnvironmentVersionSupported")
                        .map_err(unavailable)?,
                ),
                std::mem::transmute::<Export, Close>(
                    module
                        .export(c"CloseProcessSecurityEnvironment")
                        .map_err(unavailable)?,
                ),
            )
        };
        let mut flags = 0;
        let mut supported = 0;
        let mut minor = 0;
        // SAFETY: the native queries write into the output variables of the declared types.
        native_result("query Windows sandbox support", unsafe {
            query(&mut flags)
        })?;
        native_result("query Windows sandbox version", unsafe {
            version(1, &mut supported, &mut minor)
        })?;
        const FILESYSTEM_DENY: u64 = 1;
        const NETWORK_INGRESS: u64 = 8;
        let required = FILESYSTEM_DENY | NETWORK_INGRESS;
        if flags & required != required || supported == 0 || minor < 1 {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Windows Process Security Environment 1.1 with filesystem denial and network ingress policy is required",
            ));
        }
        let mut handle = null_mut();
        // SAFETY: the encoded PSEC 1.1 buffer is readable and handle is writable.
        native_result("create Windows sandbox", unsafe {
            create(policy.as_ptr().cast(), policy.len() as u32, 0, &mut handle)
        })?;
        let handle = NonZeroUsize::new(handle as usize)
            .ok_or_else(|| io::Error::other("Windows returned no sandbox handle"))?;
        Ok(Self {
            handle,
            close,
            _module: module,
        })
    }

    fn handle(&self) -> HANDLE {
        self.handle.get() as HANDLE
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        // SAFETY: this guard owns the PSEC handle and still holds the loaded DLL.
        unsafe { (self.close)(self.handle()) };
    }
}

fn native_result(operation: &str, result: i32) -> io::Result<()> {
    if result < 0 {
        return Err(io::Error::other(format!(
            "Could not {operation} (HRESULT {result:#010x})"
        )));
    }
    Ok(())
}

pub(super) fn verify_process(process: &OwnedHandle) -> io::Result<()> {
    let mut token = null_mut();
    let mut is_container = 0_u32;
    let mut returned = 0;
    // SAFETY: the process is suspended and token points to writable handle storage.
    bool_result(unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) })?;
    let token = owned_handle(token)?;
    // SAFETY: the token is live and the buffers match TokenIsAppContainer's DWORD output.
    bool_result(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenIsAppContainer,
            (&mut is_container as *mut u32).cast(),
            4,
            &mut returned,
        )
    })?;

    let mut storage = [0_usize; 48];
    // SAFETY: the aligned storage can hold the only expected capability SID and group.
    bool_result(unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenCapabilities,
            storage.as_mut_ptr().cast(),
            size_of_val(&storage) as u32,
            &mut returned,
        )
    })?;
    #[repr(C)]
    struct Groups {
        count: u32,
        values: [SID_AND_ATTRIBUTES; 2],
    }
    // SAFETY: the aligned, zero-initialized storage contains the returned
    // TOKEN_GROUPS header, with sufficient room for the two groups we accept.
    let groups = unsafe { &*storage.as_ptr().cast::<Groups>() };
    let mut internet = [0_u32; (SECURITY_MAX_SID_SIZE as usize).div_ceil(size_of::<u32>())];
    let mut bytes = size_of_val(&internet) as u32;
    // SAFETY: the documented maximum SID buffer and the length are writable.
    bool_result(unsafe {
        CreateWellKnownSid(
            WinCapabilityInternetClientSid,
            null_mut(),
            internet.as_mut_ptr().cast(),
            &mut bytes,
        )
    })?;
    let agentic = agentic_capability()?;
    let known = [internet.as_mut_ptr().cast(), agentic.values()[0]];
    let expected_capabilities = groups.count == 2
        && known.into_iter().all(|sid| {
            groups.values.iter().any(|group| {
                // SAFETY: both SIDs have live backing storage while compared.
                unsafe { EqualSid(group.Sid, sid) != 0 }
            })
        });

    let mut policy = PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY::default();
    // SAFETY: the process is live and the output type and size match the query.
    bool_result(unsafe {
        GetProcessMitigationPolicy(
            process.as_raw_handle(),
            ProcessSystemCallDisablePolicy,
            (&mut policy as *mut PROCESS_MITIGATION_SYSTEM_CALL_DISABLE_POLICY).cast(),
            size_of_val(&policy),
        )
    })?;
    // SAFETY: bit zero of the DWORD view is DisallowWin32kSystemCalls.
    let disabled = unsafe { policy.Anonymous.Flags } & 1 != 0;
    if is_container != 1 || !expected_capabilities || !disabled {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Windows sandbox token capabilities or Win32k policy could not be verified",
        ));
    }
    Ok(())
}

#[derive(Default)]
struct SidArray {
    pointer: *mut PSID,
    count: u32,
}

impl SidArray {
    fn values(&self) -> &[PSID] {
        if self.pointer.is_null() {
            return &[];
        }
        // SAFETY: the successful native derivation returned this allocation and length.
        unsafe { std::slice::from_raw_parts(self.pointer, self.count as usize) }
    }
}

impl Drop for SidArray {
    fn drop(&mut self) {
        for sid in self.values() {
            // SAFETY: the derivation transfers each SID and the containing array to us.
            unsafe { LocalFree(*sid) };
        }
        if !self.pointer.is_null() {
            // SAFETY: this is the still-owned native array, whose entries were just freed.
            unsafe { LocalFree(self.pointer.cast()) };
        }
    }
}

fn agentic_capability() -> io::Result<SidArray> {
    // PSEC adds this Windows capability to its AppContainer independently of
    // the capability string in the policy. Accept it by its exact SID.
    let name: Vec<_> = "AgenticAppContainer".encode_utf16().chain([0]).collect();
    let (mut groups, mut capabilities) = (SidArray::default(), SidArray::default());
    // SAFETY: name is terminated and all four outputs point to writable native values.
    bool_result(unsafe {
        DeriveCapabilitySidsFromName(
            name.as_ptr(),
            &mut groups.pointer,
            &mut groups.count,
            &mut capabilities.pointer,
            &mut capabilities.count,
        )
    })?;
    if capabilities.values().len() != 1 || capabilities.values()[0].is_null() {
        return Err(io::Error::other(
            "Windows could not identify its PSEC AppContainer capability",
        ));
    }
    Ok(capabilities)
}
