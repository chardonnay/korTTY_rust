# Native serial transport

`SerialPort::open` takes a Unix device path or Windows COM name and applies
`Settings`. The default is 9600 baud, 8 data bits, no parity, 1 stop bit,
no flow control, and DTR and RTS asserted where the device exposes them.
`available_ports` lists OS device identities. macOS prefers the `/dev/cu.*`
name when both call-in and callout paths are present. Linux uses sysfs, and
Windows uses GetCommPorts. An explicit Unix path can also use a stable device
symlink.

Move the reader and writer to separate workers, keep the owning port in the
session, and call `close` before joining the workers. A read returns EOF on
closure and a pending write returns a broken-pipe error. `flush` leaves
delivery to the driver once the bytes have been submitted. The break duration
used by the Java connector is 250 ms.

## Platform boundaries

- Linux uses termios and termios2. Mark and space parity depend on the driver.
  Linux termios provides no DSR/DTR hardware flow control.
- macOS uses termios and the IOKit serial speed ioctl for custom rates. Mark
  and space parity are unavailable through its termios API. DSR/DTR flow
  control requires a driver that retains those termios flags.
- The POSIX backend rejects 1.5 stop bits. Windows accepts them with 5 data
  bits. Windows requires 6–8 data bits when requesting 2 stop bits.
- Some devices have no controllable modem pins. Default assertions are
  best-effort there. Explicit requests to deassert an unavailable line
  produce an error. A driver can toggle the pins on open before it accepts
  the requested post-open settings.
- Windows requests cancellation with CancelIoEx and waits for the driver's
  overlapped completion before releasing buffers. Cancellation timing
  depends on driver compliance.

## Validation

Run `cargo test --locked -p kortty-serial`. The Unix tests use an OS
pseudo-terminal pair to verify line configuration, unmodified bytes in both
directions, unplug-like EOF, and cancellation with a blocked writer. Windows
tests cover COM name parsing and driver flag translation. For a hardware
check, connect a loopback adapter or two linked ports, choose the same frame
settings at both ends, exchange bytes, and disconnect during blocked I/O.
Verify modem pins, break timing and nonstandard framing with compatible
hardware or a logic analyzer.
