use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::net::TcpListener;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::MetadataExt as _;
use std::os::windows::io::{
    AsRawHandle as _, AsRawSocket as _, FromRawHandle as _, FromRawSocket as _, OwnedHandle,
    OwnedSocket, RawSocket,
};
use std::path::{Component, Path};
use std::ptr::{from_mut, from_ref, null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE, HANDLE,
    INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, IN_ADDR, IN_ADDR_0, INVALID_SOCKET, IPPROTO_TCP, SO_EXCLUSIVEADDRUSE, SOCK_STREAM,
    SOCKADDR_IN, SOL_SOCKET, WSA_FLAG_NO_HANDLE_INHERIT, WSA_FLAG_OVERLAPPED, WSADATA,
    WSAGetLastError, WSASocketW, WSAStartup, bind, listen, setsockopt,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, SET_ACCESS,
    SetEntriesInAclW, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetSidLengthRequired, GetTokenInformation, INHERITED_ACE,
    InitializeSecurityDescriptor, IsValidAcl, IsValidSid, OWNER_SECURITY_INFORMATION, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SECURITY_MAX_SID_SIZE,
    SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_CREATION_DISPOSITION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    GetFileType, OPEN_EXISTING, READ_CONTROL, SECURITY_IDENTIFICATION, SECURITY_SQOS_PRESENT,
};
use windows_sys::Win32::System::SystemServices::{
    ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub(super) fn create_private_directory(path: &Path) -> io::Result<()> {
    let name = wide_path(path)?;
    let user = UserSid::current()?;
    let (acl, mut descriptor) = directory_descriptor(user.sid())?;
    let attributes = security_attributes(&mut descriptor);
    // SAFETY: The NUL-terminated path, descriptor, SID and ACL remain live through the call.
    let created = unsafe { CreateDirectoryW(name.as_ptr(), &attributes) };
    if created == 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(code) if code == ERROR_ALREADY_EXISTS as i32 || code == ERROR_FILE_EXISTS as i32)
        {
            return Err(error);
        }
    }
    drop(acl);
    open_checked_directory(path, user.sid()).map(drop)
}

pub(super) fn verify_private_directory(path: &Path) -> io::Result<()> {
    let user = UserSid::current()?;
    open_checked_directory(path, user.sid()).map(drop)
}

pub(super) fn open_private_file(path: &Path, write_new: bool) -> io::Result<File> {
    let user = UserSid::current()?;
    let parent = path.parent().ok_or_else(private_error)?;
    // Hold the verified directory without FILE_SHARE_DELETE while resolving its child.
    let directory = open_checked_directory(parent, user.sid())?;
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let file = if write_new {
        // Specify TokenUser as owner even for elevated tokens with a group default
        // owner. Omitting the DACL lets Windows inherit it from the private directory.
        let mut descriptor = owner_descriptor(user.sid())?;
        let attributes = security_attributes(&mut descriptor);
        open_file(path, GENERIC_WRITE, share, CREATE_NEW, Some(&attributes))?
    } else {
        open_file(path, GENERIC_READ, share, OPEN_EXISTING, None)?
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(private_error());
    }
    verify_security(&file, user.sid(), Object::File { write_new })?;
    drop(directory);
    Ok(file)
}

fn open_checked_directory(path: &Path, sid: PSID) -> io::Result<File> {
    let directory = open_file(
        path,
        0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        OPEN_EXISTING,
        None,
    )?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(private_error());
    }
    verify_security(&directory, sid, Object::Directory)?;
    Ok(directory)
}

fn open_file(
    path: &Path,
    access: u32,
    share: u32,
    disposition: FILE_CREATION_DISPOSITION,
    attributes: Option<&SECURITY_ATTRIBUTES>,
) -> io::Result<File> {
    let name = wide_path(path)?;
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | FILE_FLAG_BACKUP_SEMANTICS
        | SECURITY_SQOS_PRESENT
        | SECURITY_IDENTIFICATION;
    // SAFETY: The path and any supplied security attributes remain live. No
    // template handle is supplied. The returned handle is exclusively owned here.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access | READ_CONTROL | FILE_READ_ATTRIBUTES,
            share,
            attributes.map_or(null(), from_ref),
            disposition,
            flags,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW succeeded. File takes over the one close of the handle.
    let file = unsafe { File::from_raw_handle(handle) };
    // SAFETY: The handle is live. GetFileType only inspects it.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(private_error());
    }
    Ok(file)
}

enum Object {
    Directory,
    File { write_new: bool },
}

fn verify_security(file: &File, sid: PSID, object: Object) -> io::Result<()> {
    let mut owner = null_mut();
    let mut acl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: The handle is live and opened with READ_CONTROL. The output
    // pointers are valid, and any allocation is released by LocalAllocation.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        )
    };
    let _descriptor = LocalAllocation(descriptor);
    check_status(status)?;
    if descriptor.is_null() || owner.is_null() || acl.is_null() {
        return Err(private_error());
    }
    // SAFETY: Both pointers were returned within the live system descriptor.
    // The process SID is valid, and each Win32 validation guards later reads.
    if unsafe { IsValidSid(owner) == 0 || EqualSid(owner, sid) == 0 || IsValidAcl(acl) == 0 } {
        return Err(private_error());
    }
    // SAFETY: IsValidAcl succeeded, so the ACL header is valid for this read.
    if unsafe { (*acl).AceCount } != 1 {
        return Err(private_error());
    }
    if matches!(object, Object::Directory) {
        let mut control = 0;
        let mut revision = 0;
        // SAFETY: The system descriptor remains live and the two outputs are valid.
        check_bool(unsafe {
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision)
        })?;
        if control & SE_DACL_PROTECTED == 0 {
            return Err(private_error());
        }
    }
    let mut ace = null_mut();
    // SAFETY: The validated ACL contains one ACE and the output pointer is valid.
    check_bool(unsafe { GetAce(acl, 0, &mut ace) })?;
    if ace.is_null() {
        return Err(private_error());
    }
    // SAFETY: GetAce returned a readable header in the live validated ACL.
    let header = unsafe { &*ace.cast::<ACE_HEADER>() };
    let flags = u32::from(header.AceFlags);
    let flags_allowed = match object {
        Object::Directory => flags == SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Object::File { write_new: true } => flags == INHERITED_ACE,
        Object::File { write_new: false } => matches!(flags, 0 | INHERITED_ACE),
    };
    let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
    let sid_bytes = usize::from(header.AceSize).saturating_sub(sid_offset);
    // A SID starts with the revision, subauthority count and authority ID.
    if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE || !flags_allowed || sid_bytes < 8 {
        return Err(private_error());
    }
    // SAFETY: The SID offset and its first two bytes were checked against AceSize.
    let trustee = unsafe { ace.cast::<u8>().add(sid_offset) };
    let subauthorities = unsafe { *trustee.add(1) };
    // SAFETY: The length check bounds the SID inside this ACE before the Windows
    // SID functions access it. The process SID remains alive for the comparison.
    let matches_user = unsafe {
        GetSidLengthRequired(subauthorities) as usize <= sid_bytes
            && IsValidSid(trustee.cast()) != 0
            && EqualSid(trustee.cast(), sid) != 0
    };
    if !matches_user {
        return Err(private_error());
    }
    Ok(())
}

fn directory_descriptor(sid: PSID) -> io::Result<(LocalAllocation, SECURITY_DESCRIPTOR)> {
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: sid.cast(),
        },
    };
    let mut acl = null_mut();
    // SAFETY: The single input entry and its SID remain live through the call.
    let status = unsafe { SetEntriesInAclW(1, &entry, null(), &mut acl) };
    let allocation = LocalAllocation(acl.cast());
    check_status(status)?;
    if acl.is_null() {
        return Err(private_error());
    }
    let mut descriptor = owner_descriptor(sid)?;
    let pointer = from_mut(&mut descriptor).cast();
    // SAFETY: The initialized descriptor and newly allocated ACL are live.
    check_bool(unsafe { SetSecurityDescriptorDacl(pointer, 1, acl, 0) })?;
    // SAFETY: The descriptor has a DACL. The protected bit blocks parent ACEs.
    check_bool(unsafe {
        SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
    })?;
    Ok((allocation, descriptor))
}

fn owner_descriptor(sid: PSID) -> io::Result<SECURITY_DESCRIPTOR> {
    let mut descriptor = SECURITY_DESCRIPTOR::default();
    let pointer = from_mut(&mut descriptor).cast();
    // SAFETY: The aligned descriptor has the size expected by Win32.
    check_bool(unsafe { InitializeSecurityDescriptor(pointer, SECURITY_DESCRIPTOR_REVISION) })?;
    // SAFETY: The descriptor is initialized and the supplied process SID is live.
    check_bool(unsafe { SetSecurityDescriptorOwner(pointer, sid, 0) })?;
    Ok(descriptor)
}

fn security_attributes(descriptor: &mut SECURITY_DESCRIPTOR) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: from_mut(descriptor).cast(),
        bInheritHandle: 0,
    }
}

#[repr(C)]
struct UserSid {
    header: TOKEN_USER,
    sid_buffer: [u8; SECURITY_MAX_SID_SIZE as usize],
}

impl UserSid {
    fn current() -> io::Result<Box<Self>> {
        let mut handle: HANDLE = null_mut();
        // SAFETY: GetCurrentProcess yields the process pseudo-handle. The API
        // writes an independently owned token handle to the valid output pointer.
        check_bool(unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle) })?;
        // SAFETY: OpenProcessToken succeeded. OwnedHandle closes only this token.
        let token = unsafe { OwnedHandle::from_raw_handle(handle) };
        let mut user = Box::new(Self {
            header: TOKEN_USER::default(),
            sid_buffer: [0; SECURITY_MAX_SID_SIZE as usize],
        });
        let mut returned = 0;
        // SAFETY: The aligned heap buffer has space for TOKEN_USER and the
        // maximum SID. It never moves while the returned internal pointer is used.
        check_bool(unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                from_mut(user.as_mut()).cast(),
                size_of::<Self>() as u32,
                &mut returned,
            )
        })?;
        // SAFETY: GetTokenInformation returned a SID pointing into the live buffer.
        if user.sid().is_null() || unsafe { IsValidSid(user.sid()) } == 0 {
            return Err(private_error());
        }
        Ok(user)
    }

    fn sid(&self) -> PSID {
        self.header.User.Sid
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The pointer came from a Win32 API that requires LocalFree.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let final_component = path.file_name().ok_or_else(private_error)?;
    if !path.is_absolute()
        || path.components().any(|part| part == Component::ParentDir)
        || final_component
            .encode_wide()
            .any(|unit| unit == u16::from(b':'))
    {
        return Err(private_error());
    }
    let mut name: Vec<_> = path.as_os_str().encode_wide().collect();
    if name.contains(&0) {
        return Err(private_error());
    }
    name.push(0);
    Ok(name)
}

fn check_bool(status: i32) -> io::Result<()> {
    if status == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn check_status(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

fn private_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "unsafe Windows control endpoint",
    )
}

pub(crate) fn bind_loopback_exclusive() -> io::Result<TcpListener> {
    let socket = tcp_socket()?;
    set_socket_option(&socket, SO_EXCLUSIVEADDRUSE)?;
    bind_loopback(&socket, 0)?;
    // SAFETY: The socket is live and bound. Winsock validates the backlog.
    check_socket(unsafe { listen(socket.as_raw_socket() as usize, 16) })?;
    Ok(TcpListener::from(socket))
}

fn tcp_socket() -> io::Result<OwnedSocket> {
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    if let Err(error) = STARTED.get_or_init(|| {
        let mut data = WSADATA::default();
        // SAFETY: data is the correctly aligned Winsock version-information buffer.
        // This process-wide reference remains active while std owns our sockets.
        let status = unsafe { WSAStartup(0x0202, &mut data) };
        if status == 0 { Ok(()) } else { Err(status) }
    }) {
        return Err(io::Error::from_raw_os_error(*error));
    }
    // SAFETY: Winsock is initialized. No protocol info or socket group is supplied.
    let socket = unsafe {
        WSASocketW(
            i32::from(AF_INET),
            SOCK_STREAM,
            IPPROTO_TCP,
            null(),
            0,
            WSA_FLAG_OVERLAPPED | WSA_FLAG_NO_HANDLE_INHERIT,
        )
    };
    if socket == INVALID_SOCKET {
        return Err(socket_error());
    }
    // SAFETY: WSASocketW returned a new handle and ownership moves into OwnedSocket.
    Ok(unsafe { OwnedSocket::from_raw_socket(socket as RawSocket) })
}

fn set_socket_option(socket: &OwnedSocket, option: i32) -> io::Result<()> {
    let enabled = 1_i32;
    // SAFETY: The live socket and correctly sized integer option are valid.
    check_socket(unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            option,
            from_ref(&enabled).cast(),
            size_of::<i32>() as i32,
        )
    })
}

fn bind_loopback(socket: &OwnedSocket, port: u16) -> io::Result<()> {
    let address = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: port.to_be(),
        sin_addr: IN_ADDR {
            S_un: IN_ADDR_0 {
                S_addr: u32::from_ne_bytes([127, 0, 0, 1]),
            },
        },
        sin_zero: [0; 8],
    };
    // SAFETY: address is a live SOCKADDR_IN with the size and family passed to bind.
    check_socket(unsafe {
        bind(
            socket.as_raw_socket() as usize,
            from_ref(&address).cast(),
            size_of::<SOCKADDR_IN>() as i32,
        )
    })
}

fn check_socket(status: i32) -> io::Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(socket_error())
    }
}

fn socket_error() -> io::Error {
    // SAFETY: Reads this thread's last Winsock error after a failed call.
    io::Error::from_raw_os_error(unsafe { WSAGetLastError() })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::Ipv4Addr;
    use std::path::PathBuf;

    use windows_sys::Win32::Networking::WinSock::{SO_REUSEADDR, WSAEACCES, WSAEADDRINUSE};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let mut random = [0; 16];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "kortty-control-{:032x}",
                u128::from_le_bytes(random)
            ));
            create_private_directory(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn private_directory_and_inherited_endpoint_support_exclusive_creation() {
        let directory = TestDirectory::new();
        create_private_directory(&directory.0).unwrap();
        verify_private_directory(&directory.0).unwrap();
        let endpoint = directory.0.join("endpoint.json");
        let mut writer = open_private_file(&endpoint, true).unwrap();
        writer.write_all(b"test descriptor").unwrap();
        drop(writer);
        assert_eq!(
            open_private_file(&endpoint, true).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let mut content = Vec::new();
        open_private_file(&endpoint, false)
            .unwrap()
            .read_to_end(&mut content)
            .unwrap();
        assert_eq!(content, b"test descriptor");
        assert_eq!(
            verify_private_directory(&endpoint).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn endpoint_open_rejects_a_file_with_an_unrestricted_dacl() {
        let directory = TestDirectory::new();
        let user = UserSid::current().unwrap();
        let mut descriptor = owner_descriptor(user.sid()).unwrap();
        let pointer = from_mut(&mut descriptor).cast();
        // SAFETY: The descriptor is live and initialized. An explicit NULL ACL
        // makes an empty, disposable test file unrestricted, with no data in it.
        check_bool(unsafe { SetSecurityDescriptorDacl(pointer, 1, null(), 0) }).unwrap();
        // SAFETY: Keep the private parent's ACL from hiding the test condition.
        check_bool(unsafe {
            SetSecurityDescriptorControl(pointer, SE_DACL_PROTECTED, SE_DACL_PROTECTED)
        })
        .unwrap();
        let file = directory.0.join("unrestricted");
        let attributes = security_attributes(&mut descriptor);
        drop(
            open_file(
                &file,
                GENERIC_WRITE,
                FILE_SHARE_READ,
                CREATE_NEW,
                Some(&attributes),
            )
            .unwrap(),
        );
        assert_eq!(
            open_private_file(&file, false).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 0);
    }

    #[test]
    fn exclusive_listener_uses_an_ephemeral_loopback_port_and_rejects_reuse() {
        let listener = bind_loopback_exclusive().unwrap();
        let address = listener.local_addr().unwrap();
        assert_eq!(address.ip(), Ipv4Addr::LOCALHOST);
        assert_ne!(address.port(), 0);
        let competing = tcp_socket().unwrap();
        set_socket_option(&competing, SO_REUSEADDR).unwrap();
        let error = bind_loopback(&competing, address.port()).unwrap_err();
        assert!(matches!(
            error.raw_os_error(),
            Some(WSAEACCES | WSAEADDRINUSE)
        ));
    }
}
