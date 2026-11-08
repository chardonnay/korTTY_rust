//! Bind a ready worker to its validated macOS application code signature.

use std::ffi::c_void;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::ptr::null;

use core_foundation_sys::base::{CFEqual, CFGetTypeID, CFRelease, CFTypeRef};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataGetTypeID};
use core_foundation_sys::dictionary::{
    CFDictionaryCreate, CFDictionaryGetCount, CFDictionaryGetTypeID, CFDictionaryGetValue,
    CFDictionaryRef, kCFTypeDictionaryKeyCallBacks, kCFTypeDictionaryValueCallBacks,
};
use core_foundation_sys::number::{
    CFBooleanGetTypeID, CFBooleanGetValue, CFNumberCreate, CFNumberGetTypeID, CFNumberGetValue,
    kCFNumberSInt32Type,
};
use core_foundation_sys::string::{
    CFStringCreateWithBytes, CFStringGetTypeID, CFStringRef, kCFStringEncodingUTF8,
};
use core_foundation_sys::url::{
    __CFURL, CFURLCreateFromFileSystemRepresentation, CFURLGetTypeID, CFURLRef,
};

const IDENTIFIER: &str = "de.kortty.rust.ssh-worker";
const DESKTOP_IDENTIFIER: &str = "de.kortty.rust";
const ENTITLEMENT: &str = "com.apple.security.app-sandbox";
// SecStaticCode.h, SecCode.h, and CSCommon.h in the macOS SDK.
const CHECK_ALL_ARCHITECTURES: u32 = 1 << 0;
const CHECK_NESTED_CODE: u32 = 1 << 3;
const STRICT_VALIDATE: u32 = 1 << 4;
const SIGNING_INFORMATION: u32 = 1 << 1;
const HARDENED_RUNTIME: u32 = 0x0001_0000;
const CDHASH_LEN: usize = 20;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecCodeInfoEntitlementsDict: CFStringRef;
    static kSecCodeInfoFlags: CFStringRef;
    static kSecCodeInfoIdentifier: CFStringRef;
    static kSecCodeInfoMainExecutable: CFStringRef;
    static kSecCodeInfoUnique: CFStringRef;
    static kSecGuestAttributePid: CFStringRef;

    fn SecStaticCodeCreateWithPath(path: CFURLRef, flags: u32, code: *mut CFTypeRef) -> i32;
    fn SecStaticCodeCheckValidity(code: CFTypeRef, flags: u32, requirement: CFTypeRef) -> i32;
    fn SecCodeCopySigningInformation(
        code: CFTypeRef,
        flags: u32,
        info: *mut CFDictionaryRef,
    ) -> i32;
    fn SecRequirementCreateWithString(
        text: CFStringRef,
        flags: u32,
        requirement: *mut CFTypeRef,
    ) -> i32;
    fn SecCodeCopyGuestWithAttributes(
        host: CFTypeRef,
        attrs: CFDictionaryRef,
        flags: u32,
        guest: *mut CFTypeRef,
    ) -> i32;
    fn SecCodeCheckValidity(code: CFTypeRef, flags: u32, requirement: CFTypeRef) -> i32;
}

/// The helper CodeDirectory hash captured from the signed bundle before spawn.
pub(super) struct ExpectedSignature([u8; CDHASH_LEN]);

pub(super) fn before_spawn(executable: &Path) -> io::Result<ExpectedSignature> {
    if !executable.is_absolute() {
        return Err(rejected("invalid macOS SSH worker identity"));
    }
    let executable = fs::canonicalize(executable)?;
    let (helper, desktop) = bundle_paths(&executable)?;
    let outer = static_code(desktop).map_err(|error| {
        rejected(format!(
            "macOS desktop bundle seal verification failed ({error})"
        ))
    })?;
    require_identifier(&signing_info(&outer)?, DESKTOP_IDENTIFIER)?;
    let code = static_code(helper)?;
    let digest = inspect_signature(&code, &executable)?;
    Ok(ExpectedSignature(digest))
}

impl ExpectedSignature {
    /// The launcher retains ownership of the live child PID through this check.
    pub(super) fn verify_running(&self, pid: libc::pid_t) -> io::Result<()> {
        if pid <= 0 {
            return Err(rejected("invalid macOS SSH worker identity"));
        }
        let requirement = requirement(self.0)?;
        let running = guest(pid)?;
        // SAFETY: the guest and requirement are live Security objects. Dynamic
        // validation binds the requirement to the running process, including
        // when its original pathname is replaced after the static check.
        check(
            unsafe { SecCodeCheckValidity(running.0, 0, requirement.0) },
            "running macOS SSH worker failed code signature verification",
        )
    }
}

fn bundle_paths(executable: &Path) -> io::Result<(&Path, &Path)> {
    let invalid = || rejected("SSH worker must be embedded in the signed macOS desktop bundle");
    let helper = executable
        .parent()
        .filter(|path| path.ends_with("Contents/MacOS"))
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == "kortty-ssh-worker.app")
        })
        .ok_or_else(invalid)?;
    let desktop = helper
        .parent()
        .filter(|path| path.ends_with("Contents/Helpers"))
        .and_then(Path::parent)
        .and_then(Path::parent)
        .filter(|path| path.extension().is_some_and(|extension| extension == "app"))
        .ok_or_else(invalid)?;
    if executable
        .file_name()
        .is_none_or(|name| name != "kortty-ssh-worker")
    {
        return Err(invalid());
    }
    Ok((helper, desktop))
}

fn static_code(bundle: &Path) -> io::Result<OwnedCf<c_void>> {
    let path = path_url(bundle, true)?;
    let mut code = null();
    // SAFETY: path and the code output are valid through the call.
    check(
        unsafe { SecStaticCodeCreateWithPath(path.0, 0, &mut code) },
        "could not inspect the macOS bundle signature",
    )?;
    let code = OwnedCf::new(code)?;
    // SAFETY: the opaque object is a live SecStaticCode. NULL requests the
    // normal signature validation without an additional signer requirement.
    check(
        unsafe {
            SecStaticCodeCheckValidity(
                code.0,
                CHECK_ALL_ARCHITECTURES | CHECK_NESTED_CODE | STRICT_VALIDATE,
                null(),
            )
        },
        "macOS bundle signature is invalid",
    )?;
    Ok(code)
}

fn signing_info(
    code: &OwnedCf<c_void>,
) -> io::Result<OwnedCf<core_foundation_sys::dictionary::__CFDictionary>> {
    let mut info = null();
    // SAFETY: code is already validated and the signing dictionary output is writable.
    check(
        unsafe { SecCodeCopySigningInformation(code.0, SIGNING_INFORMATION, &mut info) },
        "could not read the macOS bundle signature",
    )?;
    OwnedCf::new(info)
}

fn require_identifier(
    info: &OwnedCf<core_foundation_sys::dictionary::__CFDictionary>,
    expected: &str,
) -> io::Result<()> {
    let expected_identifier = string(expected)?;
    // SAFETY: Security supplies the dictionary and key. Verify the returned type.
    if unsafe {
        let identifier = CFDictionaryGetValue(info.0, kSecCodeInfoIdentifier.cast());
        identifier.is_null()
            || CFGetTypeID(identifier) != CFStringGetTypeID()
            || CFEqual(identifier, expected_identifier.0.cast()) == 0
    } {
        return Err(rejected("unexpected macOS bundle signing identifier"));
    }
    Ok(())
}

fn inspect_signature(code: &OwnedCf<c_void>, executable: &Path) -> io::Result<[u8; CDHASH_LEN]> {
    let info = signing_info(code)?;
    require_identifier(&info, IDENTIFIER)?;
    let executable = path_url(executable, false)?;
    let app_sandbox = string(ENTITLEMENT)?;
    // SAFETY: Security supplies these keys. CF's value types are checked before
    // calling typed accessors and the signing dictionary stays alive throughout.
    unsafe {
        let main = CFDictionaryGetValue(info.0, kSecCodeInfoMainExecutable.cast());
        if main.is_null()
            || CFGetTypeID(main) != CFURLGetTypeID()
            || CFEqual(main, executable.0.cast()) == 0
        {
            return Err(rejected(
                "macOS SSH worker bundle names a different executable",
            ));
        }
        let entitlements = CFDictionaryGetValue(info.0, kSecCodeInfoEntitlementsDict.cast());
        if entitlements.is_null()
            || CFGetTypeID(entitlements) != CFDictionaryGetTypeID()
            || CFDictionaryGetCount(entitlements.cast()) != 1
        {
            return Err(rejected("unexpected macOS SSH worker entitlements"));
        }
        let enabled = CFDictionaryGetValue(entitlements.cast(), app_sandbox.0.cast());
        if enabled.is_null()
            || CFGetTypeID(enabled) != CFBooleanGetTypeID()
            || !CFBooleanGetValue(enabled.cast())
        {
            return Err(rejected(
                "macOS SSH worker App Sandbox entitlement is absent",
            ));
        }
        let flags = CFDictionaryGetValue(info.0, kSecCodeInfoFlags.cast());
        let mut value = 0_u32;
        if flags.is_null()
            || CFGetTypeID(flags) != CFNumberGetTypeID()
            || !CFNumberGetValue(flags.cast(), kCFNumberSInt32Type, (&raw mut value).cast())
            || value & HARDENED_RUNTIME == 0
        {
            return Err(rejected("macOS SSH worker requires hardened runtime"));
        }
        let digest = CFDictionaryGetValue(info.0, kSecCodeInfoUnique.cast());
        if digest.is_null()
            || CFGetTypeID(digest) != CFDataGetTypeID()
            || CFDataGetLength(digest.cast()) != CDHASH_LEN as isize
        {
            return Err(rejected(
                "macOS SSH worker has no supported CodeDirectory hash",
            ));
        }
        let bytes = CFDataGetBytePtr(digest.cast());
        if bytes.is_null() {
            return Err(rejected(
                "macOS SSH worker CodeDirectory hash is unavailable",
            ));
        }
        let mut result = [0; CDHASH_LEN];
        result.copy_from_slice(std::slice::from_raw_parts(bytes, CDHASH_LEN));
        Ok(result)
    }
}

fn path_url(path: &Path, directory: bool) -> io::Result<OwnedCf<__CFURL>> {
    let bytes = path.as_os_str().as_bytes();
    let length =
        isize::try_from(bytes.len()).map_err(|_| rejected("macOS code bundle path is too long"))?;
    // SAFETY: the buffer is valid for length bytes and CFURL copies it.
    OwnedCf::new(unsafe {
        CFURLCreateFromFileSystemRepresentation(null(), bytes.as_ptr(), length, u8::from(directory))
    })
}

fn requirement(digest: [u8; CDHASH_LEN]) -> io::Result<OwnedCf<c_void>> {
    let mut expression = format!(r#"identifier "{IDENTIFIER}" and cdhash H""#);
    for byte in digest {
        let _ = write!(expression, "{byte:02x}");
    }
    expression.push('"');
    let expression = string(&expression)?;
    let mut requirement = null();
    // SAFETY: expression is a live CFString and the output is writable.
    check(
        unsafe { SecRequirementCreateWithString(expression.0, 0, &mut requirement) },
        "could not construct the macOS SSH worker code requirement",
    )?;
    OwnedCf::new(requirement)
}

fn guest(pid: libc::pid_t) -> io::Result<OwnedCf<c_void>> {
    // SAFETY: a Darwin pid_t is an i32, copied by CFNumberCreate.
    let pid = OwnedCf::new(unsafe {
        CFNumberCreate(null(), kCFNumberSInt32Type, (&raw const pid).cast())
    })?;
    // SAFETY: this is the published CFString key for a PID lookup.
    let key = unsafe { kSecGuestAttributePid.cast() };
    let value = pid.0.cast();
    // SAFETY: the one-element arrays contain live CF objects and the normal
    // callbacks retain them for the lifetime of the dictionary.
    let attrs = OwnedCf::new(unsafe {
        CFDictionaryCreate(
            null(),
            &key,
            &value,
            1,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    })?;
    let mut code = null();
    // SAFETY: NULL asks the kernel's code host for the live PID guest.
    check(
        unsafe { SecCodeCopyGuestWithAttributes(null(), attrs.0, 0, &mut code) },
        "could not identify the running macOS SSH worker",
    )?;
    OwnedCf::new(code)
}

fn string(value: &str) -> io::Result<OwnedCf<core_foundation_sys::string::__CFString>> {
    let length = isize::try_from(value.len())
        .map_err(|_| rejected("invalid macOS SSH worker code requirement length"))?;
    // SAFETY: the UTF-8 bytes are live and CFStringCreateWithBytes copies them.
    OwnedCf::new(unsafe {
        CFStringCreateWithBytes(null(), value.as_ptr(), length, kCFStringEncodingUTF8, 0)
    })
}

struct OwnedCf<T>(*const T);

impl<T> OwnedCf<T> {
    fn new(object: *const T) -> io::Result<Self> {
        if object.is_null() {
            Err(rejected("macOS could not create a code signature object"))
        } else {
            Ok(Self(object))
        }
    }
}

impl<T> Drop for OwnedCf<T> {
    fn drop(&mut self) {
        // SAFETY: every wrapped pointer is a non-null retained CF object.
        unsafe { CFRelease(self.0.cast()) };
    }
}

fn check(status: i32, action: &'static str) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(rejected(format!("{action} ({status})")))
    }
}

fn rejected(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}
