use std::io;
use std::ptr;

use core_foundation_sys::string::CFStringRef;
use objc2_foundation::ns_string;

const ASSERTION_LEVEL_ON: u32 = 255;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(
        assertion_type: CFStringRef,
        level: u32,
        name: CFStringRef,
        id: *mut u32,
    ) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
}

/// A process-owned assertion against automatic idle system sleep.
#[derive(Debug)]
pub struct IdleSleepAssertion(u32);

impl IdleSleepAssertion {
    pub fn acquire() -> io::Result<Self> {
        let mut id = 0;
        // SAFETY: NSString and CFString are toll-free bridged. Both strings
        // have static lifetime, and IOKit writes the assertion ID to this stack slot.
        let result = unsafe {
            IOPMAssertionCreateWithName(
                ptr::from_ref(ns_string!("PreventUserIdleSystemSleep")).cast(),
                ASSERTION_LEVEL_ON,
                ptr::from_ref(ns_string!("kortty terminal, scheduled or AI work")).cast(),
                &mut id,
            )
        };
        if result == 0 {
            Ok(Self(id))
        } else {
            Err(io::Error::other(format!(
                "IOKit power assertion error {result:#010x}"
            )))
        }
    }
}

impl Drop for IdleSleepAssertion {
    fn drop(&mut self) {
        // SAFETY: This object owns one successful acquisition of this assertion.
        unsafe { IOPMAssertionRelease(self.0) };
    }
}
