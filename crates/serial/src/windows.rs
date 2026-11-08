use std::io::{self, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows_sys::Win32::Devices::Communication::{
    COMMTIMEOUTS, ClearCommBreak, DCB, EVENPARITY, GetCommPorts, GetCommState, GetCommTimeouts,
    MARKPARITY, NOPARITY, ODDPARITY, ONE5STOPBITS, ONESTOPBIT, SPACEPARITY, SetCommBreak,
    SetCommState, SetCommTimeouts, TWOSTOPBITS,
};
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_SUCCESS, GENERIC_READ,
    GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
};

use crate::{FlowControl, Parity, Settings, StopBits, closed, unsupported};

const READ_TIMEOUT: u32 = 100;
const WRITE_TIMEOUT: u32 = 1000;
const MAX_PORTS: u32 = 4096;

pub(crate) struct Port(Arc<Shared>);

struct Shared {
    device: Mutex<Option<Arc<OwnedHandle>>>,
    cancelled: OwnedHandle,
    break_lock: Mutex<()>,
}

impl Shared {
    fn device(&self) -> Option<Arc<OwnedHandle>> {
        self.device
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    fn wait_cancelled(&self, millis: u32) -> io::Result<bool> {
        // SAFETY: cancelled is a live manual-reset event.
        match unsafe { WaitForSingleObject(self.cancelled.as_raw_handle(), millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Port {
    pub(crate) fn open(
        path: &Path,
        settings: Settings,
    ) -> io::Result<(Self, SerialReader, SerialWriter)> {
        let number = com_number(path)?;
        let cancelled = new_event()?;
        let read_event = new_event()?;
        let write_event = new_event()?;
        let device_name: Vec<u16> = format!(r"\\.\COM{number}")
            .encode_utf16()
            .chain([0])
            .collect();
        // SAFETY: The name is NUL-terminated. Share mode zero opens the port
        // exclusively, and a null security pointer prevents handle inheritance.
        let raw = unsafe {
            CreateFileW(
                device_name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: CreateFileW returned a newly owned valid handle.
        let device = unsafe { OwnedHandle::from_raw_handle(raw) };
        configure(device.as_raw_handle(), settings)?;
        let shared = Arc::new(Shared {
            device: Mutex::new(Some(Arc::new(device))),
            cancelled,
            break_lock: Mutex::new(()),
        });
        let reader = SerialReader(Endpoint {
            shared: Arc::clone(&shared),
            event: read_event,
        });
        let writer = SerialWriter(Endpoint {
            shared: Arc::clone(&shared),
            event: write_event,
        });
        Ok((Self(shared), reader, writer))
    }

    pub(crate) fn close(&self) -> io::Result<()> {
        let device = self
            .0
            .device
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if device.is_some() {
            // SAFETY: The manual-reset event stays live until all workers drop it.
            check(unsafe { SetEvent(self.0.cancelled.as_raw_handle()) })?;
        }
        Ok(())
    }

    pub(crate) fn send_break(&self, duration: Duration) -> io::Result<()> {
        let _guard = self
            .0
            .break_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let device = self.0.device().ok_or_else(closed)?;
        let handle = device.as_raw_handle();
        // SAFETY: The owned serial handle remains live through the matching clear.
        check(unsafe { SetCommBreak(handle) })?;
        let result = self
            .0
            .wait_cancelled(duration.as_millis() as u32)
            .and_then(|cancelled| if cancelled { Err(closed()) } else { Ok(()) });
        // SAFETY: Same handle as SetCommBreak, still owned by this operation.
        let cleared = check(unsafe { ClearCommBreak(handle) });
        result.and(cleared)
    }
}

struct Endpoint {
    shared: Arc<Shared>,
    event: OwnedHandle,
}

impl Endpoint {
    fn transfer(
        &mut self,
        device: &OwnedHandle,
        begin: impl FnOnce(HANDLE, *mut OVERLAPPED) -> i32,
    ) -> io::Result<usize> {
        let handle = device.as_raw_handle();
        let event = self.event.as_raw_handle();
        // SAFETY: The event is dedicated to this endpoint and has no pending I/O.
        check(unsafe { ResetEvent(event) })?;
        let mut operation = OVERLAPPED {
            hEvent: event,
            ..OVERLAPPED::default()
        };
        if begin(handle, &mut operation) == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                return Err(error);
            }
            let handles = [self.shared.cancelled.as_raw_handle(), event];
            // SAFETY: Both event handles remain valid through the wait.
            let result = unsafe {
                WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, INFINITE)
            };
            if result != WAIT_OBJECT_0 + 1 {
                let error = if result == WAIT_FAILED {
                    io::Error::last_os_error()
                } else {
                    closed()
                };
                let mut ignored = 0;
                // SAFETY: This operation and its caller's buffer remain live
                // until the driver acknowledges completion, even if cancellation
                // raced with normal completion and returned ERROR_NOT_FOUND.
                unsafe {
                    CancelIoEx(handle, &operation);
                    GetOverlappedResult(handle, &operation, &mut ignored, 1);
                }
                return Err(error);
            }
        }
        let mut bytes = 0;
        // SAFETY: The request is complete or its dedicated event is signaled.
        // The owned handle, overlapped state and caller's buffer are still live.
        check(unsafe { GetOverlappedResult(handle, &operation, &mut bytes, 1) })?;
        Ok(bytes as usize)
    }
}

pub struct SerialReader(Endpoint);

impl Read for SerialReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let Some(device) = self.0.shared.device() else {
            return Ok(0);
        };
        let length = output.len().min(u32::MAX as usize) as u32;
        loop {
            let started = Instant::now();
            let result = self.0.transfer(&device, |handle, operation| {
                // SAFETY: output remains borrowed and valid until the endpoint
                // observes this overlapped read's final completion status.
                unsafe { ReadFile(handle, output.as_mut_ptr(), length, null_mut(), operation) }
            });
            match result {
                Ok(0) => {
                    let delay = Duration::from_millis(u64::from(READ_TIMEOUT))
                        .saturating_sub(started.elapsed());
                    if self.0.shared.wait_cancelled(delay.as_millis() as u32)? {
                        return Ok(0);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(0),
                result => return result,
            }
        }
    }
}

pub struct SerialWriter(Endpoint);

impl Write for SerialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let device = self.0.shared.device().ok_or_else(closed)?;
        let length = bytes.len().min(4096) as u32;
        match self.0.transfer(&device, |handle, operation| {
            // SAFETY: bytes remains borrowed and valid until this operation completes.
            unsafe { WriteFile(handle, bytes.as_ptr(), length, null_mut(), operation) }
        }) {
            Ok(0) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "serial write made no progress",
            )),
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // FlushFileBuffers can wait forever for flow-controlled bytes to drain.
        Ok(())
    }
}

fn new_event() -> io::Result<OwnedHandle> {
    // SAFETY: An unnamed, noninheritable manual-reset event needs no attributes.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: CreateEventW returned a newly owned, non-null event.
        Ok(unsafe { OwnedHandle::from_raw_handle(event) })
    }
}

fn configure(handle: HANDLE, settings: Settings) -> io::Result<()> {
    let original = get_state(handle)?;
    let mut original_timeouts = COMMTIMEOUTS::default();
    // SAFETY: The handle is open and the timeouts structure is writable.
    check(unsafe { GetCommTimeouts(handle, &mut original_timeouts) })?;
    let result = configure_state(handle, original, settings);
    if result.is_err() {
        // SAFETY: Both snapshots were obtained from this live serial handle.
        unsafe {
            SetCommState(handle, &original);
            SetCommTimeouts(handle, &original_timeouts);
        }
    }
    result
}

fn configure_state(handle: HANDLE, mut state: DCB, settings: Settings) -> io::Result<()> {
    state.BaudRate = settings.baud_rate;
    state.ByteSize = settings.data_bits;
    state.Parity = match settings.parity {
        Parity::None => NOPARITY,
        Parity::Odd => ODDPARITY,
        Parity::Even => EVENPARITY,
        Parity::Mark => MARKPARITY,
        Parity::Space => SPACEPARITY,
    };
    state.StopBits = match settings.stop_bits {
        StopBits::One => ONESTOPBIT,
        StopBits::OnePointFive => ONE5STOPBITS,
        StopBits::Two => TWOSTOPBITS,
    };
    state.XonChar = 0x11;
    state.XoffChar = 0x13;
    state._bitfield = flags(settings);
    // SAFETY: state is initialized and describes a valid serial handle.
    check(unsafe { SetCommState(handle, &state) })?;
    let actual = get_state(handle)?;
    // Exclude driver-specific and unused bits from readback.
    const CONFIGURED_FLAGS: u32 = 0x333f;
    if actual.BaudRate != state.BaudRate
        || actual.ByteSize != state.ByteSize
        || actual.Parity != state.Parity
        || actual.StopBits != state.StopBits
        || actual._bitfield & CONFIGURED_FLAGS != state._bitfield & CONFIGURED_FLAGS
    {
        return Err(unsupported(
            "serial driver did not retain the requested line settings",
        ));
    }
    let timeouts = COMMTIMEOUTS {
        ReadIntervalTimeout: u32::MAX,
        ReadTotalTimeoutMultiplier: u32::MAX,
        ReadTotalTimeoutConstant: READ_TIMEOUT,
        WriteTotalTimeoutMultiplier: 1,
        WriteTotalTimeoutConstant: WRITE_TIMEOUT,
    };
    // SAFETY: timeouts is initialized and handle is open.
    check(unsafe { SetCommTimeouts(handle, &timeouts) })
}

fn get_state(handle: HANDLE) -> io::Result<DCB> {
    let mut state = DCB {
        DCBlength: size_of::<DCB>() as u32,
        ..DCB::default()
    };
    // SAFETY: state is a writable DCB of the size declared in DCBlength.
    check(unsafe { GetCommState(handle, &mut state) })?;
    Ok(state)
}

fn flags(settings: Settings) -> u32 {
    // DCB bitfields in winbase.h. Control values 0/1/2 disable, assert,
    // or enable driver handshaking respectively.
    let rts_cts = settings.flow_control == FlowControl::RtsCts;
    let dsr_dtr = settings.flow_control == FlowControl::DsrDtr;
    let xon_xoff = settings.flow_control == FlowControl::XonXoff;
    let dtr = if dsr_dtr {
        2
    } else {
        u32::from(settings.assert_dtr)
    };
    let rts = if rts_cts {
        2
    } else {
        u32::from(settings.assert_rts)
    };
    1 | (u32::from(settings.parity != Parity::None) << 1)
        | (u32::from(rts_cts) << 2)
        | (u32::from(dsr_dtr) << 3)
        | (dtr << 4)
        | (u32::from(xon_xoff) << 8)
        | (u32::from(xon_xoff) << 9)
        | (rts << 12)
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn com_number(path: &Path) -> io::Result<u32> {
    let value = path.as_os_str().to_str().and_then(|name| {
        let name = name.strip_prefix(r"\\.\").unwrap_or(name);
        let prefix = name.get(..3)?;
        let number = name.get(3..)?;
        if !prefix.eq_ignore_ascii_case("COM") || !number.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        number.parse::<u32>().ok().filter(|&number| number != 0)
    });
    value.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "serial device must be a Windows COM name",
        )
    })
}

pub(crate) fn validate(path: &Path, settings: Settings) -> io::Result<()> {
    com_number(path)?;
    if (settings.stop_bits == StopBits::OnePointFive && settings.data_bits != 5)
        || (settings.stop_bits == StopBits::Two && settings.data_bits == 5)
    {
        return Err(unsupported(
            "Windows requires 5 data bits for 1.5 stop bits and 6–8 for 2",
        ));
    }
    Ok(())
}

pub(crate) fn available_ports() -> io::Result<Vec<PathBuf>> {
    let mut numbers = vec![0; 32];
    for _ in 0..4 {
        let mut found = 0;
        // SAFETY: numbers has room for its declared count and found is writable.
        let status =
            unsafe { GetCommPorts(numbers.as_mut_ptr(), numbers.len() as u32, &mut found) };
        match status {
            ERROR_SUCCESS if found as usize <= numbers.len() => {
                numbers.truncate(found as usize);
                numbers.sort_unstable();
                numbers.dedup();
                return Ok(numbers
                    .into_iter()
                    .filter(|&number| number != 0)
                    .map(|number| PathBuf::from(format!("COM{number}")))
                    .collect());
            }
            ERROR_FILE_NOT_FOUND => return Ok(Vec::new()),
            ERROR_MORE_DATA if (numbers.len() as u32 + 1..=MAX_PORTS).contains(&found) => {
                numbers.resize(found as usize, 0);
            }
            ERROR_SUCCESS => return Err(io::Error::other("invalid Windows serial port list size")),
            other => return Err(io::Error::from_raw_os_error(other as i32)),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "serial port list is changing",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_only_com_device_names() {
        assert_eq!(com_number(Path::new("com12")).unwrap(), 12);
        assert_eq!(com_number(Path::new(r"\\.\COM3")).unwrap(), 3);
        for invalid in ["COM0", "COM+3", r"\\.\PIPE", "CON", "COM2.txt"] {
            assert_eq!(
                com_number(Path::new(invalid)).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn translates_each_flow_mode_to_the_driver_owned_signal() {
        assert_eq!(flags(Settings::default()), 0x1011);
        assert_eq!(
            flags(Settings {
                flow_control: FlowControl::RtsCts,
                ..Settings::default()
            }),
            0x2015
        );
        assert_eq!(
            flags(Settings {
                flow_control: FlowControl::DsrDtr,
                ..Settings::default()
            }),
            0x1029
        );
        assert_eq!(
            flags(Settings {
                flow_control: FlowControl::XonXoff,
                ..Settings::default()
            }),
            0x1311
        );
    }
}
