use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use kortty_recording::{RecordedScreen, Replay};

use super::{PngPreview, catalog, render};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_PNG_BYTES: u64 = 64 * 1024 * 1024;
const ATTACHMENT_BUDGET: u64 = 32 * 1024 * 1024;
pub(crate) const DIRECTORY_SUFFIX: &str = ".screenshots";
pub(crate) const STAGE_SUFFIX: &str = ".screenshots.partial";

pub(crate) struct Summary {
    pub count: usize,
    pub limited: bool,
}

pub(crate) fn attachment_dir(recording: &Path) -> PathBuf {
    recording.with_extension(&DIRECTORY_SUFFIX[1..])
}

/// Saves the recorded frames in an immutable, private directory beside the replay.
pub(crate) fn attach(recording: &Path, cancelled: impl Fn() -> bool) -> Result<Summary, String> {
    let mut replay = Replay::open(recording).map_err(|error| error.to_string())?;
    if !replay.is_complete() || replay.frame_count() == 0 {
        return Err("A complete terminal recording is required.".into());
    }
    let destination = attachment_dir(recording);
    let stage = StagedDirectory::new(recording.with_extension(&STAGE_SUFFIX[1..]))
        .map_err(|error| error.to_string())?;
    let mut count = 0;
    let mut bytes = 0;
    for index in 0..replay.frame_count() {
        // Preserve the last frame even when intermediate screenshots fill their budget.
        if index != replay.frame_count() - 1 && (bytes >= ATTACHMENT_BUDGET || cancelled()) {
            continue;
        }
        let frame = replay
            .frame(index)
            .map_err(|error| error.to_string())?
            .ok_or("The recording lost a terminal frame.")?;
        let png = render::png(&frame.screen)?;
        let size = png.len() as u64;
        if index != replay.frame_count() - 1 && bytes + size > ATTACHMENT_BUDGET {
            continue;
        }
        write_new(&image_path(&stage.path, index), &png).map_err(|error| error.to_string())?;
        bytes += size;
        count += 1;
    }
    stage
        .publish(&destination)
        .map_err(|error| error.to_string())?;
    Ok(Summary {
        count,
        limited: count != replay.frame_count(),
    })
}

/// Saves the existing PNG attachment if present, otherwise renders the selected frame.
pub(super) fn export(
    directory: Option<&Path>,
    index: usize,
    screen: &RecordedScreen,
    destination: &Path,
) -> Result<(), String> {
    let stored = directory
        .map(|directory| read(directory, index))
        .transpose()?;
    let bytes = match stored.flatten() {
        Some(bytes) => bytes,
        None => render::png(screen)?,
    };
    let parent = destination.parent().ok_or("Invalid PNG destination.")?;
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
    let stage = StagedDirectory::new(
        parent.join(format!(".kortty-frame-{:032x}", u128::from_be_bytes(nonce))),
    )
    .map_err(|error| error.to_string())?;
    let file = stage.path.join("frame.png");
    write_new(&file, &bytes).map_err(|error| error.to_string())?;
    rename_no_replace(&file, destination)
        .and_then(|()| sync_directory(parent))
        .map_err(|error| format!("Could not save the PNG ({error})."))
}

pub(super) fn preview(directory: &Path, index: usize) -> Result<PngPreview, String> {
    let png = read(directory, index)?.ok_or("This frame has no saved PNG attachment.")?;
    let header = png.get(12..24).ok_or("Incomplete PNG header.")?;
    if &header[..4] != b"IHDR" {
        return Err("Invalid PNG header.".into());
    }
    let width = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let height = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if u64::from(width) * u64::from(height) > render::MAX_IMAGE_PIXELS {
        return Err("The terminal screen exceeds the PNG size limit.".into());
    }
    let pixels = tiny_skia::Pixmap::decode_png(&png)
        .map_err(|error| format!("Could not decode the terminal PNG ({error})."))?;
    Ok(PngPreview {
        width,
        height,
        handle: iced::widget::image::Handle::from_rgba(width, height, pixels.take()),
    })
}

fn read(directory: &Path, index: usize) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        _ => return Err("Invalid terminal screenshot directory.".into()),
    }
    let file = match catalog::open_regular(&image_path(directory, index)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not open the terminal screenshot ({error}).")),
    };
    let mut png = Vec::new();
    file.take(MAX_PNG_BYTES + 1)
        .read_to_end(&mut png)
        .map_err(|error| error.to_string())?;
    if png.len() as u64 > MAX_PNG_BYTES || !png.starts_with(PNG_SIGNATURE) {
        return Err("Invalid terminal screenshot.".into());
    }
    Ok(Some(png))
}

fn image_path(directory: &Path, index: usize) -> PathBuf {
    directory.join(format!("{:03}.png", index + 1))
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

struct StagedDirectory {
    path: PathBuf,
    published: bool,
}

impl StagedDirectory {
    fn new(path: PathBuf) -> io::Result<Self> {
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt as _;
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = DirBuilder::new();
        builder.create(&path)?;
        Ok(Self {
            path,
            published: false,
        })
    }

    fn publish(mut self, destination: &Path) -> io::Result<()> {
        sync_directory(&self.path)?;
        rename_no_replace(&self.path, destination)?;
        self.published = true;
        if let Some(parent) = destination.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    Ok(renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)?)
}

#[cfg(windows)]
fn rename_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    kortty_files::rename_no_replace(from, to)
}

pub(crate) fn remove(directory: &Path) -> io::Result<()> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        _ => Err(io::Error::other("invalid terminal screenshot directory")),
    }
}
