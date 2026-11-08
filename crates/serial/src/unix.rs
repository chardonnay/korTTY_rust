use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::net::Shutdown;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{FlowControl, Parity, Settings, StopBits, closed, unsupported};

pub(crate) struct Port(Arc<Shared>);

struct Shared {
    file: Mutex<Option<Arc<Device>>>,
    cancel: UnixStream,
    signal: UnixStream,
    break_lock: Mutex<()>,
}

impl Shared {
    fn file(&self) -> Option<Arc<Device>> {
        self.file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

struct Device(File);

impl Drop for Device {
    fn drop(&mut self) {
        // SAFETY: This is the last Rust reference to the still-open tty.
        // Clear exclusivity even if a linked PTY or another earlier handle
        // keeps the kernel tty structure alive after this descriptor closes.
        let _ = unsafe { libc::ioctl(self.0.as_raw_fd(), libc::TIOCNXCL as _) };
    }
}

impl Port {
    pub(crate) fn open(
        path: &Path,
        settings: Settings,
    ) -> io::Result<(Self, SerialReader, SerialWriter)> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.file_type().is_char_device() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "serial path is not a character device",
            ));
        }
        let fd = file.as_raw_fd();
        // SAFETY: The live tty fd takes no ioctl argument for exclusive mode.
        cvt(unsafe { libc::ioctl(fd, libc::TIOCEXCL as _) })?;
        let file = Device(file);
        let (cancel, signal) = UnixStream::pair()?;
        configure(fd, settings)?;
        let shared = Arc::new(Shared {
            file: Mutex::new(Some(Arc::new(file))),
            cancel,
            signal,
            break_lock: Mutex::new(()),
        });
        Ok((
            Self(Arc::clone(&shared)),
            SerialReader(Arc::clone(&shared)),
            SerialWriter(shared),
        ))
    }

    pub(crate) fn close(&self) -> io::Result<()> {
        let file = self
            .0
            .file
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        if file.is_some() {
            // EOF is a persistent readiness notification for every poller.
            self.0.signal.shutdown(Shutdown::Write)?;
        }
        Ok(())
    }

    pub(crate) fn send_break(&self, duration: Duration) -> io::Result<()> {
        let _guard = self
            .0
            .break_lock
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let file = self.0.file().ok_or_else(closed)?;
        let fd = file.0.as_raw_fd();
        // SAFETY: Both break ioctls use a live tty fd and take no argument.
        cvt(unsafe { libc::ioctl(fd, libc::TIOCSBRK as _) })?;
        let result = wait_for_break(&self.0.cancel, duration);
        // SAFETY: The same fd remains owned until the break is cleared.
        let cleared = cvt(unsafe { libc::ioctl(fd, libc::TIOCCBRK as _) });
        result.and(cleared)
    }
}

pub struct SerialReader(Arc<Shared>);

impl Read for SerialReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let Some(file) = self.0.file() else {
            return Ok(0);
        };
        loop {
            let Some(events) = ready(&self.0.cancel, &file.0, libc::POLLIN)? else {
                return Ok(0);
            };
            match (&file.0).read(output) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if events & libc::POLLHUP != 0
                        && (error.kind() == io::ErrorKind::WouldBlock
                            || error.raw_os_error() == Some(libc::EIO)) =>
                {
                    return Ok(0);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if events & libc::POLLERR != 0 {
                        return Err(io::Error::other("serial device reported an I/O error"));
                    }
                }
                result => return result,
            }
        }
    }
}

pub struct SerialWriter(Arc<Shared>);

impl Write for SerialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let file = self.0.file().ok_or_else(closed)?;
        loop {
            let events = ready(&self.0.cancel, &file.0, libc::POLLOUT)?.ok_or_else(closed)?;
            if events & libc::POLLHUP != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "serial device disconnected",
                ));
            }
            match (&file.0).write(bytes) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if events & libc::POLLERR != 0 {
                        return Err(io::Error::other("serial device reported an I/O error"));
                    }
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // A successful write already submitted the bytes to the driver.
        // tcdrain can block indefinitely while hardware flow control is stopped.
        Ok(())
    }
}

fn ready(
    cancel: &UnixStream,
    file: &File,
    events: libc::c_short,
) -> io::Result<Option<libc::c_short>> {
    let mut fds = [
        libc::pollfd {
            fd: cancel.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: file.as_raw_fd(),
            events,
            revents: 0,
        },
    ];
    loop {
        // SAFETY: The array contains two initialized, live file descriptors.
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[0].revents != 0 {
            return Ok(None);
        }
        if fds[1].revents & libc::POLLNVAL != 0 {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if fds[1].revents != 0 {
            return Ok(Some(fds[1].revents));
        }
    }
}

fn wait_for_break(cancel: &UnixStream, duration: Duration) -> io::Result<()> {
    let deadline = Instant::now() + duration;
    let mut fd = libc::pollfd {
        fd: cancel.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let timeout = remaining.as_millis().max(1) as libc::c_int;
        // SAFETY: fd points to a single initialized poll descriptor.
        let result = unsafe { libc::poll(&mut fd, 1, timeout) };
        if result > 0 {
            return Err(closed());
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
    Ok(())
}

pub(crate) fn validate(path: &Path, settings: Settings) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "serial device path must be absolute",
        ));
    }
    if settings.stop_bits == StopBits::OnePointFive {
        return Err(unsupported(
            "POSIX termios has no portable 1.5-stop-bit setting",
        ));
    }
    #[cfg(target_os = "linux")]
    if settings.flow_control == FlowControl::DsrDtr {
        return Err(unsupported("Linux termios has no DSR/DTR flow control"));
    }
    #[cfg(target_os = "macos")]
    if matches!(settings.parity, Parity::Mark | Parity::Space) {
        return Err(unsupported(
            "macOS termios has no mark/space parity setting",
        ));
    }
    Ok(())
}

fn configure(fd: RawFd, settings: Settings) -> io::Result<()> {
    let original = termios(fd)?;
    let mut line = original;
    // SAFETY: line is an initialized termios struct obtained from this fd.
    unsafe { libc::cfmakeraw(&mut line) };
    line.c_cflag &= !(libc::CSIZE | libc::PARENB | libc::PARODD | libc::CSTOPB | libc::CRTSCTS);
    #[cfg(target_os = "linux")]
    {
        line.c_cflag &= !libc::CMSPAR;
    }
    #[cfg(target_os = "macos")]
    {
        line.c_cflag &= !MAC_DSR_DTR;
    }
    line.c_cflag |= libc::CLOCAL
        | libc::CREAD
        | match settings.data_bits {
            5 => libc::CS5,
            6 => libc::CS6,
            7 => libc::CS7,
            _ => libc::CS8,
        };
    line.c_cflag |= match settings.parity {
        Parity::None => 0,
        Parity::Even => libc::PARENB,
        Parity::Odd => libc::PARENB | libc::PARODD,
        #[cfg(target_os = "linux")]
        Parity::Mark => libc::PARENB | libc::PARODD | libc::CMSPAR,
        #[cfg(target_os = "linux")]
        Parity::Space => libc::PARENB | libc::CMSPAR,
        #[cfg(target_os = "macos")]
        Parity::Mark | Parity::Space => return Err(unsupported("macOS mark/space parity")),
    };
    if settings.stop_bits == StopBits::Two {
        line.c_cflag |= libc::CSTOPB;
    }
    if settings.flow_control == FlowControl::RtsCts {
        line.c_cflag |= libc::CRTSCTS;
    }
    #[cfg(target_os = "macos")]
    if settings.flow_control == FlowControl::DsrDtr {
        line.c_cflag |= MAC_DSR_DTR;
    }
    line.c_iflag &=
        !(libc::IXON | libc::IXOFF | libc::IXANY | libc::INPCK | libc::IGNPAR | libc::PARMRK);
    if settings.parity != Parity::None {
        line.c_iflag |= libc::INPCK;
    }
    if settings.flow_control == FlowControl::XonXoff {
        line.c_iflag |= libc::IXON | libc::IXOFF;
        line.c_cc[libc::VSTART] = 0x11;
        line.c_cc[libc::VSTOP] = 0x13;
    }
    line.c_cc[libc::VMIN] = 1;
    line.c_cc[libc::VTIME] = 0;

    let configured = set_speed(fd, &mut line, settings.baud_rate)
        .and_then(|()| verify_line(fd, &line))
        .and_then(|()| set_modem_lines(fd, settings));
    if configured.is_err() {
        // SAFETY: Restore an initialized snapshot taken from the same live fd.
        let _ = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    }
    configured
}

#[cfg(target_os = "macos")]
const MAC_DSR_DTR: libc::tcflag_t = 0x0004_0000 | 0x0008_0000; // sys/termios.h CDTR_IFLOW | CDSR_OFLOW

fn termios(fd: RawFd) -> io::Result<libc::termios> {
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: tcgetattr initializes the output on success.
    cvt(unsafe { libc::tcgetattr(fd, termios.as_mut_ptr()) })?;
    // SAFETY: Success above guarantees that the structure was initialized.
    Ok(unsafe { termios.assume_init() })
}

fn verify_line(fd: RawFd, requested: &libc::termios) -> io::Result<()> {
    let actual = termios(fd)?;
    let mask = libc::CSIZE | libc::PARENB | libc::PARODD | libc::CSTOPB | libc::CRTSCTS;
    #[cfg(target_os = "linux")]
    let mask = mask | libc::CMSPAR;
    #[cfg(target_os = "macos")]
    let mask = mask | MAC_DSR_DTR;
    let flow = libc::IXON | libc::IXOFF;
    if actual.c_cflag & mask != requested.c_cflag & mask
        || actual.c_iflag & flow != requested.c_iflag & flow
    {
        return Err(unsupported(
            "serial driver did not retain the requested framing or flow control",
        ));
    }
    Ok(())
}

fn apply_termios(fd: RawFd, line: &mut libc::termios, speed: libc::speed_t) -> io::Result<()> {
    // SAFETY: Both functions accept an initialized termios struct and a live tty fd.
    cvt(unsafe { libc::cfsetspeed(line, speed) })?;
    cvt(unsafe { libc::tcsetattr(fd, libc::TCSANOW, line) })
}

#[cfg(target_os = "macos")]
fn set_speed(fd: RawFd, line: &mut libc::termios, baud: u32) -> io::Result<()> {
    if matches!(
        baud,
        50 | 75
            | 110
            | 134
            | 150
            | 200
            | 300
            | 600
            | 1200
            | 1800
            | 2400
            | 4800
            | 9600
            | 19200
            | 38400
            | 57600
            | 115200
            | 230400
    ) {
        return apply_termios(fd, line, libc::speed_t::from(baud));
    }
    apply_termios(fd, line, libc::B9600)?;
    // IOKit/serial/ioss.h defines IOSSIOSPEED as _IOW('T', 2, speed_t).
    const IOSSIOSPEED: libc::c_ulong = 0x8000_0000
        | ((size_of::<libc::speed_t>() as libc::c_ulong) << 16)
        | ((b'T' as libc::c_ulong) << 8)
        | 2;
    let mut speed = libc::speed_t::from(baud);
    // SAFETY: The ioctl reads a speed_t from the live value at this address.
    cvt(unsafe { libc::ioctl(fd, IOSSIOSPEED, &mut speed) })
}

#[cfg(target_os = "linux")]
fn set_speed(fd: RawFd, line: &mut libc::termios, baud: u32) -> io::Result<()> {
    let speed = match baud {
        50 => libc::B50,
        75 => libc::B75,
        110 => libc::B110,
        134 => libc::B134,
        150 => libc::B150,
        200 => libc::B200,
        300 => libc::B300,
        600 => libc::B600,
        1200 => libc::B1200,
        1800 => libc::B1800,
        2400 => libc::B2400,
        4800 => libc::B4800,
        9600 => libc::B9600,
        19200 => libc::B19200,
        38400 => libc::B38400,
        57600 => libc::B57600,
        115200 => libc::B115200,
        230400 => libc::B230400,
        460800 => libc::B460800,
        500000 => libc::B500000,
        576000 => libc::B576000,
        921600 => libc::B921600,
        1000000 => libc::B1000000,
        1152000 => libc::B1152000,
        1500000 => libc::B1500000,
        2000000 => libc::B2000000,
        _ => {
            apply_termios(fd, line, libc::B38400)?;
            return set_linux_custom_speed(fd, baud);
        }
    };
    apply_termios(fd, line, speed)
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "powerpc", target_arch = "powerpc64"))
))]
fn set_linux_custom_speed(fd: RawFd, baud: u32) -> io::Result<()> {
    let mut extended = MaybeUninit::<libc::termios2>::uninit();
    // SAFETY: The kernel initializes the termios2 structure on success.
    cvt(unsafe { libc::ioctl(fd, libc::TCGETS2, extended.as_mut_ptr()) })?;
    // SAFETY: TCGETS2 succeeded above.
    let mut extended = unsafe { extended.assume_init() };
    // A cleared input baud selection uses the configured output rate.
    extended.c_cflag = (extended.c_cflag & !(libc::CBAUD | libc::CIBAUD)) | libc::BOTHER;
    extended.c_ispeed = baud;
    extended.c_ospeed = baud;
    // SAFETY: TCSETS2 reads the initialized extended settings of this tty.
    cvt(unsafe { libc::ioctl(fd, libc::TCSETS2, &extended) })?;
    // SAFETY: The same live fd can overwrite the initialized structure.
    cvt(unsafe { libc::ioctl(fd, libc::TCGETS2, &mut extended) })?;
    if extended.c_ispeed != baud || extended.c_ospeed != baud {
        return Err(unsupported(
            "serial driver rounded the requested custom baud rate",
        ));
    }
    Ok(())
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "powerpc", target_arch = "powerpc64")
))]
fn set_linux_custom_speed(_: RawFd, _: u32) -> io::Result<()> {
    Err(unsupported(
        "custom baud rates need a Linux termios2 device",
    ))
}

fn set_modem_lines(fd: RawFd, settings: Settings) -> io::Result<()> {
    for (flag, requested, controlled) in [
        (
            libc::TIOCM_DTR,
            settings.assert_dtr,
            settings.flow_control == FlowControl::DsrDtr,
        ),
        (
            libc::TIOCM_RTS,
            settings.assert_rts,
            settings.flow_control == FlowControl::RtsCts,
        ),
    ] {
        if controlled {
            continue;
        }
        let request = if requested {
            libc::TIOCMBIS
        } else {
            libc::TIOCMBIC
        };
        // SAFETY: The ioctl reads an initialized int bit mask for the live fd.
        let result = cvt(unsafe { libc::ioctl(fd, request as _, &flag) });
        if let Err(error) = result {
            if matches!(
                error.raw_os_error(),
                Some(libc::ENOTTY | libc::EOPNOTSUPP | libc::EINVAL)
            ) {
                // PTYs and some real adapters have no modem signals. Defaults
                // are best-effort there, but never claim explicit deassertion.
                if !requested {
                    return Err(unsupported(
                        "serial device does not support modem line changes",
                    ));
                }
            } else {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn cvt(value: libc::c_int) -> io::Result<()> {
    if value < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn available_ports() -> io::Result<Vec<PathBuf>> {
    #[cfg(target_os = "macos")]
    let mut ports: Vec<_> = fs::read_dir("/dev")?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let eligible = name.starts_with("cu.")
                || name
                    .strip_prefix("tty.")
                    .is_some_and(|suffix| !Path::new("/dev").join(format!("cu.{suffix}")).exists());
            eligible
                .then(|| entry.path())
                .filter(|path| is_character_device(path))
        })
        .collect();
    #[cfg(target_os = "linux")]
    let mut ports: Vec<_> = fs::read_dir("/sys/class/tty")?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("device").exists())
        .map(|entry| Path::new("/dev").join(entry.file_name()))
        .filter(|path| is_character_device(path))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    Ok(ports)
}

fn is_character_device(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_char_device())
}
