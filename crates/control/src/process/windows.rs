use std::collections::HashMap;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use windows_sys::Win32::Foundation::{
    ERROR_NO_MORE_FILES, FILETIME, GetLastError, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject,
};

use super::Process;

const MAX_PROCESS_ENTRIES: usize = 131_072;

pub(super) struct Snapshot {
    parents: HashMap<u32, u32>,
}

impl Snapshot {
    pub(super) fn new() -> Option<Self> {
        // SAFETY: TH32CS_SNAPPROCESS requires no process handle and ignores PID.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        // SAFETY: the successful call transferred the only owner of this handle.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..PROCESSENTRY32W::default()
        };
        // SAFETY: the snapshot is live and entry is an initialized writable record.
        if unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } == 0 {
            return None;
        }
        let mut parents = HashMap::new();
        loop {
            if parents.len() == MAX_PROCESS_ENTRIES
                || parents
                    .insert(entry.th32ProcessID, entry.th32ParentProcessID)
                    .is_some()
            {
                return None;
            }
            // SAFETY: the snapshot and writable entry remain valid across the call.
            if unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } == 0 {
                // SAFETY: retrieves the calling thread's error for Process32NextW.
                return (unsafe { GetLastError() } == ERROR_NO_MORE_FILES)
                    .then_some(Self { parents });
            }
        }
    }

    pub(super) fn process(&self, pid: u32) -> Option<Process> {
        let parent = *self.parents.get(&pid)?;
        // SAFETY: OpenProcess takes a numeric PID and does not inherit the handle.
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: OpenProcess returned a new process handle owned by this call.
        let process = unsafe { OwnedHandle::from_raw_handle(handle) };
        let (mut creation, mut exit, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the handle has query rights and all four time outputs are writable.
        if unsafe {
            GetProcessTimes(
                process.as_raw_handle(),
                &mut creation,
                &mut exit,
                &mut kernel,
                &mut user,
            )
        } == 0
        {
            return None;
        }
        // SAFETY: the handle has SYNCHRONIZE. A zero timeout does not block.
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } != WAIT_TIMEOUT {
            return None;
        }
        let started =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        (started != 0).then_some(Process {
            parent,
            started: (started, 0),
        })
    }
}
