//! Review a Windows rsync image and its adjacent runtime libraries as one bundle.

use std::fs::OpenOptions;
use std::io::Seek as _;
use std::os::windows::fs::OpenOptionsExt as _;

use sha2::{Digest as _, Sha256};

use super::*;

const MAX_LIBRARY_ENTRIES: usize = 128;
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;

pub(super) struct Prepared {
    program: PathBuf,
    // Drop the non-writable file handles before the scratch directory cleans up.
    _held: Vec<File>,
    _library_directory: local_tree::Directory,
    _scratch: Scratch,
}

impl Prepared {
    pub(super) fn new(
        program: &Path,
        reviewed: [u8; 32],
        stopped: &impl Fn() -> bool,
    ) -> io::Result<Self> {
        let resolved = canonical_program(program)?;
        let name = resolved.file_name().ok_or_else(changed_program)?;
        let source_path = resolved.parent().ok_or_else(changed_program)?;
        let source = local_tree::Directory::open_reviewed_removal_parent(source_path)?;
        let scratch = Scratch::new(&std::env::temp_dir())?;
        let directory = create_scratch_child(scratch.directory()?, OsStr::new("client"))?;
        let (actual, _) = measure(&source, source_path, name, Some(&directory), stopped)?;
        if actual != reviewed || canonical_program(program)? != resolved {
            return Err(changed_program());
        }
        let staged = scratch.path.join("client");
        let (verified, held) = measure(&directory, &staged, name, None, stopped)?;
        if verified != reviewed {
            return Err(changed_program());
        }
        directory.require_owner_only()?;
        Ok(Self {
            program: staged.join(name),
            _held: held,
            _library_directory: directory,
            _scratch: scratch,
        })
    }

    pub(super) fn program(&self) -> &Path {
        &self.program
    }
}

pub(super) fn fingerprint(program: &Path, stopped: &impl Fn() -> bool) -> io::Result<[u8; 32]> {
    let resolved = canonical_program(program)?;
    let name = resolved.file_name().ok_or_else(changed_program)?;
    let directory = resolved.parent().ok_or_else(changed_program)?;
    let source = local_tree::Directory::open_reviewed_removal_parent(directory)?;
    let (digest, _) = measure(&source, directory, name, None, stopped)?;
    if canonical_program(program)? != resolved {
        return Err(changed_program());
    }
    Ok(digest)
}

fn canonical_program(program: &Path) -> io::Result<PathBuf> {
    if !crate::private_files::executable::valid_path(program) {
        return Err(changed_program());
    }
    let canonical = program.canonicalize()?;
    let spelling = |path: &Path| {
        path.to_str()
            .map(|path| {
                path.strip_prefix(r"\\?\")
                    .unwrap_or(path)
                    .replace('/', "\\")
            })
            .ok_or_else(changed_program)
    };
    if !spelling(program)?.eq_ignore_ascii_case(&spelling(&canonical)?) {
        return Err(changed_program());
    }
    Ok(canonical)
}

fn library_names(directory: &local_tree::Directory) -> io::Result<Vec<OsString>> {
    let mut names = directory.names(4096)?;
    names.retain(|name| {
        Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
    });
    if names.len() > MAX_LIBRARY_ENTRIES {
        return Err(io::Error::other(
            "The rsync program has too many adjacent libraries",
        ));
    }
    names.sort_unstable();
    Ok(names)
}

fn measure(
    directory: &local_tree::Directory,
    path: &Path,
    program_name: &OsStr,
    copy_to: Option<&local_tree::Directory>,
    stopped: &impl Fn() -> bool,
) -> io::Result<([u8; 32], Vec<File>)> {
    const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const SHARE_READ: u32 = 1;
    let libraries = library_names(directory)?;
    let mut names = std::collections::BTreeSet::new();
    let mut held = Vec::new();
    let mut digest = Sha256::new();
    digest.update(b"kortty-windows-rsync-program-v1\0");
    let mut remaining = MAX_BUNDLE_BYTES;
    for name in std::iter::once(program_name).chain(libraries.iter().map(OsString::as_os_str)) {
        require_running(stopped)?;
        let display = name.to_str().ok_or_else(changed_program)?;
        crate::sftp_window::checked_local_component(display.as_bytes())?;
        if !names.insert(display.to_lowercase()) {
            return Err(changed_program());
        }
        let expected = directory.read_child(name)?.ok_or_else(changed_program)?;
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ)
            .custom_flags(OPEN_REPARSE_POINT)
            .open(path.join(name))?;
        let stamp = FileStamp::read(&expected)?;
        stamp.require_unchanged(&file)?;
        drop(expected);
        crate::private_files::executable::require_native_image(&mut file)?;
        file.rewind()?;
        let size = file.metadata()?.len();
        if size == 0 || size > MAX_PROGRAM || size > remaining {
            return Err(io::Error::other(
                "The reviewed rsync program bundle exceeds its size limit",
            ));
        }
        remaining -= size;
        digest.update((display.len() as u32).to_le_bytes());
        digest.update(display.as_bytes());
        digest.update(size.to_le_bytes());
        let mut copy = copy_to.map(|target| target.create_file(name)).transpose()?;
        let mut buffer = [0; 32 * 1024];
        let mut input_remaining = size;
        while input_remaining != 0 {
            require_running(stopped)?;
            let length = input_remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..length])?;
            digest.update(&buffer[..length]);
            if let Some(copy) = &mut copy {
                copy.write_all(&buffer[..length])?;
            }
            input_remaining -= length as u64;
        }
        if file.read(&mut buffer[..1])? != 0 {
            return Err(changed_program());
        }
        if let Some(copy) = copy {
            copy.sync_all()?;
        }
        stamp.require_unchanged(&file)?;
        held.push(file);
    }
    if library_names(directory)? != libraries
        || !directory.same_as(&local_tree::Directory::open_path(path)?)?
    {
        return Err(changed_program());
    }
    Ok((digest.finalize().into(), held))
}
