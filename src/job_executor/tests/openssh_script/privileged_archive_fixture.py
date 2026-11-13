import os
from pathlib import Path
import struct
import sys


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


def main():
    root = Path(__file__).resolve().parent
    assert os.environ.get("SSH_ORIGINAL_COMMAND") == (root / "command").read_text()
    mode = (root / "mode").read_text()
    input_stream = sys.stdin.buffer
    output = sys.stdout.buffer
    assert read_frame(input_stream) == (root / "expected-review").read_bytes()
    (root / "review-seen").touch()
    output.write((root / "review").read_bytes())
    output.flush()
    if mode == "revoked":
        if input_stream.read(1):
            (root / "unexpected-data").touch()
        return
    commit = read_frame(input_stream)
    assert commit == (root / "expected-commit").read_bytes()
    assert input_stream.read(1) == b""
    (root / "commit-and-eof-seen").touch()
    if mode == "applied":
        output.write((root / "applied").read_bytes())
        output.flush()
    else:
        assert mode == "lost"


if __name__ == "__main__":
    main()
