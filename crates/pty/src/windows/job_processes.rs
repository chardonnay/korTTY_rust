use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::JobObjects::{
    IsProcessInJob, JobObjectBasicProcessIdList, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};

use crate::JobProcessIdentity;

use super::{owned_handle, signaled};

// The Win32 structure ends in a variable-length ULONG_PTR array.
#[repr(C)]
struct JobProcesses {
    assigned: u32,
    listed: u32,
    pids: [usize; 1024],
}

pub(super) fn find<T>(
    job: &OwnedHandle,
    previous: Option<JobProcessIdentity>,
    inspect: impl Fn(&str, &[&str]) -> Option<T>,
) -> Option<(JobProcessIdentity, T)> {
    let examine = |pid| examine(job, pid, &inspect);
    if let Some(found) = previous.and_then(|id| examine(id.pid).filter(|(found, _)| *found == id)) {
        return Some(found);
    }
    let processes = processes(job)?;
    processes
        .pids
        .get(..processes.listed as usize)?
        .iter()
        .filter_map(|&pid| u32::try_from(pid).ok())
        .find_map(examine)
}

pub(super) fn only_process(job: &OwnedHandle, pid: u32) -> bool {
    processes(job).is_some_and(|processes| {
        processes.assigned == 1 && processes.listed == 1 && processes.pids[0] == pid as usize
    })
}

fn processes(job: &OwnedHandle) -> Option<JobProcesses> {
    let mut processes = JobProcesses {
        assigned: 0,
        listed: 0,
        pids: [0; 1024],
    };
    // SAFETY: the handle is the terminal's private job. The writable C-layout
    // buffer has the exact supplied capacity for the variable-length record.
    if unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicProcessIdList,
            (&raw mut processes).cast(),
            size_of::<JobProcesses>() as u32,
            null_mut(),
        )
    } == 0
        || processes.listed > processes.assigned
    {
        return None;
    }
    Some(processes)
}

fn examine<T>(
    job: &OwnedHandle,
    pid: u32,
    inspect: &impl Fn(&str, &[&str]) -> Option<T>,
) -> Option<(JobProcessIdentity, T)> {
    // SAFETY: opening a numeric process ID does not inherit the new handle.
    let process = owned_handle(unsafe {
        OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid)
    })
    .ok()?;
    let mut in_job = 0;
    // SAFETY: both handles are live and the BOOL output is writable.
    if unsafe { IsProcessInJob(process.as_raw_handle(), job.as_raw_handle(), &mut in_job) } == 0
        || in_job == 0
    {
        return None;
    }
    let mut path = [0_u16; 1024];
    let mut length = path.len() as u32;
    // SAFETY: the process handle has query access and path and length are writable.
    if unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            path.as_mut_ptr(),
            &mut length,
        )
    } == 0
    {
        return None;
    }
    let found = inspect(&String::from_utf16_lossy(path.get(..length as usize)?), &[])?;
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: the process handle has query rights and all four outputs are writable.
    if unsafe {
        GetProcessTimes(
            process.as_raw_handle(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
    } == 0
        || !matches!(signaled(&process, 0), Ok(false))
    {
        return None;
    }
    let started = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
    Some((
        JobProcessIdentity {
            pid,
            started: (started, 0),
        },
        found,
    ))
}
