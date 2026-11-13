use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr::{NonNull, null, null_mut};

use libloading::Library;

use super::escape_markup;

const SERVICE: &CStr = c"org.freedesktop.Notifications";
const PATH: &CStr = c"/org/freedesktop/Notifications";
const SIGNALS: &CStr = c"type='signal',sender='org.freedesktop.Notifications',interface='org.freedesktop.Notifications',path='/org/freedesktop/Notifications'";
const CALL_TIMEOUT_MS: c_int = 1500;
const ARRAY: c_int = b'a' as c_int;
const BOOLEAN: c_int = b'b' as c_int;
const DICT_ENTRY: c_int = b'e' as c_int;
const STRING: c_int = b's' as c_int;
const UINT32: c_int = b'u' as c_int;
const INT32: c_int = b'i' as c_int;
const INT64: c_int = b'x' as c_int;
const VARIANT: c_int = b'v' as c_int;
const INVALID: c_int = 0;

type Pointer = *mut c_void;

// Public libdbus-1 DBusMessageIter storage. Only libdbus interprets its fields.
#[repr(C)]
struct Iter {
    first: [Pointer; 2],
    stamp: u32,
    numbers: [c_int; 9],
    last: [Pointer; 2],
}

impl Iter {
    fn empty() -> Self {
        Self {
            first: [null_mut(); 2],
            stamp: 0,
            numbers: [0; 9],
            last: [null_mut(); 2],
        }
    }
}

macro_rules! dbus_api {
    ($($symbol:ident: $signature:ty),+ $(,)?) => {
        pub(super) struct Api {
            $($symbol: $signature,)+
            _library: Library,
        }

        impl Api {
            pub(super) fn load() -> Option<Self> {
                // SAFETY: Open the installed system library and retain it for every symbol use.
                let library = unsafe { Library::new("libdbus-1.so.3") }.ok()?;
                // SAFETY: These signatures are from the public libdbus-1 API.
                let api = unsafe {
                    Self {
                        $($symbol: *library.get(concat!("dbus_", stringify!($symbol), "\0").as_bytes()).ok()?,)+
                        _library: library,
                    }
                };
                // SAFETY: libdbus permits repeated thread initialization.
                (unsafe { (api.threads_init_default)() } != 0).then_some(api)
            }
        }
    };
}

dbus_api! {
    threads_init_default: unsafe extern "C" fn() -> c_uint,
    bus_get_private: unsafe extern "C" fn(c_int, Pointer) -> Pointer,
    bus_add_match: unsafe extern "C" fn(Pointer, *const c_char, Pointer),
    connection_set_exit_on_disconnect: unsafe extern "C" fn(Pointer, c_uint),
    connection_read_write: unsafe extern "C" fn(Pointer, c_int) -> c_uint,
    connection_pop_message: unsafe extern "C" fn(Pointer) -> Pointer,
    connection_send: unsafe extern "C" fn(Pointer, Pointer, *mut u32) -> c_uint,
    connection_get_outgoing_size: unsafe extern "C" fn(Pointer) -> std::ffi::c_long,
    connection_send_with_reply_and_block: unsafe extern "C" fn(Pointer, Pointer, c_int, Pointer) -> Pointer,
    connection_close: unsafe extern "C" fn(Pointer),
    connection_unref: unsafe extern "C" fn(Pointer),
    message_new_method_call: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char, *const c_char) -> Pointer,
    message_new_signal: unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> Pointer,
    message_unref: unsafe extern "C" fn(Pointer),
    message_get_sender: unsafe extern "C" fn(Pointer) -> *const c_char,
    message_has_path: unsafe extern "C" fn(Pointer, *const c_char) -> c_uint,
    message_has_signature: unsafe extern "C" fn(Pointer, *const c_char) -> c_uint,
    message_is_signal: unsafe extern "C" fn(Pointer, *const c_char, *const c_char) -> c_uint,
    message_get_args: unsafe extern "C" fn(Pointer, Pointer, c_int, ...) -> c_uint,
    free_string_array: unsafe extern "C" fn(*mut *mut c_char),
    message_iter_init_append: unsafe extern "C" fn(Pointer, *mut Iter),
    message_iter_append_basic: unsafe extern "C" fn(*mut Iter, c_int, *const c_void) -> c_uint,
    message_iter_open_container: unsafe extern "C" fn(*mut Iter, c_int, *const c_char, *mut Iter) -> c_uint,
    message_iter_close_container: unsafe extern "C" fn(*mut Iter, *mut Iter) -> c_uint,
    message_iter_abandon_container_if_open: unsafe extern "C" fn(*mut Iter, *mut Iter),
}

#[derive(Eq, PartialEq)]
pub(super) struct Notification {
    pub(super) sender: CString,
    pub(super) id: u32,
}

pub(super) enum Signal<'a> {
    Token(&'a str),
    Activated,
    Closed,
}

pub(super) struct Bus<'a> {
    api: &'a Api,
    handle: NonNull<c_void>,
}

impl Api {
    pub(super) fn connect(&self) -> Option<Bus<'_>> {
        let bus = self.connect_private()?;
        // SAFETY: The connection is live and the match rule is a static C string.
        unsafe { (self.bus_add_match)(bus.handle.as_ptr(), SIGNALS.as_ptr(), null_mut()) };
        Some(bus)
    }

    pub(super) fn connect_private(&self) -> Option<Bus<'_>> {
        // SAFETY: DBUS_BUS_SESSION is zero. Error storage is optional. This
        // connection is private and will be closed and unreferenced by Bus.
        let handle = NonNull::new(unsafe { (self.bus_get_private)(0, null_mut()) })?;
        // SAFETY: The handle is live and belongs to this private connection.
        unsafe { (self.connection_set_exit_on_disconnect)(handle.as_ptr(), 0) };
        Some(Bus { api: self, handle })
    }
}

impl<'a> Bus<'a> {
    pub(super) fn launcher_badge(&self, path: &CStr, uri: &CStr, count: i64, urgent: bool) -> bool {
        // SAFETY: The object path and two static strings are valid D-Bus names.
        let raw = unsafe {
            (self.api.message_new_signal)(
                path.as_ptr(),
                c"com.canonical.Unity.LauncherEntry".as_ptr(),
                c"Update".as_ptr(),
            )
        };
        let Some(message) = Message::new(self.api, raw) else {
            return false;
        };
        let mut iter = Iter::empty();
        // SAFETY: The new message is writable and libdbus initializes the iterator.
        unsafe { (self.api.message_iter_init_append)(raw, &mut iter) };
        let complete = self.append_string(&mut iter, uri)
            && self.append_array(&mut iter, c"{sv}", |items| {
                self.append_property(items, c"count", c"x", INT64, &count)
                    && self.append_property(
                        items,
                        c"count-visible",
                        c"b",
                        BOOLEAN,
                        &u32::from(count > 0),
                    )
                    && self.append_property(
                        items,
                        c"urgent",
                        c"b",
                        BOOLEAN,
                        &u32::from(urgent && count > 0),
                    )
            });
        // SAFETY: Both handles remain live. libdbus retains its own queued reference.
        if !complete
            || unsafe {
                (self.api.connection_send)(
                    self.handle.as_ptr(),
                    message.handle.as_ptr(),
                    null_mut(),
                )
            } == 0
        {
            return false;
        }
        let until = std::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            if !self.read(10) {
                return false;
            }
            // SAFETY: The handle remains valid for the duration of this call.
            if unsafe { (self.api.connection_get_outgoing_size)(self.handle.as_ptr()) } == 0 {
                return true;
            }
            if std::time::Instant::now() >= until {
                return false;
            }
        }
    }

    pub(super) fn notify(
        &self,
        title: &str,
        body: &str,
        still_allowed: impl Fn() -> bool,
    ) -> Option<Notification> {
        let capabilities = self.call(SERVICE, c"GetCapabilities", |_| true)?;
        let sender = capabilities.sender()?;
        let markup = capabilities.body_markup()?;
        let title = CString::new(title).ok()?;
        let body = if markup {
            CString::new(escape_markup(body)).ok()?
        } else {
            CString::new(body).ok()?
        };
        let reply = self.call(sender, c"Notify", |request| {
            let mut iter = Iter::empty();
            // SAFETY: This message is new and writable. The iterator storage
            // is initialized by libdbus before appending the specified fields.
            unsafe { (self.api.message_iter_init_append)(request, &mut iter) };
            self.append_string(&mut iter, c"kortty")
                && self.append_basic(&mut iter, UINT32, &0_u32)
                && self.append_string(&mut iter, c"utilities-terminal")
                && self.append_string(&mut iter, &title)
                && self.append_string(&mut iter, &body)
                && self.append_array(&mut iter, c"s", |items| {
                    self.append_string(items, c"default")
                        && self.append_string(items, c"Open terminal")
                })
                && self.append_array(&mut iter, c"{sv}", |_| true)
                && self.append_basic(&mut iter, INT32, &-1_i32)
                && still_allowed()
        })?;
        if reply.sender()? != sender || !reply.signature(c"u") {
            return None;
        }
        let id = reply.id()?;
        (id != 0).then(|| Notification {
            sender: sender.to_owned(),
            id,
        })
    }

    fn call(
        &self,
        destination: &CStr,
        method: &CStr,
        append: impl FnOnce(Pointer) -> bool,
    ) -> Option<Message<'a>> {
        // SAFETY: All arguments are valid C strings and the returned message
        // is transferred into a reference-counted RAII wrapper.
        let request = unsafe {
            (self.api.message_new_method_call)(
                destination.as_ptr(),
                PATH.as_ptr(),
                SERVICE.as_ptr(),
                method.as_ptr(),
            )
        };
        let request = Message::new(self.api, request)?;
        if !append(request.handle.as_ptr()) {
            return None;
        }
        // SAFETY: Both handles remain valid for the synchronous call. Error
        // storage is optional, and a non-null result transfers one reference.
        let reply = unsafe {
            (self.api.connection_send_with_reply_and_block)(
                self.handle.as_ptr(),
                request.handle.as_ptr(),
                CALL_TIMEOUT_MS,
                null_mut(),
            )
        };
        Message::new(self.api, reply)
    }

    fn append_string(&self, iter: &mut Iter, value: &CStr) -> bool {
        self.append_basic(iter, STRING, &value.as_ptr())
    }

    fn append_basic<T>(&self, iter: &mut Iter, kind: c_int, value: &T) -> bool {
        // SAFETY: Private callers pass an initialized writer and the exact C
        // storage type for each basic type code. libdbus copies the value.
        unsafe { (self.api.message_iter_append_basic)(iter, kind, (value as *const T).cast()) != 0 }
    }

    fn append_array(
        &self,
        iter: &mut Iter,
        signature: &CStr,
        append: impl FnOnce(&mut Iter) -> bool,
    ) -> bool {
        self.append_container(iter, ARRAY, Some(signature), append)
    }

    fn append_property<T>(
        &self,
        iter: &mut Iter,
        name: &CStr,
        signature: &CStr,
        kind: c_int,
        value: &T,
    ) -> bool {
        self.append_container(iter, DICT_ENTRY, None, |entry| {
            self.append_string(entry, name)
                && self.append_container(entry, VARIANT, Some(signature), |variant| {
                    self.append_basic(variant, kind, value)
                })
        })
    }

    fn append_container(
        &self,
        iter: &mut Iter,
        kind: c_int,
        signature: Option<&CStr>,
        append: impl FnOnce(&mut Iter) -> bool,
    ) -> bool {
        let mut items = Iter::empty();
        let signature = signature.map_or(null(), CStr::as_ptr);
        // SAFETY: The parent is an initialized writer and the child belongs
        // only to this matching open/close pair.
        if unsafe { (self.api.message_iter_open_container)(iter, kind, signature, &mut items) } == 0
        {
            return false;
        }
        let complete = append(&mut items)
            && unsafe { (self.api.message_iter_close_container)(iter, &mut items) } != 0;
        // SAFETY: Safe for both a still-open container and a closed iterator.
        unsafe { (self.api.message_iter_abandon_container_if_open)(iter, &mut items) };
        complete
    }

    pub(super) fn read(&self, timeout_ms: c_int) -> bool {
        // SAFETY: The worker exclusively owns this live connection.
        unsafe { (self.api.connection_read_write)(self.handle.as_ptr(), timeout_ms) != 0 }
    }

    pub(super) fn pop(&self) -> Option<Message<'a>> {
        // SAFETY: A popped message transfers one reference to the caller.
        Message::new(self.api, unsafe {
            (self.api.connection_pop_message)(self.handle.as_ptr())
        })
    }
}

impl Drop for Bus<'_> {
    fn drop(&mut self) {
        // SAFETY: This is our one owned private connection reference.
        unsafe {
            (self.api.connection_close)(self.handle.as_ptr());
            (self.api.connection_unref)(self.handle.as_ptr());
        }
    }
}

pub(super) struct Message<'a> {
    api: &'a Api,
    handle: NonNull<c_void>,
}

impl<'a> Message<'a> {
    fn new(api: &'a Api, handle: Pointer) -> Option<Self> {
        Some(Self {
            api,
            handle: NonNull::new(handle)?,
        })
    }

    fn sender(&self) -> Option<&CStr> {
        // SAFETY: The returned sender, when present, is borrowed from this message.
        let sender = unsafe { (self.api.message_get_sender)(self.handle.as_ptr()) };
        if sender.is_null() {
            return None;
        }
        let sender = unsafe { CStr::from_ptr(sender) };
        sender.to_bytes().starts_with(b":").then_some(sender)
    }

    fn signature(&self, signature: &CStr) -> bool {
        // SAFETY: The message and NUL-terminated signature are live.
        unsafe { (self.api.message_has_signature)(self.handle.as_ptr(), signature.as_ptr()) != 0 }
    }

    fn id(&self) -> Option<u32> {
        if !self.signature(c"u") && !self.signature(c"uu") {
            return None;
        }
        let mut id = 0_u32;
        // SAFETY: The validated signature starts with uint32, and id is writable.
        (unsafe {
            (self.api.message_get_args)(
                self.handle.as_ptr(),
                null_mut(),
                UINT32,
                &raw mut id,
                INVALID,
            )
        } != 0)
            .then_some(id)
    }

    fn id_and_text(&self) -> Option<(u32, &str)> {
        if !self.signature(c"us") {
            return None;
        }
        let (mut id, mut value) = (0_u32, null::<c_char>());
        // SAFETY: Each output matches the validated argument signature.
        let read = unsafe {
            (self.api.message_get_args)(
                self.handle.as_ptr(),
                null_mut(),
                UINT32,
                &raw mut id,
                STRING,
                &raw mut value,
                INVALID,
            )
        };
        if read == 0 || value.is_null() {
            return None;
        }
        // SAFETY: The pointer is to the string argument borrowed from this message.
        Some((id, unsafe { CStr::from_ptr(value) }.to_str().ok()?))
    }

    fn body_markup(&self) -> Option<bool> {
        if !self.signature(c"as") {
            return None;
        }
        let (mut values, mut count) = (null_mut::<*mut c_char>(), 0_i32);
        // SAFETY: For a string array libdbus fills a char*** and a signed count.
        let read = unsafe {
            (self.api.message_get_args)(
                self.handle.as_ptr(),
                null_mut(),
                ARRAY,
                STRING,
                &raw mut values,
                &raw mut count,
                INVALID,
            )
        };
        if read == 0 || values.is_null() {
            return None;
        }
        // SAFETY: The returned array contains count valid C strings and a
        // trailing null pointer. libdbus allocated the array and its strings.
        let markup = unsafe {
            let markup = (0..count)
                .any(|index| CStr::from_ptr(*values.add(index as usize)) == c"body-markup");
            (self.api.free_string_array)(values);
            markup
        };
        Some(markup)
    }

    pub(super) fn signal(&self) -> Option<(&CStr, u32, Signal<'_>)> {
        let handle = self.handle.as_ptr();
        // SAFETY: The message is live and the path and interface names are static.
        if unsafe { (self.api.message_has_path)(handle, PATH.as_ptr()) } == 0 {
            return None;
        }
        let sender = self.sender()?;
        let is = |name: &CStr| unsafe { (self.api.message_is_signal)(handle, SERVICE.as_ptr(), name.as_ptr()) } != 0;
        if is(c"ActivationToken") {
            let (id, token) = self.id_and_text()?;
            Some((sender, id, Signal::Token(token)))
        } else if is(c"ActionInvoked") {
            let (id, action) = self.id_and_text()?;
            (action == "default").then_some((sender, id, Signal::Activated))
        } else if is(c"NotificationClosed") && self.signature(c"uu") {
            Some((sender, self.id()?, Signal::Closed))
        } else {
            None
        }
    }
}

impl Drop for Message<'_> {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns one reference to a live message.
        unsafe { (self.api.message_unref)(self.handle.as_ptr()) };
    }
}
