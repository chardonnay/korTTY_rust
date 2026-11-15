import os
from pathlib import Path
import re
import struct
import sys
import time

SUDO_READY = b"KORTTY-PRIVILEGED-ARCHIVE-V2\n"
SUDO_COMMIT = b"KORTTY-PRIVILEGED-ARCHIVE-COMMIT\n"


def read_frame(input_stream):
    header = input_stream.read(4)
    if not header:
        return b""
    assert len(header) == 4
    size, = struct.unpack(">I", header)
    assert 0 < size <= 32768
    body = input_stream.read(size)
    assert len(body) == size
    return header + body


def authenticate(root, mode, command, input_stream, output):
    if not mode.startswith("sudo-"):
        assert command == (root / "command").read_text()
        return True
    matched = re.fullmatch(
        r"exec /usr/bin/sudo -k -S -p '(\[kortty-sudo-[0-9a-f]{32}\] )' "
        r"-u root -- /usr/libexec/kortty/kortty-server-archive --privileged-create-v2",
        command,
    )
    assert matched, "expected the exact credential-bearing helper invocation"
    marker = matched[1].encode("ascii")
    diagnostics = sys.stderr.buffer
    if mode != "sudo-no-prompt":
        diagnostics.write(b"echoed ssh arguments: " + marker + b"\n")
        diagnostics.flush()
        if mode == "sudo-revoked-before-auth":
            (root / "sudo-revoked").touch()
            time.sleep(0.1)
        diagnostics.write(marker[:9])
        diagnostics.flush()
        time.sleep(0.03)
        diagnostics.write(marker[9:])
        diagnostics.flush()
        line = input_stream.readline()
        if line:
            (root / "sudo-password-seen").touch()
        if mode == "sudo-revoked-before-auth":
            assert not line
            return False
        assert line == (root / "expected-sudo-password").read_bytes()
        if mode == "sudo-repeated-prompt":
            diagnostics.write(b"\n" + marker)
            diagnostics.flush()
        elif mode == "sudo-bad-greeting":
            output.write(b"unexpected helper greeting\n")
            output.flush()
        elif mode == "sudo-revoked-before-review":
            (root / "sudo-revoked").touch()
            time.sleep(0.1)
            output.write(SUDO_READY)
            output.flush()
        else:
            output.write(SUDO_READY)
            output.flush()
            return True
        if input_stream.read(1):
            (root / "unexpected-data").touch()
        return False
    output.write(SUDO_READY)
    output.flush()
    return True


def main():
    root = Path(__file__).resolve().parent
    mode = (root / "mode").read_text()
    input_stream = sys.stdin.buffer
    output = sys.stdout.buffer
    if not authenticate(
        root, mode, os.environ.get("SSH_ORIGINAL_COMMAND", ""), input_stream, output
    ):
        return
    assert read_frame(input_stream) == (root / "expected-review").read_bytes()
    (root / "review-seen").touch()
    output.write((root / "review").read_bytes())
    output.flush()
    if mode.endswith("revoked"):
        if input_stream.read(1):
            (root / "unexpected-data").touch()
        return
    commit = read_frame(input_stream)
    assert commit == (root / "expected-commit").read_bytes()
    if mode.startswith("password") or mode == "sudo-zip":
        expected = (root / "expected-password").read_bytes()
        assert input_stream.read(len(expected)) == expected
    elif mode.startswith("sudo-"):
        assert input_stream.read(len(SUDO_COMMIT)) == SUDO_COMMIT
    assert input_stream.read(1) == b""
    (root / "commit-and-eof-seen").touch()
    if mode in ("applied", "password", "sudo-password", "sudo-zip", "sudo-no-prompt") or "copy" in mode:
        applied = (root / "applied").read_bytes()
        if mode == "copy-midstream":
            partial = int((root / "copy-frame-bytes").read_text()) + 3
            output.write(applied[:partial])
            output.flush()
            time.sleep(0.05)
            (root / "copy-revoked").touch()
            time.sleep(0.6)
            applied = applied[partial:]
        output.write(applied)
        output.flush()
    else:
        assert mode == "lost"


if __name__ == "__main__":
    main()
