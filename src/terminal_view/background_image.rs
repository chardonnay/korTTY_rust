//! Local terminal backgrounds, prepared from a file the user selected.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{self, Cursor, Read};
use std::path::Path;

use iced::widget::image::Handle;
use image::{
    DynamicImage, ExtendedColorType, ImageDecoder as _, ImageEncoder as _, ImageFormat,
    ImageReader, Limits, RgbaImage, imageops::FilterType,
};
use pgp::crypto::hash::HashAlgorithm;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::private_files::Directory;
use crate::{profile, ssh_dialog};

pub(crate) const DIRECTORY: &str = "terminal-backgrounds";
pub(crate) const MAX_STORED_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 12 * 1024 * 1024;
const MAX_SOURCE_EDGE: u32 = 4096;
const MAX_SOURCE_PIXELS: u64 = 8 * 1024 * 1024;
const LONG_EDGE: u32 = 1280;
const SHORT_EDGE: u32 = 720;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AssetId([u8; 32]);

impl AssetId {
    fn for_png(bytes: &[u8]) -> io::Result<Self> {
        HashAlgorithm::Sha256
            .digest(bytes)
            .map_err(io::Error::other)?
            .try_into()
            .map(Self)
            .map_err(|_| io::Error::other("Invalid SHA-256 digest"))
    }

    fn parse(hex: &str) -> Option<Self> {
        fn digit(value: u8) -> Option<u8> {
            match value {
                b'0'..=b'9' => Some(value - b'0'),
                b'a'..=b'f' => Some(value - b'a' + 10),
                _ => None,
            }
        }
        if hex.len() != 64 {
            return None;
        }
        let mut digest = [0; 32];
        for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            *byte = (digit(pair[0])? << 4) | digit(pair[1])?;
        }
        Some(Self(digest))
    }

    fn file_name(self) -> String {
        format!("{self}.png")
    }

    pub(crate) fn relative_path(self) -> String {
        format!("{DIRECTORY}/{}", self.file_name())
    }

    pub(crate) fn from_relative_path(path: &str) -> Option<Self> {
        let (directory, name) = path.split_once('/')?;
        (directory == DIRECTORY).then_some(())?;
        Self::from_file_name(name)
    }

    fn from_file_name(name: &str) -> Option<Self> {
        Self::parse(name.strip_suffix(".png")?)
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for AssetId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let hex = String::deserialize(deserializer)?;
        Self::parse(&hex)
            .ok_or_else(|| serde::de::Error::custom("invalid terminal background identifier"))
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Image {
    pub(super) handle: Handle,
    pub(super) width: u32,
    pub(super) height: u32,
}

impl Image {
    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }

    fn new(pixels: RgbaImage) -> Self {
        let (width, height) = pixels.dimensions();
        Self {
            handle: Handle::from_rgba(width, height, pixels.into_raw()),
            width,
            height,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Prepared {
    id: AssetId,
    png: Vec<u8>,
    image: Image,
}

impl Prepared {
    pub(crate) fn id(&self) -> AssetId {
        self.id
    }

    pub(crate) fn handle(&self) -> &Handle {
        self.image.handle()
    }

    pub(crate) fn publish(&self, root: &Path) -> io::Result<()> {
        let directory = assets(root, true)?;
        let name = self.id.file_name();
        match directory.publish_copy(&name, &self.png) {
            Ok(()) => directory.sync(),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = read_bounded(directory.open_private_file(&name)?, MAX_STORED_BYTES)?;
                if existing != self.png {
                    return Err(invalid(
                        "The stored background differs from the prepared image.",
                    ));
                }
                directory.sync()
            }
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn prepare(path: &Path) -> io::Result<Prepared> {
    let path_text = path
        .to_str()
        .filter(|path| path.len() <= 4096 && !ssh_dialog::unsafe_text(path));
    if !path.is_absolute() || path_text.is_none() {
        return Err(invalid("Choose an absolute local path to an image file."));
    }
    #[cfg(windows)]
    if !matches!(
        path.components().next(),
        Some(std::path::Component::Prefix(prefix))
            if matches!(prefix.kind(), std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_))
    ) {
        return Err(invalid("Choose an image file on a local drive."));
    }
    let bytes = read_bounded(profile::open_preference(path)?, MAX_SOURCE_BYTES)?;
    let format = image::guess_format(&bytes).map_err(|_| format_error())?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Bmp
    ) {
        return Err(format_error());
    }
    let image = decode(&bytes, format, MAX_SOURCE_EDGE, MAX_SOURCE_PIXELS)?;
    let (max_width, max_height) = if image.width() >= image.height() {
        (LONG_EDGE, SHORT_EDGE)
    } else {
        (SHORT_EDGE, LONG_EDGE)
    };
    let pixels = if image.width() > max_width || image.height() > max_height {
        image.resize(max_width, max_height, FilterType::Triangle)
    } else {
        image
    }
    .into_rgba8();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(io::Error::other)?;
    if png.len() > MAX_STORED_BYTES {
        return Err(invalid("The normalized terminal background exceeds 4 MiB."));
    }
    Ok(Prepared {
        id: AssetId::for_png(&png)?,
        png,
        image: Image::new(pixels),
    })
}

fn assets(root: &Path, create: bool) -> io::Result<Directory> {
    Directory::open(root, create)?.private_child(DIRECTORY, create)
}

pub(crate) fn read_asset(root: &Path, id: AssetId) -> io::Result<Vec<u8>> {
    let bytes = read_verified(&assets(root, false)?, id)?;
    decode_stored(&bytes)?;
    Ok(bytes)
}

pub(crate) fn validate_asset(id: AssetId, bytes: &[u8]) -> io::Result<()> {
    require_digest(id, bytes)?;
    decode_stored(bytes)?;
    Ok(())
}

fn read_verified(directory: &Directory, id: AssetId) -> io::Result<Vec<u8>> {
    let bytes = read_bounded(
        directory.open_private_file(&id.file_name())?,
        MAX_STORED_BYTES,
    )?;
    require_digest(id, &bytes)?;
    Ok(bytes)
}

fn require_digest(id: AssetId, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > MAX_STORED_BYTES || id != AssetId::for_png(bytes)? {
        return Err(invalid(
            "The stored background does not match its identifier.",
        ));
    }
    Ok(())
}

fn decode_stored(bytes: &[u8]) -> io::Result<RgbaImage> {
    let image = decode(
        bytes,
        ImageFormat::Png,
        LONG_EDGE,
        u64::from(LONG_EDGE) * u64::from(SHORT_EDGE),
    )?;
    if image.width().min(image.height()) > SHORT_EDGE {
        return Err(invalid("The stored background has unexpected dimensions."));
    }
    Ok(image.into_rgba8())
}

fn decode(
    bytes: &[u8],
    format: ImageFormat,
    max_edge: u32,
    max_pixels: u64,
) -> io::Result<DynamicImage> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_edge);
    limits.max_image_height = Some(max_edge);
    limits.max_alloc = Some(max_pixels * 10);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(io::Error::other)?;
    let (width, height) = decoder.dimensions();
    if width == 0
        || height == 0
        || width > max_edge
        || height > max_edge
        || u64::from(width) * u64::from(height) > max_pixels
    {
        return Err(invalid(
            "The terminal background exceeds the supported image dimensions.",
        ));
    }
    limits
        .reserve(decoder.total_bytes())
        .map_err(io::Error::other)?;
    decoder.set_limits(limits).map_err(io::Error::other)?;
    DynamicImage::from_decoder(decoder).map_err(io::Error::other)
}

fn read_bounded(file: File, limit: usize) -> io::Result<Vec<u8>> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err(invalid(
            "The terminal background is not a supported regular image file.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid(
            "The terminal background exceeds the file size limit.",
        ));
    }
    Ok(bytes)
}

fn format_error() -> io::Error {
    invalid("Choose a PNG, JPEG, GIF, or BMP image.")
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[derive(Default)]
pub(crate) struct Images {
    loaded: BTreeMap<AssetId, Image>,
}

impl Images {
    pub(crate) fn refresh(&mut self, root: &Path, ids: impl Iterator<Item = AssetId>) {
        let ids = ids.collect::<BTreeSet<_>>();
        self.loaded.retain(|id, _| ids.contains(id));
        let Ok(directory) = assets(root, false) else {
            return;
        };
        for id in ids {
            if let std::collections::btree_map::Entry::Vacant(entry) = self.loaded.entry(id)
                && let Ok(bytes) = read_verified(&directory, id)
                && let Ok(pixels) = decode_stored(&bytes)
            {
                entry.insert(Image::new(pixels));
            }
        }
    }

    pub(crate) fn get(&self, id: AssetId) -> Option<&Image> {
        self.loaded.get(&id)
    }
}

/// Removes an obsolete catalogue asset during an exclusive profile restore.
pub(crate) fn retire(root: &Path, id: AssetId) -> io::Result<()> {
    let directory = match assets(root, false) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let name = id.file_name();
    let name = std::ffi::OsStr::new(&name);
    let (identity, is_directory) = match directory.deletion_entry(name) {
        Ok(entry) => entry,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !is_directory && read_verified(&directory, id).is_ok() {
        directory.remove_deletion_entry(name, identity, false)?;
        directory.sync()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn prepared_background_is_local_verified_and_retired_by_exact_digest() {
        let profile = crate::snippets::tests::Profile::new();
        let source = profile.0.join("source.png");
        let bytes = image::RgbaImage::from_pixel(8, 5, image::Rgba([210, 40, 10, 255]));
        bytes.save(&source).unwrap();

        let prepared = prepare(&source).unwrap();
        let id = prepared.id();
        let stored = profile.0.join(id.relative_path());
        assert!(!stored.exists());
        assert_eq!(AssetId::from_relative_path(&id.relative_path()), Some(id));
        assert!(AssetId::from_relative_path(&format!("{DIRECTORY}/../{id}.png")).is_none());
        assert_eq!(
            serde_json::from_str::<AssetId>(&serde_json::to_string(&id).unwrap()).unwrap(),
            id
        );
        assert!(serde_json::from_str::<AssetId>("\"../unreviewed.png\"").is_err());

        prepared.publish(&profile.0).unwrap();
        prepared.publish(&profile.0).unwrap();
        fs::remove_file(&source).unwrap();
        let bytes = read_asset(&profile.0, id).unwrap();
        validate_asset(id, &bytes).unwrap();
        assert!(validate_asset(id, &bytes[..bytes.len() - 1]).is_err());
        let mut images = Images::default();
        images.refresh(&profile.0, std::iter::once(id));
        assert_eq!(
            images.get(id).map(|image| (image.width, image.height)),
            Some((8, 5))
        );

        let unrelated = profile.0.join(DIRECTORY).join("unrelated.txt");
        fs::write(&unrelated, b"preserve").unwrap();
        retire(&profile.0, id).unwrap();
        assert!(!stored.exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"preserve");
        prepared.publish(&profile.0).unwrap();
        fs::write(&stored, b"preserve unexpected bytes").unwrap();
        retire(&profile.0, id).unwrap();
        assert_eq!(fs::read(&stored).unwrap(), b"preserve unexpected bytes");
        images.refresh(&profile.0, std::iter::empty());
        assert!(images.get(id).is_none());
    }

    #[test]
    fn rejects_unknown_formats_and_oversized_inputs_before_publication() {
        let profile = crate::snippets::tests::Profile::new();
        let source = profile.0.join("source.png");
        fs::write(&source, b"not an image").unwrap();
        assert!(prepare(&source).is_err());
        let file = File::options().write(true).open(&source).unwrap();
        file.set_len(MAX_SOURCE_BYTES as u64 + 1).unwrap();
        drop(file);
        assert!(prepare(&source).is_err());
        assert!(!profile.0.join(DIRECTORY).exists());
    }
}
