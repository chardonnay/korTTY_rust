#![cfg(any(target_os = "macos", target_os = "linux"))]

use std::ffi::{CStr, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kortty_serial::{SerialPort, Settings, StopBits};

const TIMEOUT: Duration = Duration::from_secs(4);

#[test]
fn configures_raw_framing_and_transfers_exact_bytes_in_both_directions() {
    let (mut peer, slave, name) = pair().unwrap();
    let mut port = SerialPort::open(
        &name,
        Settings {
            baud_rate: 115200,
            ..Settings::default()
        },
    )
    .unwrap();
    let mut termios = MaybeUninit::<libc::termios>::uninit();
    // SAFETY: slave remains open, and tcgetattr initializes termios on success.
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), termios.as_mut_ptr()) },
        0
    );
    // SAFETY: tcgetattr succeeded.
    let termios = unsafe { termios.assume_init() };
    assert_eq!(termios.c_cflag & libc::CSIZE, libc::CS8);
    assert_eq!(
        termios.c_cflag & (libc::PARENB | libc::CSTOPB | libc::CRTSCTS),
        0
    );
    assert_eq!(
        termios.c_iflag & (libc::IXON | libc::IXOFF | libc::ICRNL),
        0
    );
    assert_eq!(termios.c_lflag & (libc::ICANON | libc::ECHO), 0);
    assert_eq!(termios.c_oflag & libc::OPOST, 0);
    // SAFETY: termios contains initialized settings from the fixture device.
    assert_eq!(unsafe { libc::cfgetospeed(&termios) }, libc::B115200);
    drop(slave);

    let mut reader = port.take_reader().unwrap();
    let mut writer = port.take_writer().unwrap();
    assert!(port.take_reader().is_none());
    assert!(port.take_writer().is_none());

    let input = [b'\r', b'\n', 0, 0xff, 0x11, 0x13, b'X'];
    let (send, receive) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut actual = [0; 7];
        send.send(reader.read_exact(&mut actual).map(|()| actual))
            .unwrap();
    });
    peer.write_all(&input).unwrap();
    assert_eq!(receive.recv_timeout(TIMEOUT).unwrap().unwrap(), input);
    worker.join().unwrap();

    let output = [0xff, 0, b'\r', b'\n', b'Y'];
    writer.write_all(&output).unwrap();
    let mut actual = [0; 5];
    read_peer(&mut peer, &mut actual).unwrap();
    assert_eq!(actual, output);
}

#[test]
fn close_interrupts_read_and_backpressured_write() {
    let (_peer, slave, name) = pair().unwrap();
    drop(slave);
    let mut port = SerialPort::open(name, Settings::default()).unwrap();
    let mut reader = port.take_reader().unwrap();
    let mut writer = port.take_writer().unwrap();
    let (started_read, ready_read) = mpsc::channel();
    let (started_write, ready_write) = mpsc::channel();
    let (read_end, wait_read) = mpsc::channel();
    let (write_end, wait_write) = mpsc::channel();
    let reading = thread::spawn(move || {
        started_read.send(()).unwrap();
        read_end.send(reader.read(&mut [0; 32])).unwrap();
    });
    let writing = thread::spawn(move || {
        let data = [0; 4096];
        writer.write_all(&[0]).unwrap();
        started_write.send(()).unwrap();
        let result = loop {
            if let Err(error) = writer.write_all(&data) {
                break error.kind();
            }
        };
        write_end.send(result).unwrap();
    });

    ready_read.recv_timeout(TIMEOUT).unwrap();
    ready_write.recv_timeout(TIMEOUT).unwrap();
    thread::sleep(Duration::from_millis(25));
    assert!(wait_read.try_recv().is_err());
    assert!(wait_write.try_recv().is_err());
    port.close().unwrap();
    port.close().unwrap();
    assert_eq!(wait_read.recv_timeout(TIMEOUT).unwrap().unwrap(), 0);
    assert_eq!(
        wait_write.recv_timeout(TIMEOUT).unwrap(),
        io::ErrorKind::BrokenPipe
    );
    reading.join().unwrap();
    writing.join().unwrap();
}

#[test]
fn closing_the_peer_reports_end_of_stream() {
    let (peer, slave, name) = pair().unwrap();
    drop(slave);
    let mut port = SerialPort::open(name, Settings::default()).unwrap();
    let mut reader = port.take_reader().unwrap();
    let (done, wait) = mpsc::channel();
    let worker = thread::spawn(move || {
        done.send(reader.read(&mut [0; 8])).unwrap();
    });
    drop(peer);
    assert_eq!(wait.recv_timeout(TIMEOUT).unwrap().unwrap(), 0);
    worker.join().unwrap();
}

#[test]
fn closed_idle_endpoints_do_not_keep_the_device_open() {
    let (_peer, slave, name) = pair().unwrap();
    drop(slave);
    let mut port = SerialPort::open(&name, Settings::default()).unwrap();
    let mut reader = port.take_reader().unwrap();
    let mut writer = port.take_writer().unwrap();
    port.close().unwrap();
    let _replacement = SerialPort::open(name, Settings::default()).unwrap();
    assert_eq!(reader.read(&mut [0]).unwrap(), 0);
    assert_eq!(
        writer.write(&[0]).unwrap_err().kind(),
        io::ErrorKind::BrokenPipe
    );
}

#[test]
fn rejects_invalid_paths_and_unsupported_framing_before_open() {
    assert_eq!(
        SerialPort::open("relative-device", Settings::default())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        SerialPort::open(
            "/dev/nonexistent",
            Settings {
                baud_rate: 0,
                ..Settings::default()
            }
        )
        .err()
        .unwrap()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        SerialPort::open(
            "/dev/nonexistent",
            Settings {
                stop_bits: StopBits::OnePointFive,
                ..Settings::default()
            }
        )
        .err()
        .unwrap()
        .kind(),
        io::ErrorKind::Unsupported
    );
    assert!(SerialPort::open("/dev/null", Settings::default()).is_err());
}

#[cfg(all(
    target_os = "linux",
    not(any(target_arch = "powerpc", target_arch = "powerpc64"))
))]
#[test]
fn linux_termios2_preserves_an_exact_custom_baud_rate() {
    let (_peer, slave, name) = pair().unwrap();
    let _port = SerialPort::open(
        name,
        Settings {
            baud_rate: 123_457,
            ..Settings::default()
        },
    )
    .unwrap();
    let mut settings = MaybeUninit::<libc::termios2>::uninit();
    // SAFETY: The live fixture slave's ioctl initializes settings on success.
    assert_eq!(
        unsafe { libc::ioctl(slave.as_raw_fd(), libc::TCGETS2, settings.as_mut_ptr()) },
        0
    );
    // SAFETY: TCGETS2 succeeded.
    let settings = unsafe { settings.assume_init() };
    assert_eq!(settings.c_ispeed, 123_457);
    assert_eq!(settings.c_ospeed, 123_457);
}

fn pair() -> io::Result<(File, File, PathBuf)> {
    let mut master = -1;
    let mut slave = -1;
    // This test fixture uses a real OS pseudo-terminal pair in place of a
    // serial cable. Production opens only the caller's selected device.
    // SAFETY: openpty writes the two fds. Other optional arguments are null.
    if unsafe { libc::openpty(&mut master, &mut slave, null_mut(), null_mut(), null_mut()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Successful openpty returned two new owned descriptors.
    let (master, slave) = unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave)) };
    let mut name = [0; 1024];
    // SAFETY: The buffer and the slave descriptor remain live through ttyname_r.
    let result = unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    // SAFETY: A successful ttyname_r returned a NUL-terminated pathname.
    let name = unsafe { CStr::from_ptr(name.as_ptr()) };
    Ok((
        master,
        slave,
        PathBuf::from(OsStr::from_bytes(name.to_bytes())),
    ))
}

fn read_peer(peer: &mut File, mut bytes: &mut [u8]) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    while !bytes.is_empty() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(io::ErrorKind::TimedOut)?;
        let mut fd = libc::pollfd {
            fd: peer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd points to one initialized, live poll descriptor.
        let ready = unsafe { libc::poll(&mut fd, 1, remaining.as_millis().max(1) as i32) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let count = peer.read(bytes)?;
        if count == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        bytes = &mut bytes[count..];
    }
    Ok(())
}
