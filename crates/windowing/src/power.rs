#[cfg(target_os = "macos")]
pub use kortty_macos::IdleSleepAssertion as SystemSleepRequest;

/// Whether the desktop platform exposes the idle-sleep request used by kortty.
pub const fn supports_system_sleep_prevention() -> bool {
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
#[derive(Debug)]
pub struct SystemSleepRequest {
    _private: (),
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
impl SystemSleepRequest {
    pub fn acquire() -> std::io::Result<Self> {
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
    }
}

#[cfg(target_os = "windows")]
pub use windows::SystemSleepRequest;

#[cfg(target_os = "windows")]
mod windows {
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Power::{
        PowerClearRequest, PowerCreateRequest, PowerRequestSystemRequired, PowerSetRequest,
    };
    use windows_sys::Win32::System::SystemServices::POWER_REQUEST_CONTEXT_VERSION;
    use windows_sys::Win32::System::Threading::{
        POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
    };

    /// Owns one system-required request and its native handle.
    #[derive(Debug)]
    pub struct SystemSleepRequest(OwnedHandle);

    impl SystemSleepRequest {
        pub fn acquire() -> io::Result<Self> {
            let mut reason: Vec<_> = "kortty terminal, scheduled or AI work"
                .encode_utf16()
                .chain([0])
                .collect();
            let context = REASON_CONTEXT {
                Version: POWER_REQUEST_CONTEXT_VERSION,
                Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
                Reason: REASON_CONTEXT_0 {
                    SimpleReasonString: reason.as_mut_ptr(),
                },
            };
            // SAFETY: The context and its terminated UTF-16 reason live for the call.
            let raw = unsafe { PowerCreateRequest(&context) };
            if raw == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: PowerCreateRequest transferred ownership of this handle.
            let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
            // SAFETY: The valid handle remains live until its OwnedHandle drops.
            if unsafe { PowerSetRequest(handle.as_raw_handle(), PowerRequestSystemRequired) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(handle))
        }
    }

    impl Drop for SystemSleepRequest {
        fn drop(&mut self) {
            // SAFETY: One successful PowerSetRequest on this live handle belongs to us.
            unsafe { PowerClearRequest(self.0.as_raw_handle(), PowerRequestSystemRequired) };
        }
    }
}
