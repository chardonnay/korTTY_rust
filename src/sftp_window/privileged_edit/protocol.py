# Fixed Linux helper for a single privileged file operation.
import hashlib
import hmac
import json
import os
import signal
import stat
import struct
import sys
from contextlib import ExitStack, contextmanager

LIMIT = 2 * 1024 * 1024
TRANSFER_LIMIT = 1024 * 1024 * 1024
CHUNK = 65536
HEADER = 8192
DIR = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
IN = sys.stdin.buffer
OUT = sys.stdout.buffer
PUBLISHING = False


class Refusal(Exception):
    pass


def require(condition, status="blocked"):
    if not condition:
        raise Refusal(status)


def receive(count):
    data = IN.read(count)
    require(len(data) == count, "failed")
    return data


def request():
    count = struct.unpack(">I", receive(4))[0]
    require(0 < count <= HEADER)
    value = json.loads(receive(count))
    require(isinstance(value, dict))
    return value


def send(status, **fields):
    data = json.dumps({"status": status, **fields}, separators=(",", ":")).encode("ascii")
    OUT.write(struct.pack(">I", len(data)) + data)
    OUT.flush()


def components(path):
    require(isinstance(path, str))
    path = path.encode("utf-8")
    require(path.startswith(b"/") and len(path) <= 4096)
    parts = path.split(b"/")[1:]
    require(len(parts) <= 64 and all(
        part and part not in (b".", b"..") and len(part) <= 255
        and all(byte >= 32 and byte != 127 for byte in part) for part in parts
    ))
    return parts


def file_id(info):
    return info.st_dev, info.st_ino


def ancestors(parts, stack):
    parent = os.open(b"/", DIR)
    stack.callback(os.close, parent)
    identities = []
    for part in [None, *parts]:
        if part is not None:
            parent = os.open(part, DIR, dir_fd=parent)
            stack.callback(os.close, parent)
        info = os.fstat(parent)
        require(info.st_uid == 0 and not info.st_mode & 0o022)
        identities.append(file_id(info))
    return parent, identities


def require_ancestry(parts, chain):
    with ExitStack() as stack:
        _, current_chain = ancestors(parts, stack)
        require(current_chain == chain, "conflict")


def metadata(info):
    return (info.st_dev, info.st_ino, info.st_uid, info.st_gid, info.st_mode,
            info.st_nlink, info.st_size, info.st_mtime_ns, info.st_ctime_ns)


def regular(info, limit=LIMIT):
    return (stat.S_ISREG(info.st_mode) and info.st_uid == 0 and info.st_nlink == 1
            and not info.st_mode & 0o7022 and 0 <= info.st_size <= limit)


def attributes(fd):
    names = sorted(os.listxattr(fd))
    require(len(names) <= 128)
    values = {}
    total = 0
    for name in names:
        require(name.startswith("user.") or name in ("system.posix_acl_access", "security.selinux"))
        value = os.getxattr(fd, name)
        total += len(value) + len(os.fsencode(name))
        require(total <= 128 * 1024)
        values[name] = value
    return values


def digest(data):
    return hashlib.sha256(data).hexdigest()


def text(data):
    require(len(data) <= LIMIT and b"\x00" not in data)
    data.decode("utf-8")
    return data


def snapshot(fd):
    info = os.fstat(fd)
    require(regular(info))
    attrs = attributes(fd)
    os.lseek(fd, 0, os.SEEK_SET)
    data = bytearray()
    while len(data) <= LIMIT:
        chunk = os.read(fd, min(CHUNK, LIMIT + 1 - len(data)))
        if not chunk:
            break
        data.extend(chunk)
    require(len(data) == info.st_size and metadata(os.fstat(fd)) == metadata(info), "conflict")
    text(data)
    hashed = digest(data)
    pairs = [(os.fsencode(name).hex(), value.hex()) for name, value in attrs.items()]
    identity = json.dumps([metadata(info), hashed, pairs], separators=(",", ":")).encode("ascii")
    return info, attrs, data, hashed, digest(identity)


def named(parent, name):
    return os.stat(name, dir_fd=parent, follow_symlinks=False)


def original(parent, name, stack, limit=LIMIT):
    info = named(parent, name)
    require(regular(info, limit))
    fd = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
    stack.callback(os.close, fd)
    require(metadata(os.fstat(fd)) == metadata(info), "conflict")
    return fd


def remove_stage(parent, stage_name, stage, staged):
    # Remove only the entries created by this operation. A foreign entry is retained.
    try:
        if stage is None or file_id(named(parent, stage_name)) != file_id(os.fstat(stage)):
            return
        if staged is not None:
            try:
                if file_id(named(stage, b"data")) != file_id(os.fstat(staged)):
                    return
                os.unlink(b"data", dir_fd=stage)
            except FileNotFoundError:
                pass
        os.rmdir(stage_name, dir_fd=parent)
    except OSError:
        pass


def size_and_hash(req, limit):
    size = req["size"]
    hashed = req["sha256"]
    require(type(size) is int and 0 <= size <= limit)
    require(isinstance(hashed, str) and len(hashed) == 64 and all(
        char in "0123456789abcdef" for char in hashed
    ))
    return size, hashed


@contextmanager
def staging(parent):
    stage_name = b".kortty-root-edit-" + os.urandom(16).hex().encode("ascii")
    os.mkdir(stage_name, 0o700, dir_fd=parent)
    stage = staged = None
    try:
        stage = os.open(stage_name, DIR, dir_fd=parent)
        stage_info = os.fstat(stage)
        require(stage_info.st_uid == 0 and stat.S_IMODE(stage_info.st_mode) == 0o700)
        staged = os.open(
            b"data", os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600, dir_fd=stage,
        )
        yield stage_name, stage, staged, stage_info
    finally:
        remove_stage(parent, stage_name, stage, staged)
        if staged is not None:
            os.close(staged)
        if stage is not None:
            os.close(stage)


def store(fd, data):
    pending = memoryview(data)
    while pending:
        count = os.write(fd, pending)
        require(count > 0, "failed")
        pending = pending[count:]


def await_commit():
    token = os.urandom(16).hex()
    send("ready", token=token)
    require(hmac.compare_digest(receive(32), token.encode("ascii")), "failed")


def write(req, parts, parent, chain, source, opened):
    global PUBLISHING
    info, attrs, _, _, version = opened
    size, hashed = size_and_hash(req, LIMIT)
    require(req["version"] == version, "conflict")
    send("accept")
    data = text(receive(size))
    require(digest(data) == hashed, "failed")
    name = parts[-1]
    with staging(parent) as (stage_name, stage, staged, stage_info):
        store(staged, data)
        os.fchown(staged, info.st_uid, info.st_gid)
        os.fchmod(staged, stat.S_IMODE(info.st_mode))
        previous = attributes(staged)
        for key in previous.keys() - attrs.keys():
            os.removexattr(staged, key)
        for key, value in attrs.items():
            if previous.get(key) != value:
                os.setxattr(staged, key, value)
        staged_info, staged_attrs, _, staged_hash, _ = snapshot(staged)
        require((staged_info.st_uid, staged_info.st_gid, staged_info.st_mode)
                == (info.st_uid, info.st_gid, info.st_mode))
        require(staged_attrs == attrs and staged_hash == hashed, "failed")
        os.fsync(staged)
        os.fsync(stage)
        await_commit()

        require_ancestry(parts[:-1], chain)
        require(snapshot(source)[-1] == version, "conflict")
        require(metadata(named(parent, name)) == metadata(info), "conflict")
        require(file_id(named(parent, stage_name)) == file_id(stage_info), "conflict")
        require(metadata(named(stage, b"data")) == metadata(staged_info), "conflict")
        PUBLISHING = True
        os.replace(b"data", name, src_dir_fd=stage, dst_dir_fd=parent)
        os.fsync(stage)
        os.fsync(parent)
        current_info, _, _, current_hash, current_version = snapshot(staged)
        require(metadata(named(parent, name)) == metadata(current_info) and current_hash == hashed)
        return current_version


def download(parts, parent, chain, stack):
    name = parts[-1]
    source = original(parent, name, stack, TRANSFER_LIMIT)
    info = os.fstat(source)
    size = info.st_size
    require_ancestry(parts[:-1], chain)
    send("download", size=size)
    hashed = hashlib.sha256()
    remaining = size
    while remaining:
        data = os.read(source, min(remaining, CHUNK))
        require(data, "conflict")
        hashed.update(data)
        OUT.write(data)
        remaining -= len(data)
    OUT.flush()
    require_ancestry(parts[:-1], chain)
    require(metadata(os.fstat(source)) == metadata(info)
            and metadata(named(parent, name)) == metadata(info), "conflict")
    send("downloaded", size=size, sha256=hashed.hexdigest())


def require_absent(parent, name):
    try:
        named(parent, name)
    except FileNotFoundError:
        return
    raise Refusal("conflict")


def create(req, parts, parent, chain):
    name = parts[-1]
    size, expected = size_and_hash(req, TRANSFER_LIMIT)
    require_absent(parent, name)
    with staging(parent) as (stage_name, stage, staged, stage_info):
        send("accept")
        hashed = hashlib.sha256()
        remaining = size
        while remaining:
            data = receive(min(remaining, CHUNK))
            hashed.update(data)
            store(staged, data)
            remaining -= len(data)
        require(hashed.hexdigest() == expected, "failed")
        os.fchmod(staged, 0o600)
        staged_info = os.fstat(staged)
        require(regular(staged_info, TRANSFER_LIMIT) and staged_info.st_size == size)
        os.fsync(staged)
        os.fsync(stage)
        await_commit()
        publish_created(parts, parent, chain, stage_name, stage, staged, stage_info, staged_info)
    send("created", size=size, sha256=expected)


def publish_created(parts, parent, chain, stage_name, stage, staged, stage_info, staged_info):
    global PUBLISHING
    name = parts[-1]
    require_ancestry(parts[:-1], chain)
    require(file_id(named(parent, stage_name)) == file_id(stage_info), "conflict")
    require(metadata(named(stage, b"data")) == metadata(staged_info), "conflict")
    PUBLISHING = True
    try:
        os.link(b"data", name, src_dir_fd=stage, dst_dir_fd=parent, follow_symlinks=False)
    except FileExistsError:
        PUBLISHING = False
        raise Refusal("conflict")
    os.unlink(b"data", dir_fd=stage)
    os.fsync(stage)
    os.fsync(parent)
    require(file_id(named(parent, name)) == file_id(os.fstat(staged)), "conflict")


TREE_ENTRIES = 128
TREE_PATH_BYTES = 32 * 1024
TREE_DOWNLOAD_DEPTH = 32
RENAME_NOREPLACE = 1


def no_replace_rename():
    import ctypes
    try:
        rename = ctypes.CDLL(None, use_errno=True).renameat2
    except AttributeError:
        raise Refusal("blocked") from None
    rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int,
                       ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int

    def publish(source, name, destination, target):
        return (ctypes.get_errno()
                if rename(source, name, destination, target, RENAME_NOREPLACE) else 0)

    return publish


def tree_target(parts, parent, chain):
    require_ancestry(parts[:-1], chain)
    require_absent(parent, parts[-1])
    return digest(json.dumps(chain, separators=(",", ":")).encode("ascii"))


@contextmanager
def tree_stage(parent):
    name = b".kortty-root-tree-" + os.urandom(16).hex().encode("ascii")
    os.mkdir(name, 0o700, dir_fd=parent)
    created = named(parent, name)
    with ExitStack() as stack:
        owned = []
        try:
            root = os.open(name, DIR, dir_fd=parent)
            stack.callback(os.close, root)
            info = os.fstat(root)
            require(info.st_uid == 0 and file_id(info) == file_id(created))
            os.fchmod(root, 0o700)
            require(stat.S_IMODE(os.fstat(root).st_mode) == 0o700)
            yield name, root, owned, stack
        finally:
            try:
                if file_id(named(parent, name)) == file_id(created):
                    for directory, leaf, identity, folder in reversed(owned):
                        if file_id(named(directory, leaf)) == identity:
                            if folder:
                                os.rmdir(leaf, dir_fd=directory)
                            else:
                                os.unlink(leaf, dir_fd=directory)
                    os.rmdir(name, dir_fd=parent)
            except OSError:
                pass


def tree_body(req, root, owned, stack):
    count, total = req["entries"], req["size"]
    require(type(count) is int and 0 <= count <= TREE_ENTRIES)
    require(type(total) is int and 0 <= total <= TRANSFER_LIMIT)
    folders = {(): root}
    seen = set()
    path_bytes = 0
    remaining = total
    for _ in range(count):
        entry = request()
        kind = entry["kind"]
        require(kind in ("folder", "file"))
        keys = {"kind", "path"}
        if kind == "file":
            keys |= {"size", "sha256"}
        require(entry.keys() == keys and isinstance(entry["path"], str))
        path = tuple(components("/" + entry["path"]))
        path_bytes += len(entry["path"].encode("utf-8"))
        require(path_bytes <= TREE_PATH_BYTES and path not in seen and path[:-1] in folders)
        seen.add(path)
        parent, name = folders[path[:-1]], path[-1]
        if kind == "folder":
            os.mkdir(name, 0o700, dir_fd=parent)
            created = named(parent, name)
            owned.append((parent, name, file_id(created), True))
            fd = os.open(name, DIR, dir_fd=parent)
            stack.callback(os.close, fd)
            info = os.fstat(fd)
            require(info.st_uid == 0 and file_id(info) == file_id(created)
                    and stat.S_IMODE(info.st_mode) == 0o700)
            folders[path] = fd
        else:
            size, expected = size_and_hash(entry, remaining)
            remaining -= size
            fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                         0o600, dir_fd=parent)
            try:
                owned.append((parent, name, file_id(os.fstat(fd)), False))
                hashed = hashlib.sha256()
                while size:
                    data = receive(min(size, CHUNK))
                    hashed.update(data)
                    store(fd, data)
                    size -= len(data)
                require(hashed.hexdigest() == expected, "conflict")
                info = os.fstat(fd)
                require(regular(info, TRANSFER_LIMIT) and stat.S_IMODE(info.st_mode) == 0o600)
                os.fsync(fd)
            finally:
                os.close(fd)
    require(remaining == 0)
    for folder in reversed(tuple(folders.values())):
        os.fsync(folder)


def create_tree(req, parts, parent, chain, publish):
    witness = tree_target(parts, parent, chain)
    token = os.urandom(16).hex()
    send("tree_target", witness=witness, token=token)
    require(hmac.compare_digest(receive(32), token.encode("ascii")), "failed")
    require(tree_target(parts, parent, chain) == witness, "conflict")
    with tree_stage(parent) as (name, root, owned, stack):
        send("accept")
        tree_body(req, root, owned, stack)
        await_commit()
        require_ancestry(parts[:-1], chain)
        publish_tree(publish, parent, name, root, parts[-1])
    send("tree_created", entries=req["entries"], size=req["size"])


def publish_tree(publish, parent, name, root, target):
    import errno
    global PUBLISHING
    identity = file_id(os.fstat(root))
    require(file_id(named(parent, name)) == identity, "conflict")
    PUBLISHING = True
    failure = publish(parent, name, parent, target)
    if failure in (errno.EEXIST, errno.ENOTEMPTY, errno.EINVAL, errno.ENOSYS, errno.EOPNOTSUPP):
        PUBLISHING = False
        raise Refusal("conflict" if failure in (errno.EEXIST, errno.ENOTEMPTY) else "blocked")
    require(failure == 0, "failed")
    os.fsync(parent)
    require(file_id(named(parent, target)) == identity, "conflict")


def source_file(parent, name, stamp, output=None):
    hashed = hashlib.sha256()
    fd = os.open(name, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC, dir_fd=parent)
    try:
        require(metadata(os.fstat(fd)) == stamp, "conflict")
        remaining = stamp[6]
        while remaining:
            data = os.read(fd, min(remaining, CHUNK))
            require(data, "conflict")
            remaining -= len(data)
            hashed.update(data)
            if output is not None:
                output.write(data)
        require(metadata(os.fstat(fd)) == stamp
                and metadata(named(parent, name)) == stamp, "conflict")
    finally:
        os.close(fd)
    return hashed.hexdigest()


def download_tree(parts, stream):
    with ExitStack() as stack:
        root, chain = ancestors(parts, stack)
        root_stamp = metadata(os.fstat(root))
        folders = []
        files = []
        rows = []
        seen_dirs = {file_id(os.fstat(root))}
        size = path_bytes = 0

        def walk(folder, prefix):
            nonlocal size, path_bytes
            names = []
            with os.scandir(folder) as children:
                for child in children:
                    require(len(rows) + len(names) < TREE_ENTRIES)
                    names.append(os.fsencode(child.name))
            for name in sorted(names):
                require(len(rows) < TREE_ENTRIES)
                relative = prefix + name
                path = relative.decode("utf-8")
                require(len(relative) <= 2048)
                parts_count = len(components("/" + path))
                path_bytes += len(relative)
                require(path_bytes <= TREE_PATH_BYTES)
                info = named(folder, name)
                stamp = metadata(info)
                require(parts_count - int(not stat.S_ISDIR(info.st_mode)) <= TREE_DOWNLOAD_DEPTH)
                if stat.S_ISDIR(info.st_mode):
                    identity = file_id(info)
                    require(info.st_uid == 0 and not info.st_mode & 0o022
                            and identity not in seen_dirs)
                    seen_dirs.add(identity)
                    fd = os.open(name, DIR, dir_fd=folder)
                    stack.callback(os.close, fd)
                    require(metadata(os.fstat(fd)) == stamp, "conflict")
                    folders.append((folder, name, fd, stamp))
                    rows.append((path, stamp, None))
                    walk(fd, relative + b"/")
                else:
                    require(regular(info, TRANSFER_LIMIT))
                    size += info.st_size
                    require(size <= TRANSFER_LIMIT)
                    hashed = source_file(folder, name, stamp)
                    files.append((folder, name, stamp, hashed))
                    rows.append((path, stamp, hashed))

        def verify():
            require_ancestry(parts, chain)
            require(metadata(os.fstat(root)) == root_stamp, "conflict")
            for parent, name, fd, stamp in folders:
                require(metadata(os.fstat(fd)) == stamp
                        and metadata(named(parent, name)) == stamp, "conflict")
            for parent, name, stamp, _ in files:
                require(metadata(named(parent, name)) == stamp, "conflict")

        walk(root, b"")
        verify()
        witness = digest(json.dumps((chain, root_stamp, rows), separators=(",", ":")).encode("ascii"))
        send("tree_manifest", witness=witness, entries=len(rows), size=size)
        for path, stamp, hashed in rows:
            if hashed is None:
                send("tree_directory", path=path)
            else:
                send("tree_file", path=path, size=stamp[6], sha256=hashed)
        if stream:
            await_commit()
            verify()
            for parent, name, stamp, hashed in files:
                require(source_file(parent, name, stamp, OUT) == hashed, "conflict")
            verify()
            send("tree_read", witness=witness)


def interrupted(_number, _frame):
    raise Refusal("failed")


def copy_protected(parts, parent, chain, to, stack):
    require(parts != to)
    destination, destination_chain = ancestors(to[:-1], stack)
    require_absent(destination, to[-1])
    source = original(parent, parts[-1], stack, TRANSFER_LIMIT)
    info = os.fstat(source)
    with staging(destination) as (stage_name, stage, staged, stage_info):
        remaining = info.st_size
        while remaining:
            data = os.read(source, min(CHUNK, remaining))
            require(data, "conflict")
            store(staged, data)
            remaining -= len(data)
        os.fchmod(staged, 0o600)
        staged_info = os.fstat(staged)
        require(regular(staged_info, TRANSFER_LIMIT) and staged_info.st_size == info.st_size)
        os.fsync(staged)
        os.fsync(stage)
        await_commit()
        require_ancestry(parts[:-1], chain)
        expected = metadata(info)
        require(metadata(os.fstat(source)) == expected
                and metadata(named(parent, parts[-1])) == expected, "conflict")
        publish_created(to, destination, destination_chain, stage_name, stage, staged,
                        stage_info, staged_info)


def protected_node(parent, name, stack):
    info = named(parent, name)
    directory = stat.S_ISDIR(info.st_mode)
    require(info.st_uid == 0 and not info.st_mode & 0o7022
            and (directory or stat.S_ISREG(info.st_mode) and info.st_nlink == 1))
    flags = DIR if directory else os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW | os.O_CLOEXEC
    source = os.open(name, flags, dir_fd=parent)
    stack.callback(os.close, source)
    expected = metadata(info)
    require(metadata(os.fstat(source)) == expected, "conflict")
    return source, expected, directory


def require_node(parent, name, source, expected):
    try:
        current = metadata(named(parent, name))
    except FileNotFoundError:
        raise Refusal("conflict") from None
    require(metadata(os.fstat(source)) == expected and current == expected, "conflict")


def rename_protected(parts, parent, chain, to, stack):
    import errno
    global PUBLISHING
    require(parts != to)
    destination, destination_chain = ancestors(to[:-1], stack)
    require_absent(destination, to[-1])
    name = parts[-1]
    source, expected, _ = protected_node(parent, name, stack)
    publish = no_replace_rename()
    await_commit()
    require_ancestry(parts[:-1], chain)
    require_ancestry(to[:-1], destination_chain)
    require_node(parent, name, source, expected)
    PUBLISHING = True
    failure = publish(parent, name, destination, to[-1])
    if failure:
        PUBLISHING = False
        raise Refusal("conflict" if failure in (errno.EEXIST, errno.ENOTEMPTY, errno.ENOENT)
                      else "blocked")
    require(file_id(named(destination, to[-1])) == file_id(os.fstat(source)), "conflict")
    os.fsync(parent)
    os.fsync(destination)


def mount_id(fd):
    with open(f"/proc/self/fdinfo/{fd}", "rb") as details:
        for line in details:
            if line.startswith(b"mnt_id:"):
                return int(line.split(b":", 1)[1])
    raise Refusal("blocked")


def protected_tree(parent, name, recursive, stack):
    nodes = []
    seen = set()
    scope = mount_id(parent)

    def inspect(directory, leaf, path):
        require(len(path) <= TREE_DOWNLOAD_DEPTH and len(nodes) < TREE_ENTRIES)
        fd, expected, folder = protected_node(directory, leaf, stack)
        require(mount_id(fd) == scope and expected[:2] not in seen)
        seen.add(expected[:2])
        nodes.append((directory, leaf, fd, expected, folder, path))
        if folder:
            with os.scandir(fd) as children:
                for child in children:
                    require(recursive)
                    child_name = os.fsencode(child.name)
                    inspect(fd, child_name, (*path, child_name))
        require_node(directory, leaf, fd, expected)

    inspect(parent, name, ())
    return nodes


def require_tree(nodes):
    for directory, name, fd, expected, _, _ in nodes:
        require_node(directory, name, fd, expected)


def make_private_tree_root(fd):
    import errno
    for attribute in ("system.posix_acl_default", "system.posix_acl_access"):
        try:
            os.removexattr(fd, attribute)
        except OSError as error:
            if error.errno not in (errno.ENODATA, errno.ENOTSUP):
                raise
    os.fchmod(fd, 0o700)


def copy_protected_tree(parts, parent, chain, to, stack):
    require(to[:len(parts)] != parts)
    destination, destination_chain = ancestors(to[:-1], stack)
    require_absent(destination, to[-1])
    nodes = protected_tree(parent, parts[-1], True, stack)
    require(nodes[0][4] and nodes[0][3][:2] not in destination_chain)
    require(sum(sum(len(part) + 1 for part in path) for *_, path in nodes) <= TREE_PATH_BYTES)
    require(sum(expected[6] for _, _, _, expected, folder, _ in nodes if not folder)
            <= TRANSFER_LIMIT)
    publish = no_replace_rename()
    with tree_stage(destination) as (stage_name, stage, owned, staging_stack):
        make_private_tree_root(stage)
        folders = {(): stage}
        for _, leaf, source, expected, folder, path in nodes[1:]:
            directory = folders[path[:-1]]
            if folder:
                os.mkdir(leaf, 0o700, dir_fd=directory)
                created = file_id(named(directory, leaf))
                owned.append((directory, leaf, created, True))
                fd = os.open(leaf, DIR, dir_fd=directory)
                staging_stack.callback(os.close, fd)
                require(file_id(os.fstat(fd)) == created)
                os.fchmod(fd, 0o700)
                folders[path] = fd
            else:
                fd = os.open(leaf, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
                             0o600, dir_fd=directory)
                try:
                    owned.append((directory, leaf, file_id(os.fstat(fd)), False))
                    remaining = expected[6]
                    while remaining:
                        data = os.read(source, min(CHUNK, remaining))
                        require(data, "conflict")
                        store(fd, data)
                        remaining -= len(data)
                    os.fchmod(fd, 0o600)
                    os.fsync(fd)
                finally:
                    os.close(fd)
        for fd in reversed(tuple(folders.values())):
            os.fsync(fd)
        require_tree(nodes)
        await_commit()
        require_ancestry(parts[:-1], chain)
        require_ancestry(to[:-1], destination_chain)
        require_tree(nodes)
        publish_tree(publish, destination, stage_name, stage, to[-1])


def delete_protected(parts, parent, chain, recursive, stack):
    global PUBLISHING
    nodes = protected_tree(parent, parts[-1], recursive, stack)
    await_commit()
    require_ancestry(parts[:-1], chain)
    require_tree(nodes)
    for directory, name, fd, expected, folder, _ in reversed(nodes):
        # Removing a child changes its directory stamp. Verify its held identity
        # and let rmdir reject any entries that were absent from the reviewed tree.
        if folder:
            current = metadata(named(directory, name))
            require(current[:5] == expected[:5]
                    and current == metadata(os.fstat(fd)), "conflict")
        else:
            require_node(directory, name, fd, expected)
        PUBLISHING = True
        if folder:
            os.rmdir(name, dir_fd=directory)
        else:
            os.unlink(name, dir_fd=directory)
    os.fsync(parent)


def file_operation(action, parts, parent, chain, stack):
    global PUBLISHING
    require(isinstance(action, dict))
    kind = action["kind"]
    name = parts[-1]
    if kind == "copy":
        require(action.keys() == {"kind", "to"})
        copy_protected(parts, parent, chain, components(action["to"]), stack)
    elif kind == "copy_tree":
        require(action.keys() == {"kind", "to"})
        copy_protected_tree(parts, parent, chain, components(action["to"]), stack)
    elif kind == "rename":
        require(action.keys() == {"kind", "to"})
        rename_protected(parts, parent, chain, components(action["to"]), stack)
    elif kind == "delete":
        require(action.keys() == {"kind", "recursive"} and type(action["recursive"]) is bool)
        delete_protected(parts, parent, chain, action["recursive"], stack)
    elif kind == "mkdir":
        require(action.keys() == {"kind"})
        require_absent(parent, name)
        await_commit()
        require_ancestry(parts[:-1], chain)
        require_absent(parent, name)
        PUBLISHING = True
        try:
            os.mkdir(name, 0o700, dir_fd=parent)
        except FileExistsError:
            PUBLISHING = False
            raise Refusal("conflict")
        os.fsync(parent)
    else:
        if kind == "chmod":
            require(action.keys() == {"kind", "mode"})
            mode = action["mode"]
            require(type(mode) is int and 0 <= mode <= 0o777)
        else:
            require(kind == "chown" and action.keys() == {"kind", "uid", "gid"})
            uid, gid = action["uid"], action["gid"]
            require(uid is not None or gid is not None)
            require(all(value is None or type(value) is int and 0 <= value < 0xffffffff
                        for value in (uid, gid)))
        source, expected, _ = protected_node(parent, name, stack)
        await_commit()
        require_ancestry(parts[:-1], chain)
        require_node(parent, name, source, expected)
        PUBLISHING = True
        if kind == "chmod":
            os.fchmod(source, mode)
        else:
            os.fchown(source, -1 if uid is None else uid, -1 if gid is None else gid)
        os.fsync(source)
    send("file_action_applied")


def run():
    require(sys.platform == "linux" and os.geteuid() == 0)
    require(sys.argv[1:] in ([], ["transfer"]))
    transfer = bool(sys.argv[1:])
    os.umask(0o077)
    for kind in (signal.SIGALRM, signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
        signal.signal(kind, interrupted)
    signal.alarm(3600 if transfer else 60)
    send("hello", protocol=1)
    req = request()
    op = req["operation"]
    keys = {"operation", "path"}
    if op == "write":
        keys |= {"size", "sha256", "version"}
    if op == "create":
        keys |= {"size", "sha256"}
    if op == "create_tree":
        keys |= {"entries", "size"}
    if op == "file_action":
        keys.add("action")
    require(op in (("download", "create", "inspect_tree", "create_tree",
                    "inspect_tree_source", "download_tree")
                   if transfer else ("read", "write", "file_action"))
            and req.keys() == keys)
    parts = components(req["path"])
    if op in ("inspect_tree_source", "download_tree"):
        download_tree(parts, op == "download_tree")
        return
    with ExitStack() as stack:
        parent, chain = ancestors(parts[:-1], stack)
        if op == "file_action":
            file_operation(req["action"], parts, parent, chain, stack)
            return
        if transfer:
            if op == "download":
                download(parts, parent, chain, stack)
            elif op == "create":
                create(req, parts, parent, chain)
            elif op == "inspect_tree":
                no_replace_rename()
                send("tree_target", witness=tree_target(parts, parent, chain))
            else:
                create_tree(req, parts, parent, chain, no_replace_rename())
            return
        source = original(parent, parts[-1], stack)
        opened = snapshot(source)
        require(metadata(named(parent, parts[-1])) == metadata(opened[0]), "conflict")
        if op == "read":
            require_ancestry(parts[:-1], chain)
            _, _, data, hashed, version = opened
            send("read", size=len(data), sha256=hashed, version=version)
            OUT.write(data)
            OUT.flush()
        else:
            version = write(req, parts, parent, chain, source, opened)
            send("written", size=req["size"], sha256=req["sha256"], version=version)


try:
    run()
except Refusal as error:
    send("uncertain" if PUBLISHING else error.args[0])
except (OSError, ValueError, KeyError, TypeError):
    send("uncertain" if PUBLISHING else "blocked")
