//! Direct Kitty graphics transfers and bounded, per-session image storage.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::{self, Write};
use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::alphabet;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig};
use base64::engine::{DecodePaddingMode, Engine};
use flate2::{Decompress, FlushDecompress, Status};

pub(crate) const MAX_APC_BODY: usize = 1024 * 1024;
pub(crate) const MAX_PLACEMENTS_PER_LINE: usize = 256;
const STORAGE_QUOTA: usize = 128 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 10_000;
const MAX_IMAGES: usize = 4096;
const MAX_CONTROLS: usize = 4096;
const FIRST_ASSIGNED_ID: u32 = 1 << 31;
static NEXT_IMAGE_KEY: AtomicU64 = AtomicU64::new(1);
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// One decoded image. Its key distinguishes reused protocol ids.
pub struct KittyImage {
    pub key: u64,
    pub id: u32,
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8 pixels.
    pub rgba: Box<[u8]>,
}

impl PartialEq for KittyImage {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Eq for KittyImage {}

impl fmt::Debug for KittyImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("KittyImage")
            .field("key", &self.key)
            .field("id", &self.id)
            .field("number", &self.number)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Image geometry relative to its anchor cell. Floating-point values are
/// expressed in cells so a resize need not decode or scale the stored pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct KittyPlacement {
    pub image: Arc<KittyImage>,
    pub placement_id: u32,
    pub column: u16,
    pub columns: u16,
    pub rows: u16,
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub z_index: i32,
}

#[derive(Clone, Copy)]
pub(crate) struct KittyContext {
    pub(crate) cell_width: u16,
    pub(crate) cell_height: u16,
    pub(crate) column: u16,
    pub(crate) row: u16,
}

#[derive(Default)]
pub(crate) struct KittyOutput {
    pub(crate) responses: Vec<u8>,
    pub(crate) actions: Vec<KittyAction>,
}

pub(crate) enum KittyAction {
    Place {
        placement: KittyPlacement,
        move_cursor: bool,
    },
    Erase(KittyErase),
}

pub(crate) struct KittyErase {
    pub(crate) visible_only: bool,
    pub(crate) selector: KittySelector,
    pub(crate) purge: KittyPurge,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KittyPurge {
    None,
    Removed,
    Image(u64),
    IdRange { first: u32, last: u32 },
}

pub(crate) enum KittySelector {
    All,
    Image {
        key: u64,
        placement_id: Option<u32>,
    },
    Images(HashSet<u64>),
    Cell {
        column: u32,
        row: isize,
        z_index: Option<i32>,
    },
    Column(u32),
    Row(isize),
    ZIndex(i32),
    IdRange {
        first: u32,
        last: u32,
    },
}

impl KittySelector {
    /// Tests an anchored placement. History rows are negative. An inactive
    /// screen may use an out-of-range sentinel row.
    pub(crate) fn matches(&self, placement: &KittyPlacement, row: isize) -> bool {
        let intersects_column = |column| {
            let start = u32::from(placement.column);
            (start..start + u32::from(placement.columns)).contains(&column)
        };
        let intersects_row =
            |target| (row..row.saturating_add(placement.rows as isize)).contains(&target);
        match *self {
            Self::All => true,
            Self::Image { key, placement_id } => {
                placement.image.key == key
                    && placement_id.is_none_or(|id| id == placement.placement_id)
            }
            Self::Images(ref keys) => keys.contains(&placement.image.key),
            Self::Cell {
                column,
                row: target,
                z_index,
            } => {
                intersects_column(column)
                    && intersects_row(target)
                    && z_index.is_none_or(|z| placement.z_index == z)
            }
            Self::Column(column) => intersects_column(column),
            Self::Row(target) => intersects_row(target),
            Self::ZIndex(z) => placement.z_index == z,
            Self::IdRange { first, last } => id_in_range(placement.image.id, first, last),
        }
    }
}

fn id_in_range(id: u32, first: u32, last: u32) -> bool {
    id != 0 && (first..=last).contains(&id)
}

pub(crate) struct KittyGraphics {
    images: VecDeque<Arc<KittyImage>>,
    archived: HashSet<u64>,
    transmission: Option<Transmission>,
    stored_bytes: usize,
    quota: usize,
    next_assigned_id: u32,
}

impl Default for KittyGraphics {
    fn default() -> Self {
        Self {
            images: VecDeque::new(),
            archived: HashSet::new(),
            transmission: None,
            stored_bytes: 0,
            quota: STORAGE_QUOTA,
            next_assigned_id: FIRST_ASSIGNED_ID,
        }
    }
}

impl KittyGraphics {
    /// Processes one framed APC body, including its initial G. The placement
    /// inventory is needed only if the store must evict images.
    pub(crate) fn handle(
        &mut self,
        body: &[u8],
        context: KittyContext,
        placed: impl FnOnce() -> HashSet<u64>,
    ) -> KittyOutput {
        let mut output = KittyOutput::default();
        let Some(body) = body.strip_prefix(b"G") else {
            return output;
        };
        let (controls, payload) = Controls::parse(body);

        if body.len() >= MAX_APC_BODY {
            self.transmission = None;
            respond(
                &mut output,
                &controls,
                controls.echo_id(),
                controls.quiet(),
                Err(Error::too_large()),
            );
            return output;
        }

        if let Some(mut transmission) = self.transmission.take()
            && controls.continues()
        {
            if controls.has(b'q') {
                transmission.quiet = controls.quiet();
            }
            match controls.more() {
                Ok(more) => {
                    transmission.append(payload);
                    if more {
                        self.transmission = Some(transmission);
                    } else {
                        self.finish(transmission, context, placed, &mut output);
                    }
                }
                Err(error) => respond(
                    &mut output,
                    &transmission.controls,
                    transmission.controls.echo_id(),
                    transmission.quiet,
                    Err(error),
                ),
            }
            return output;
        }

        let result = self.dispatch(controls, payload, context, placed, &mut output);
        if let Err((controls, error)) = result {
            respond(
                &mut output,
                &controls,
                controls.echo_id(),
                controls.quiet(),
                Err(error),
            );
        }
        output
    }

    fn dispatch(
        &mut self,
        controls: Controls,
        payload: &[u8],
        context: KittyContext,
        placed: impl FnOnce() -> HashSet<u64>,
        output: &mut KittyOutput,
    ) -> Result<(), (Controls, Error)> {
        let result = match controls.action() {
            Ok(action @ (b't' | b'T' | b'q' | b'f')) => {
                let more = match controls.more() {
                    Ok(more) => more,
                    Err(error) => return Err((controls, error)),
                };
                let mut transmission = Transmission::new(controls, action, self.quota);
                transmission.append(payload);
                if more {
                    self.transmission = Some(transmission);
                } else {
                    self.finish(transmission, context, placed, output);
                }
                return Ok(());
            }
            Ok(b'p') => self.place_stored(&controls, context, output),
            Ok(b'd') => self.erase(&controls, context, output),
            Ok(b'a' | b'c') => Err(Error::invalid("Animation is not supported")),
            Ok(_) => Err(Error::invalid("Unknown action")),
            Err(error) => Err(error),
        };
        result.map_err(|error| (controls, error))
    }

    fn finish(
        &mut self,
        mut transmission: Transmission,
        context: KittyContext,
        placed: impl FnOnce() -> HashSet<u64>,
        output: &mut KittyOutput,
    ) {
        transmission.finish_base64();
        let controls = transmission.controls;
        let action = transmission.action;
        let result = transmission.options.and_then(|options| {
            let decoded = decode(options, transmission.data, self.quota)?;
            if action == b'q' {
                return controls.unsigned(b'i');
            }
            let image = self.store(&controls, decoded, placed, output)?;
            if action == b'T' {
                place(&controls, image.clone(), context, output)?;
            }
            Ok(image.id)
        });
        let id = result
            .as_ref()
            .copied()
            .unwrap_or_else(|_| controls.echo_id());
        respond(
            output,
            &controls,
            id,
            transmission.quiet,
            result.map(|_| ()),
        );
    }

    fn store(
        &mut self,
        controls: &Controls,
        decoded: Decoded,
        placed: impl FnOnce() -> HashSet<u64>,
        output: &mut KittyOutput,
    ) -> Result<Arc<KittyImage>, Error> {
        let mut id = controls.unsigned(b'i')?;
        let number = controls.unsigned(b'I')?;
        if id != 0 {
            if let Some(replaced) = self.image_by_id(id) {
                self.remove_image(replaced.key);
                erase_image(output, replaced.key, None);
            }
        } else if number != 0 {
            id = self.assign_id();
        }
        self.make_room(decoded.rgba.len(), placed, output);
        let image = Arc::new(KittyImage {
            key: NEXT_IMAGE_KEY.fetch_add(1, Ordering::Relaxed),
            id,
            number,
            width: decoded.width,
            height: decoded.height,
            rgba: decoded.rgba,
        });
        self.stored_bytes += image.rgba.len();
        self.images.push_back(image.clone());
        Ok(image)
    }

    fn assign_id(&mut self) -> u32 {
        loop {
            let id = self.next_assigned_id;
            self.next_assigned_id = id.checked_add(1).unwrap_or(FIRST_ASSIGNED_ID);
            if self.image_by_id(id).is_none() {
                return id;
            }
        }
    }

    fn make_room(
        &mut self,
        bytes: usize,
        placed: impl FnOnce() -> HashSet<u64>,
        output: &mut KittyOutput,
    ) {
        if self.fits(bytes) {
            return;
        }
        let placed = placed();
        let unplaced = self
            .images
            .iter()
            .filter(|image| !placed.contains(&image.key));
        let in_use = self
            .images
            .iter()
            .filter(|image| placed.contains(&image.key));
        let mut evicted = HashSet::new();
        for image in unplaced.chain(in_use) {
            if self.images.len() - evicted.len() < MAX_IMAGES
                && self.stored_bytes + bytes <= self.quota
            {
                break;
            }
            evicted.insert(image.key);
            self.stored_bytes -= image.rgba.len();
        }
        self.images.retain(|image| !evicted.contains(&image.key));
        self.archived.retain(|key| !evicted.contains(key));
        let evicted_placements: HashSet<_> = evicted.intersection(&placed).copied().collect();
        if !evicted_placements.is_empty() {
            output.actions.push(KittyAction::Erase(KittyErase {
                visible_only: false,
                selector: KittySelector::Images(evicted_placements),
                purge: KittyPurge::None,
            }));
        }
    }

    fn fits(&self, bytes: usize) -> bool {
        self.images.len() < MAX_IMAGES && self.stored_bytes + bytes <= self.quota
    }

    /// Reuses an old immutable pixel allocation within the same image budget.
    /// Archived images remain visible but are outside the new host's namespace.
    pub(crate) fn register_history_image(&mut self, image: &Arc<KittyImage>) -> bool {
        if self.archived.contains(&image.key) {
            return true;
        }
        if !self.fits(image.rgba.len()) || self.images.iter().any(|stored| stored.key == image.key)
        {
            return false;
        }
        self.stored_bytes += image.rgba.len();
        self.archived.insert(image.key);
        self.images.push_front(Arc::clone(image));
        true
    }

    pub(crate) fn matches_erase(
        &self,
        selector: &KittySelector,
        placement: &KittyPlacement,
        row: isize,
    ) -> bool {
        // An internal quota eviction can remove imported images. A host
        // command cannot address an earlier session's imported assets.
        (matches!(selector, KittySelector::Images(_))
            || !self.archived.contains(&placement.image.key))
            && selector.matches(placement, row)
    }

    fn remove_image(&mut self, key: u64) {
        if let Some(index) = self.images.iter().position(|image| image.key == key)
            && let Some(removed) = self.images.remove(index)
        {
            self.stored_bytes -= removed.rgba.len();
        }
    }

    fn image_by_id(&self, id: u32) -> Option<Arc<KittyImage>> {
        self.images
            .iter()
            .rev()
            .find(|image| image.id == id && id != 0 && !self.archived.contains(&image.key))
            .cloned()
    }

    fn image_by_number(&self, number: u32) -> Option<Arc<KittyImage>> {
        self.images
            .iter()
            .rev()
            .find(|image| {
                image.number == number && number != 0 && !self.archived.contains(&image.key)
            })
            .cloned()
    }

    fn place_stored(
        &self,
        controls: &Controls,
        context: KittyContext,
        output: &mut KittyOutput,
    ) -> Result<(), Error> {
        let id = controls.unsigned(b'i')?;
        let number = controls.unsigned(b'I')?;
        if id == 0 && number == 0 {
            return Err(Error::invalid("No image id or number was given"));
        }
        let image = if id != 0 {
            self.image_by_id(id)
        } else {
            self.image_by_number(number)
        }
        .ok_or(Error("ENOENT", "Image not found"))?;
        let id = image.id;
        place(controls, image, context, output)?;
        respond(output, controls, id, controls.quiet(), Ok(()));
        Ok(())
    }

    fn erase(
        &self,
        controls: &Controls,
        context: KittyContext,
        output: &mut KittyOutput,
    ) -> Result<(), Error> {
        let target = controls.character(b'd', b'a')?;
        let (selector, visible_only, purge) = match target.to_ascii_lowercase() {
            b'a' => (KittySelector::All, true, KittyPurge::Removed),
            b'i' | b'n' => {
                let image = if target.eq_ignore_ascii_case(&b'i') {
                    self.image_by_id(controls.unsigned(b'i')?)
                } else {
                    self.image_by_number(controls.unsigned(b'I')?)
                };
                let placement_id = controls.unsigned(b'p')?;
                let Some(image) = image else {
                    return Ok(());
                };
                (
                    KittySelector::Image {
                        key: image.key,
                        placement_id: (placement_id != 0).then_some(placement_id),
                    },
                    false,
                    KittyPurge::Image(image.key),
                )
            }
            b'c' => (
                KittySelector::Cell {
                    column: u32::from(context.column),
                    row: context.row as isize,
                    z_index: None,
                },
                true,
                KittyPurge::Removed,
            ),
            b'p' | b'q' => {
                let x = controls.coordinate(b'x')?;
                let y = controls.coordinate(b'y')?;
                let z_index = if target.eq_ignore_ascii_case(&b'q') {
                    Some(controls.signed(b'z')?)
                } else {
                    None
                };
                let (Some(column), Some(row)) = (x, y) else {
                    return Ok(());
                };
                (
                    KittySelector::Cell {
                        column,
                        row: row as isize,
                        z_index,
                    },
                    true,
                    KittyPurge::Removed,
                )
            }
            b'x' => {
                let Some(column) = controls.coordinate(b'x')? else {
                    return Ok(());
                };
                (KittySelector::Column(column), true, KittyPurge::Removed)
            }
            b'y' => {
                let Some(row) = controls.coordinate(b'y')? else {
                    return Ok(());
                };
                (KittySelector::Row(row as isize), true, KittyPurge::Removed)
            }
            b'z' => (
                KittySelector::ZIndex(controls.signed(b'z')?),
                true,
                KittyPurge::Removed,
            ),
            b'r' => {
                let first = controls.unsigned(b'x')?;
                let last = controls.unsigned(b'y')?;
                (
                    KittySelector::IdRange { first, last },
                    false,
                    KittyPurge::IdRange { first, last },
                )
            }
            b'f' => return Ok(()),
            _ => return Err(Error::invalid("Unknown delete target")),
        };
        output.actions.push(KittyAction::Erase(KittyErase {
            visible_only,
            selector,
            purge: if target.is_ascii_uppercase() {
                purge
            } else {
                KittyPurge::None
            },
        }));
        Ok(())
    }

    /// Completes a capital-letter delete using the host's actual placement inventory.
    pub(crate) fn finish_erase(
        &mut self,
        purge: KittyPurge,
        removed: &HashSet<u64>,
        remaining: &HashSet<u64>,
    ) {
        if purge == KittyPurge::None {
            return;
        }
        self.images.retain(|image| {
            if self.archived.contains(&image.key) {
                return true;
            }
            let matches_target = match purge {
                KittyPurge::Image(key) => image.key == key,
                KittyPurge::IdRange { first, last } => id_in_range(image.id, first, last),
                KittyPurge::None | KittyPurge::Removed => false,
            };
            let remove =
                (matches_target || removed.contains(&image.key)) && !remaining.contains(&image.key);
            if remove {
                self.stored_bytes -= image.rgba.len();
            }
            !remove
        });
    }

    /// Discards stored images and any partial transfer. The host owns the anchors.
    pub(crate) fn clear(&mut self) {
        self.images.clear();
        self.archived.clear();
        self.stored_bytes = 0;
        self.abort_transfer();
    }

    /// Discards only the in-flight transfer after a dropped Kitty APC frame.
    pub(crate) fn abort_transfer(&mut self) {
        self.transmission = None;
    }
}

fn erase_image(output: &mut KittyOutput, key: u64, placement_id: Option<u32>) {
    output.actions.push(KittyAction::Erase(KittyErase {
        visible_only: false,
        selector: KittySelector::Image { key, placement_id },
        purge: KittyPurge::None,
    }));
}

fn place(
    controls: &Controls,
    image: Arc<KittyImage>,
    context: KittyContext,
    output: &mut KittyOutput,
) -> Result<(), Error> {
    if controls.unsigned(b'U')? != 0 {
        return Err(Error::invalid("Unicode placeholders are not supported"));
    }
    if controls.unsigned(b'P')? != 0 || controls.unsigned(b'Q')? != 0 {
        return Err(Error::invalid("Relative placements are not supported"));
    }
    let move_cursor = controls.unsigned(b'C')? != 1;
    let placement = layout(controls, image, context)?;
    if placement.placement_id != 0 {
        erase_image(output, placement.image.key, Some(placement.placement_id));
    }
    output.actions.push(KittyAction::Place {
        placement,
        move_cursor,
    });
    Ok(())
}

fn layout(
    controls: &Controls,
    image: Arc<KittyImage>,
    context: KittyContext,
) -> Result<KittyPlacement, Error> {
    let source_x = controls.unsigned(b'x')?.min(image.width);
    let source_y = controls.unsigned(b'y')?.min(image.height);
    let source_width = source_size(controls.unsigned(b'w')?, image.width - source_x);
    let source_height = source_size(controls.unsigned(b'h')?, image.height - source_y);
    if source_width == 0 || source_height == 0 {
        return Err(Error::invalid("Source rectangle is outside the image"));
    }

    let cell_width = f64::from(context.cell_width.max(1));
    let cell_height = f64::from(context.cell_height.max(1));
    let mut offset_x = f64::from(controls.unsigned(b'X')?).min(cell_width - 1.0);
    let mut offset_y = f64::from(controls.unsigned(b'Y')?).min(cell_height - 1.0);
    let mut columns = controls.unsigned(b'c')?.min(MAX_IMAGE_DIMENSION);
    let mut rows = controls.unsigned(b'r')?.min(MAX_IMAGE_DIMENSION);
    let src_width = f64::from(source_width);
    let src_height = f64::from(source_height);
    let (width, height) = match (columns, rows) {
        (0, 0) => (src_width, src_height),
        (_, 0) => {
            let width = (f64::from(columns) * cell_width - offset_x).max(1.0);
            (width, width * src_height / src_width)
        }
        (0, _) => {
            let height = (f64::from(rows) * cell_height - offset_y).max(1.0);
            (height * src_width / src_height, height)
        }
        (_, _) => {
            let box_width = (f64::from(columns) * cell_width - offset_x).max(1.0);
            let box_height = (f64::from(rows) * cell_height - offset_y).max(1.0);
            let scale = (box_width / src_width).min(box_height / src_height);
            let width = src_width * scale;
            let height = src_height * scale;
            offset_x += (box_width - width) / 2.0;
            offset_y += (box_height - height) / 2.0;
            (width, height)
        }
    };
    let cells_for = |pixels: f64, cell: f64| {
        (pixels / cell - 1e-9)
            .ceil()
            .clamp(1.0, f64::from(MAX_IMAGE_DIMENSION)) as u32
    };
    if columns == 0 {
        columns = cells_for(offset_x + width, cell_width);
    }
    if rows == 0 {
        rows = cells_for(offset_y + height, cell_height);
    }
    let placement_id = if image.id == 0 {
        0
    } else {
        controls.unsigned(b'p')?
    };
    Ok(KittyPlacement {
        image,
        placement_id,
        column: context.column,
        columns: columns as u16,
        rows: rows as u16,
        source_x,
        source_y,
        source_width,
        source_height,
        left: offset_x / cell_width,
        top: offset_y / cell_height,
        width: width / cell_width,
        height: height / cell_height,
        z_index: controls.signed(b'z')?,
    })
}

fn source_size(requested: u32, available: u32) -> u32 {
    if requested == 0 {
        available
    } else {
        requested.min(available)
    }
}

#[derive(Clone, Copy)]
struct Error(&'static str, &'static str);

impl Error {
    const fn invalid(message: &'static str) -> Self {
        Self("EINVAL", message)
    }

    const fn too_large() -> Self {
        Self("EFBIG", "Image data is too large")
    }

    const fn out_of_memory() -> Self {
        Self("ENOMEM", "Out of memory")
    }

    const fn bad_png() -> Self {
        Self("EBADPNG", "Failed to decode the PNG image")
    }
}

fn respond(
    output: &mut KittyOutput,
    controls: &Controls,
    id: u32,
    quiet: u32,
    status: Result<(), Error>,
) {
    if quiet >= if status.is_ok() { 1 } else { 2 } {
        return;
    }
    let number = controls.unsigned(b'I').unwrap_or(0);
    if id == 0 && number == 0 {
        return;
    }
    let mut response = String::from("\x1b_G");
    if id != 0 {
        let _ = write!(response, "i={id}");
    }
    if number != 0 {
        if id != 0 {
            response.push(',');
        }
        let _ = write!(response, "I={number}");
    }
    let placement = controls.unsigned(b'p').unwrap_or(0);
    if id != 0 && placement != 0 {
        let _ = write!(response, ",p={placement}");
    }
    response.push(';');
    match status {
        Ok(()) => response.push_str("OK"),
        Err(Error(code, message)) => {
            let _ = write!(response, "{code}:{message}");
        }
    }
    response.push_str("\x1b\\");
    output.responses.extend_from_slice(response.as_bytes());
}

#[derive(Default)]
struct Controls {
    values: BTreeMap<u8, String>,
    malformed: bool,
}

impl Controls {
    fn parse(body: &[u8]) -> (Self, &[u8]) {
        let (controls, payload) = match body.iter().position(|&byte| byte == b';') {
            Some(split) => (&body[..split], &body[split + 1..]),
            None => (body, &[][..]),
        };
        let mut command = Self::default();
        if controls.len() > MAX_CONTROLS {
            command.malformed = true;
            return (command, payload);
        }
        for pair in controls
            .split(|&byte| byte == b',')
            .filter(|pair| !pair.is_empty())
        {
            let [key, b'=', value @ ..] = pair else {
                command.malformed = true;
                continue;
            };
            if !key.is_ascii_alphabetic()
                || value.len() > 32
                || !value.iter().all(|byte| byte.is_ascii_graphic())
            {
                command.malformed = true;
                continue;
            }
            if let Ok(value) = std::str::from_utf8(value) {
                command.values.insert(*key, value.to_owned());
            }
        }
        (command, payload)
    }

    fn has(&self, key: u8) -> bool {
        self.values.contains_key(&key)
    }

    fn character(&self, key: u8, default: u8) -> Result<u8, Error> {
        match self.values.get(&key).map(|value| value.as_bytes()) {
            None => Ok(default),
            Some([byte]) => Ok(*byte),
            Some(_) => Err(Error::invalid("Invalid control character")),
        }
    }

    fn unsigned(&self, key: u8) -> Result<u32, Error> {
        match self.values.get(&key) {
            None => Ok(0),
            Some(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) => {
                value
                    .parse()
                    .map_err(|_| Error::invalid("Invalid unsigned control value"))
            }
            Some(_) => Err(Error::invalid("Invalid unsigned control value")),
        }
    }

    fn signed(&self, key: u8) -> Result<i32, Error> {
        self.values.get(&key).map_or(Ok(0), |value| {
            value
                .parse()
                .map_err(|_| Error::invalid("Invalid signed control value"))
        })
    }

    fn coordinate(&self, key: u8) -> Result<Option<u32>, Error> {
        Ok(self.unsigned(key)?.checked_sub(1))
    }

    fn quiet(&self) -> u32 {
        self.unsigned(b'q').unwrap_or(0).min(2)
    }

    fn echo_id(&self) -> u32 {
        self.unsigned(b'i').unwrap_or(0)
    }

    fn action(&self) -> Result<u8, Error> {
        if self.malformed {
            return Err(Error::invalid("Malformed control data"));
        }
        if self.unsigned(b'i')? != 0 && self.unsigned(b'I')? != 0 {
            return Err(Error::invalid(
                "Both an image id and an image number were given",
            ));
        }
        self.character(b'a', b't')
    }

    fn more(&self) -> Result<bool, Error> {
        match self.unsigned(b'm')? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::invalid("Invalid continuation flag")),
        }
    }

    fn continues(&self) -> bool {
        !self.malformed && self.values.keys().all(|key| matches!(key, b'm' | b'q'))
    }
}

#[derive(Clone, Copy)]
enum ImageFormat {
    Rgb,
    Rgba,
    Png,
}

struct TransferOptions {
    format: ImageFormat,
    compressed: bool,
    width: u32,
    height: u32,
    limit: usize,
    truncate: bool,
}

impl TransferOptions {
    fn parse(controls: &Controls, action: u8, quota: usize) -> Result<Self, Error> {
        if action == b'f' {
            return Err(Error::invalid("Animation frames are not supported"));
        }
        if action == b'q' && controls.has(b'e') {
            return Err(Error::invalid("Feature queries are not supported"));
        }
        if controls.character(b't', b'd')? != b'd' {
            return Err(Error::invalid("Only direct transmission is supported"));
        }
        let format = match controls.unsigned(b'f')? {
            24 => ImageFormat::Rgb,
            32 => ImageFormat::Rgba,
            100 => ImageFormat::Png,
            0 if !controls.has(b'f') => ImageFormat::Rgba,
            _ => return Err(Error::invalid("Unknown image format")),
        };
        let compressed = match controls.character(b'o', 0)? {
            0 => false,
            b'z' => true,
            _ => return Err(Error::invalid("Unknown compression")),
        };
        let width = controls.unsigned(b's')?;
        let height = controls.unsigned(b'v')?;
        let truncate = !compressed && !matches!(format, ImageFormat::Png);
        let raw_length = match format {
            ImageFormat::Rgb => check_size(width, height, quota)? / 4 * 3,
            ImageFormat::Rgba => check_size(width, height, quota)?,
            ImageFormat::Png => quota,
        };
        Ok(Self {
            format,
            compressed,
            width,
            height,
            limit: if truncate { raw_length } else { quota },
            truncate,
        })
    }
}

struct Transmission {
    controls: Controls,
    action: u8,
    options: Result<TransferOptions, Error>,
    quiet: u32,
    data: Vec<u8>,
    tail: [u8; 4],
    tail_len: usize,
    padding_seen: bool,
}

impl Transmission {
    fn new(controls: Controls, action: u8, quota: usize) -> Self {
        let quiet = controls.quiet();
        let options = TransferOptions::parse(&controls, action, quota);
        Self {
            controls,
            action,
            options,
            quiet,
            data: Vec::new(),
            tail: [0; 4],
            tail_len: 0,
            padding_seen: false,
        }
    }

    fn append(&mut self, mut payload: &[u8]) {
        if self.options.is_err() || payload.is_empty() {
            return;
        }
        if self.tail_len != 0 {
            let take = (4 - self.tail_len).min(payload.len());
            self.tail[self.tail_len..self.tail_len + take].copy_from_slice(&payload[..take]);
            self.tail_len += take;
            payload = &payload[take..];
            if self.tail_len < 4 {
                return;
            }
            self.tail_len = 0;
            let tail = self.tail;
            self.decode_base64(&tail);
        }
        let whole = payload.len() / 4 * 4;
        for chunk in payload[..whole].chunks(4096) {
            self.decode_base64(chunk);
        }
        let tail = &payload[whole..];
        self.tail[..tail.len()].copy_from_slice(tail);
        self.tail_len = tail.len();
    }

    fn finish_base64(&mut self) {
        if self.tail_len != 0 {
            let tail = self.tail;
            self.decode_base64(&tail[..self.tail_len]);
            self.tail_len = 0;
        }
    }

    fn decode_base64(&mut self, input: &[u8]) {
        let Ok(options) = &self.options else {
            return;
        };
        if self.padding_seen {
            self.options = Err(Error::invalid("Unexpected data after base64 padding"));
            return;
        }
        let mut decoded = [0; 3072];
        let Ok(length) = BASE64.decode_slice(input, &mut decoded) else {
            self.options = Err(Error::invalid("Payload is not base64"));
            return;
        };
        self.padding_seen = input.contains(&b'=');
        let available = options.limit - self.data.len();
        if length > available && !options.truncate {
            self.options = Err(Error::too_large());
            return;
        }
        let length = length.min(available);
        if let Err(error) = reserve_bounded(&mut self.data, length, options.limit) {
            self.options = Err(error);
            return;
        }
        self.data.extend_from_slice(&decoded[..length]);
    }
}

struct Decoded {
    width: u32,
    height: u32,
    rgba: Box<[u8]>,
}

fn check_size(width: u32, height: u32, quota: usize) -> Result<usize, Error> {
    if width == 0 || height == 0 {
        return Err(Error::invalid("Image width and height must be given"));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(Error::too_large());
    }
    let bytes = width as usize * height as usize * 4;
    if bytes > quota {
        return Err(Error::too_large());
    }
    Ok(bytes)
}

fn reserve_bounded(buffer: &mut Vec<u8>, additional: usize, limit: usize) -> Result<(), Error> {
    let needed = buffer.len() + additional;
    if needed > buffer.capacity() {
        let target = (buffer.capacity() * 2).max(needed).min(limit);
        buffer
            .try_reserve_exact(target - buffer.len())
            .map_err(|_| Error::out_of_memory())?;
    }
    Ok(())
}

fn decode(options: TransferOptions, data: Vec<u8>, quota: usize) -> Result<Decoded, Error> {
    let (format, width, height) = (options.format, options.width, options.height);
    if matches!(format, ImageFormat::Png) {
        let data = if options.compressed {
            inflate(&data, quota)?
        } else {
            data
        };
        return decode_png(&data, quota);
    }

    let output_length = check_size(width, height, quota)?;
    let channels = if matches!(format, ImageFormat::Rgb) {
        3
    } else {
        4
    };
    let expected = output_length / 4 * channels;
    let mut raw = if options.compressed {
        inflate(&data, expected)?
    } else {
        data
    };
    if raw.len() < expected {
        return Err(Error("ENODATA", "Insufficient image data"));
    }
    raw.truncate(expected);
    let rgba = if channels == 4 {
        raw.into_boxed_slice()
    } else {
        convert_pixels(&raw, png::ColorType::Rgb, output_length)?
    };
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

fn decode_png(data: &[u8], quota: usize) -> Result<Decoded, Error> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(data),
        png::Limits {
            bytes: 16 * 1024 * 1024,
        },
    );
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_ignore_text_chunk(true);
    let header = decoder.read_header_info().map_err(|_| Error::bad_png())?;
    let (width, height) = (header.width, header.height);
    let output_length = check_size(width, height, quota)?;
    let mut reader = decoder.read_info().map_err(|_| Error::bad_png())?;
    if reader.info().animation_control.is_some() {
        return Err(Error("EBADPNG", "Animated PNG is not supported"));
    }
    let buffer_size = reader.output_buffer_size().ok_or(Error::too_large())?;
    if buffer_size > output_length {
        return Err(Error::too_large());
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(buffer_size)
        .map_err(|_| Error::out_of_memory())?;
    pixels.resize(buffer_size, 0);
    let frame = reader
        .next_frame(&mut pixels)
        .map_err(|_| Error::bad_png())?;
    if (frame.width, frame.height) != (width, height) || frame.bit_depth != png::BitDepth::Eight {
        return Err(Error::bad_png());
    }
    reader.finish().map_err(|_| Error::bad_png())?;
    pixels.truncate(frame.buffer_size());
    let rgba = if frame.color_type == png::ColorType::Rgba {
        pixels.into_boxed_slice()
    } else {
        convert_pixels(&pixels, frame.color_type, output_length)?
    };
    Ok(Decoded {
        width,
        height,
        rgba,
    })
}

fn convert_pixels(
    raw: &[u8],
    color: png::ColorType,
    output_length: usize,
) -> Result<Box<[u8]>, Error> {
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(output_length)
        .map_err(|_| Error::out_of_memory())?;
    match color {
        png::ColorType::Rgb => {
            for pixel in raw.as_chunks::<3>().0 {
                rgba.extend_from_slice(pixel);
                rgba.push(255);
            }
        }
        png::ColorType::Grayscale => {
            for &gray in raw {
                rgba.extend_from_slice(&[gray, gray, gray, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for pixel in raw.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]]);
            }
        }
        _ => return Err(Error::bad_png()),
    }
    if rgba.len() != output_length {
        return Err(Error::bad_png());
    }
    Ok(rgba.into_boxed_slice())
}

fn inflate(input: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
    let mut stream = Decompress::new(true);
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        let read_before = stream.total_in() as usize;
        let written_before = stream.total_out() as usize;
        let capacity = (limit + 1 - output.len()).min(chunk.len());
        let status = stream
            .decompress(
                &input[read_before..],
                &mut chunk[..capacity],
                FlushDecompress::None,
            )
            .map_err(|_| Error::invalid("Data is not a valid zlib stream"))?;
        let written = stream.total_out() as usize - written_before;
        if output.len() + written > limit {
            return Err(Error::too_large());
        }
        reserve_bounded(&mut output, written, limit)?;
        output.extend_from_slice(&chunk[..written]);
        if status == Status::StreamEnd {
            if stream.total_in() as usize != input.len() {
                return Err(Error::invalid("Unexpected data after zlib stream"));
            }
            return Ok(output);
        }
        if stream.total_in() as usize == read_before && written == 0 {
            return Err(Error::invalid("Compressed image data is truncated"));
        }
    }
}

#[cfg(test)]
mod tests {
    use flate2::{Compression, write::ZlibEncoder};
    use std::io::Write as _;

    use super::*;

    const CONTEXT: KittyContext = KittyContext {
        cell_width: 10,
        cell_height: 20,
        column: 3,
        row: 4,
    };

    fn command(graphics: &mut KittyGraphics, controls: &str, payload: &str) -> KittyOutput {
        let body = format!("G{controls};{payload}");
        graphics.handle(body.as_bytes(), CONTEXT, HashSet::new)
    }

    fn transmit(graphics: &mut KittyGraphics, controls: &str, pixels: &[u8]) -> KittyOutput {
        command(graphics, controls, &BASE64.encode(pixels))
    }

    fn response(ids: &str, status: &str) -> Vec<u8> {
        format!("\x1b_G{ids};{status}\x1b\\").into_bytes()
    }

    fn placements(output: &KittyOutput) -> Vec<(&KittyPlacement, bool)> {
        output
            .actions
            .iter()
            .filter_map(|action| match action {
                KittyAction::Place {
                    placement,
                    move_cursor,
                } => Some((placement, *move_cursor)),
                KittyAction::Erase(_) => None,
            })
            .collect()
    }

    fn only_erase(output: &KittyOutput) -> &KittyErase {
        match output.actions.as_slice() {
            [KittyAction::Erase(erase)] => erase,
            _ => panic!("expected a single erase action"),
        }
    }

    #[test]
    fn direct_query_is_answered_but_never_stored() {
        let mut graphics = KittyGraphics::default();
        let output = command(&mut graphics, "i=31,s=1,v=1,a=q,t=d,f=24", "AAAA");
        assert_eq!(output.responses, response("i=31", "OK"));
        assert!(output.actions.is_empty());
        assert!(graphics.images.is_empty());
        let output = graphics.handle(b"unrelated", CONTEXT, HashSet::new);
        assert!(output.responses.is_empty() && output.actions.is_empty());
    }

    #[test]
    fn split_base64_is_acknowledged_once_and_preserves_rgba() {
        let mut graphics = KittyGraphics::default();
        let pixels = [0, 255, 0, 128].repeat(12);
        let payload = BASE64.encode(&pixels);
        for (index, chunk) in payload.as_bytes().chunks(7).enumerate() {
            let more = (index + 1) * 7 < payload.len();
            let controls = if index == 0 {
                "a=T,i=9,f=32,s=3,v=4,q=2,m=1"
            } else if more {
                "m=1"
            } else {
                "m=0,q=0"
            };
            let output = command(&mut graphics, controls, std::str::from_utf8(chunk).unwrap());
            if more {
                assert!(output.responses.is_empty() && output.actions.is_empty());
            } else {
                assert_eq!(output.responses, response("i=9", "OK"));
                let placed = placements(&output);
                assert_eq!(placed[0].0.image.rgba.as_ref(), pixels);
                assert!(placed[0].1);
            }
        }
    }

    #[test]
    fn another_command_cancels_a_transfer_and_a_bad_chunk_reports_on_completion() {
        let mut graphics = KittyGraphics::default();
        assert!(
            command(&mut graphics, "a=T,i=9,f=32,s=3,v=4,m=1", "AAAA")
                .responses
                .is_empty()
        );
        command(&mut graphics, "a=d,d=A", "");
        command(&mut graphics, "m=0", "AAAA");
        assert!(graphics.images.is_empty());

        assert!(
            command(&mut graphics, "a=t,i=9,s=1,v=1,m=1", "!!!!")
                .responses
                .is_empty()
        );
        let output = command(&mut graphics, "m=0", "AAAA");
        assert!(output.responses.starts_with(b"\x1b_Gi=9;EINVAL:"));
        assert!(graphics.images.is_empty());
    }

    #[test]
    fn raw_rgb_gains_alpha_and_extra_bytes_are_discarded() {
        let mut graphics = KittyGraphics::default();
        let output = transmit(
            &mut graphics,
            "a=t,i=3,f=24,s=1,v=1",
            &[10, 20, 30, 40, 50, 60],
        );
        assert_eq!(output.responses, response("i=3", "OK"));
        assert_eq!(graphics.stored_bytes, 4);
        assert_eq!(graphics.images[0].rgba.as_ref(), &[10, 20, 30, 255]);
        let output = command(&mut graphics, "a=t,i=4,f=24,s=1,v=1", "ChQe");
        assert_eq!(output.responses, response("i=4", "OK"));
    }

    #[test]
    fn zlib_is_bounded_and_requires_one_complete_stream() {
        let mut graphics = KittyGraphics::default();
        let pixels = [1, 2, 3, 255].repeat(1024);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&pixels).unwrap();
        let mut compressed = encoder.finish().unwrap();

        let output = transmit(&mut graphics, "a=t,i=3,f=32,o=z,s=32,v=32", &compressed);
        assert_eq!(output.responses, response("i=3", "OK"));
        assert_eq!(graphics.images[0].rgba.as_ref(), pixels);

        let output = transmit(&mut graphics, "a=t,i=4,f=32,o=z,s=1,v=1", &compressed);
        assert!(output.responses.starts_with(b"\x1b_Gi=4;EFBIG:"));
        let output = transmit(
            &mut graphics,
            "a=t,i=5,f=32,o=z,s=32,v=32",
            &compressed[..compressed.len() - 1],
        );
        assert!(output.responses.starts_with(b"\x1b_Gi=5;EINVAL:"));
        compressed.push(0);
        let output = transmit(&mut graphics, "a=t,i=6,f=32,o=z,s=32,v=32", &compressed);
        assert!(output.responses.starts_with(b"\x1b_Gi=6;EINVAL:"));
        assert_eq!(graphics.images.len(), 1);
    }

    #[test]
    fn png_expands_to_rgba_and_validates_the_complete_image() {
        let mut encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut encoded, 2, 1);
        encoder.set_color(png::ColorType::Indexed);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_palette(vec![20, 40, 60, 80, 100, 120]);
        encoder.set_trns(vec![140]);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0, 1]).unwrap();
        writer.finish().unwrap();

        let mut graphics = KittyGraphics::default();
        let output = transmit(&mut graphics, "a=t,i=1,f=100", &encoded);
        assert_eq!(output.responses, response("i=1", "OK"));
        let image = &graphics.images[0];
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba.as_ref(), &[20, 40, 60, 140, 80, 100, 120, 255]);

        let output = transmit(&mut graphics, "a=t,i=2,f=100", &[1, 2, 3, 4]);
        assert!(output.responses.starts_with(b"\x1b_Gi=2;EBADPNG:"));
        let last = encoded.len() - 1;
        encoded[last] ^= 1;
        let output = transmit(&mut graphics, "a=t,i=3,f=100", &encoded);
        assert!(output.responses.starts_with(b"\x1b_Gi=3;EBADPNG:"));
        assert_eq!(graphics.images.len(), 1);
    }

    #[test]
    fn image_numbers_get_assigned_ids_and_resolve_the_latest_image() {
        let mut graphics = KittyGraphics::default();
        let first = transmit(&mut graphics, "a=t,I=13,s=1,v=1", &[1; 4]);
        let first_id = graphics.images[0].id;
        assert_ne!(first_id, 0);
        assert_eq!(
            first.responses,
            response(&format!("i={first_id},I=13"), "OK")
        );

        transmit(&mut graphics, "a=t,I=13,s=1,v=1", &[2; 4]);
        let second_id = graphics.images[1].id;
        assert_ne!(first_id, second_id);
        let placed = command(&mut graphics, "a=p,I=13,p=7", "");
        assert_eq!(
            placed.responses,
            response(&format!("i={second_id},I=13,p=7"), "OK")
        );
        assert_eq!(placements(&placed)[0].0.placement_id, 7);
        assert_eq!(placements(&placed)[0].0.image.rgba.as_ref(), &[2; 4]);
    }

    #[test]
    fn quiet_levels_and_untrusted_control_values_do_not_leak() {
        let mut graphics = KittyGraphics::default();
        let success = transmit(&mut graphics, "a=t,i=1,q=1,s=1,v=1", &[0; 4]);
        assert!(success.responses.is_empty());
        let failure = command(&mut graphics, "a=p,i=99,q=1", "");
        assert!(failure.responses.starts_with(b"\x1b_Gi=99;ENOENT:"));
        assert!(
            command(&mut graphics, "a=p,i=99,q=2", "")
                .responses
                .is_empty()
        );
        let output = command(&mut graphics, "a=p,i=1,z=\x1b[31m", "");
        assert!(output.responses.starts_with(b"\x1b_Gi=1;EINVAL:"));
        assert_eq!(
            output
                .responses
                .iter()
                .filter(|&&byte| byte == b'\x1b')
                .count(),
            2
        );
        assert!(
            command(&mut graphics, "a=p,i=non-numeric", "")
                .responses
                .is_empty()
        );
        let output = transmit(&mut graphics, "a=T,s=1,v=1", &[0; 4]);
        assert!(output.responses.is_empty());
        assert_eq!(placements(&output).len(), 1);
    }

    #[test]
    fn invalid_or_unsupported_commands_leave_existing_images_intact() {
        let mut graphics = KittyGraphics::default();
        transmit(&mut graphics, "a=t,i=1,s=1,v=1", &[1; 4]);
        for (controls, payload, error) in [
            ("a=p,i=1,I=2", "", "\x1b_Gi=1,I=2;EINVAL:"),
            ("a=t,i=1,t=f", "L2V0Yy9wYXNzd2Q=", "\x1b_Gi=1;EINVAL:"),
            ("a=t,i=1,t=t", "", "\x1b_Gi=1;EINVAL:"),
            ("a=t,i=1,t=s", "", "\x1b_Gi=1;EINVAL:"),
            ("a=t,i=1,f=32,s=2,v=2", "AAAA", "\x1b_Gi=1;ENODATA:"),
            ("a=t,i=1,f=24,s=1,v=1", "!!!!", "\x1b_Gi=1;EINVAL:"),
            ("a=t,i=1,f=24,s=1,v=1", "AAA=AAAA", "\x1b_Gi=1;EINVAL:"),
            ("a=t,i=1,f=24,s=20000,v=1", "AAAA", "\x1b_Gi=1;EFBIG:"),
            ("a=f,i=1,f=24,s=1,v=1", "AAAA", "\x1b_Gi=1;EINVAL:"),
            ("a=a,i=1,s=3", "", "\x1b_Gi=1;EINVAL:"),
            ("a=p,i=1,U=1,c=2,r=2", "", "\x1b_Gi=1;EINVAL:"),
            ("a=p,i=1,P=1,Q=1", "", "\x1b_Gi=1;EINVAL:"),
        ] {
            let output = command(&mut graphics, controls, payload);
            assert!(output.responses.starts_with(error.as_bytes()), "{controls}");
            assert!(output.actions.is_empty());
        }
        assert_eq!(graphics.images.len(), 1);
        assert_eq!(graphics.images[0].rgba.as_ref(), &[1; 4]);
    }

    #[test]
    fn placement_crops_fits_and_replaces_the_same_named_placement() {
        let mut graphics = KittyGraphics::default();
        transmit(&mut graphics, "a=t,i=1,s=40,v=40,q=1", &[0; 40 * 40 * 4]);

        for sizing in ["c=2", "r=1", "c=4,r=1"] {
            let output = command(&mut graphics, &format!("a=p,i=1,{sizing}"), "");
            let (placement, moves) = placements(&output)[0];
            assert!(moves);
            assert_eq!(placement.column, 3);
            assert_eq!(placement.rows, 1);
            assert_eq!((placement.width, placement.height), (2.0, 1.0));
            if sizing == "c=4,r=1" {
                assert_eq!(placement.left, 1.0);
                assert_eq!(placement.columns, 4);
            } else {
                assert_eq!(placement.columns, 2);
            }
        }

        let output = command(
            &mut graphics,
            "a=p,i=1,p=5,x=10,y=20,w=10,h=10,X=3,Y=5,z=-1,C=1",
            "",
        );
        let [
            KittyAction::Erase(erase),
            KittyAction::Place {
                placement,
                move_cursor,
            },
        ] = output.actions.as_slice()
        else {
            panic!("expected erase before place");
        };
        assert!(!move_cursor && !erase.visible_only);
        assert!(erase.selector.matches(placement, 99));
        assert_eq!(output.responses, response("i=1,p=5", "OK"));
        assert_eq!(
            (
                placement.source_x,
                placement.source_y,
                placement.source_width,
                placement.source_height
            ),
            (10, 20, 10, 10)
        );
        assert_eq!((placement.left, placement.top), (0.3, 0.25));
        assert_eq!(
            (placement.columns, placement.rows, placement.z_index),
            (2, 1, -1)
        );
    }

    #[test]
    fn replacement_and_eviction_report_actions_for_old_anchors() {
        let mut graphics = KittyGraphics {
            quota: 8,
            ..KittyGraphics::default()
        };
        let output = transmit(&mut graphics, "a=T,i=1,s=1,v=1", &[1; 4]);
        let first = placements(&output)[0].0.clone();
        transmit(&mut graphics, "a=t,i=2,s=1,v=1", &[2; 4]);

        let output = graphics.handle(b"Ga=t,i=3,s=1,v=1;AwMDAw==", CONTEXT, || {
            HashSet::from([first.image.key])
        });
        assert!(output.actions.is_empty());
        assert_eq!(
            graphics
                .images
                .iter()
                .map(|image| image.id)
                .collect::<Vec<_>>(),
            [1, 3]
        );

        let third_key = graphics.images[1].key;
        let output = graphics.handle(b"Ga=t,i=4,s=1,v=1;BAQEBA==", CONTEXT, || {
            HashSet::from([first.image.key, third_key])
        });
        let erase = only_erase(&output);
        assert!(!erase.visible_only && erase.selector.matches(&first, -30));
        assert_eq!(graphics.stored_bytes, 8);
        assert_eq!(
            graphics
                .images
                .iter()
                .map(|image| image.id)
                .collect::<Vec<_>>(),
            [3, 4]
        );

        let output = command(&mut graphics, "a=p,i=3", "");
        let previous = placements(&output)[0].0.clone();
        let output = transmit(&mut graphics, "a=t,i=3,s=1,v=1", &[9; 4]);
        assert!(only_erase(&output).selector.matches(&previous, -30));
        assert_ne!(graphics.images[1].key, previous.image.key);
        assert_ne!(graphics.images[1], previous.image);
    }

    #[test]
    fn delete_targets_erase_placements_and_capitalized_selectors_can_free_data() {
        let mut graphics = KittyGraphics::default();
        let output = transmit(&mut graphics, "a=T,i=1,p=2,s=1,v=1,c=2,r=2,z=-1", &[0; 4]);
        let placement = placements(&output)[0].0.clone();
        let key = placement.image.key;

        for controls in [
            "a=d,d=a",
            "a=d,d=i,i=1,p=2",
            "a=d,d=c",
            "a=d,d=p,x=5,y=6",
            "a=d,d=q,x=5,y=6,z=-1",
            "a=d,d=x,x=5",
            "a=d,d=y,y=6",
            "a=d,d=z,z=-1",
            "a=d,d=r,x=1,y=4",
        ] {
            let output = command(&mut graphics, controls, "");
            assert!(output.responses.is_empty());
            let erase = only_erase(&output);
            assert!(erase.selector.matches(&placement, 4), "{controls}");
            assert_eq!(erase.purge, KittyPurge::None);
        }
        for controls in [
            "a=d,d=i,i=1,p=3",
            "a=d,d=p,x=6,y=6",
            "a=d,d=q,x=5,y=6,z=0",
            "a=d,d=r,x=2,y=4",
        ] {
            let output = command(&mut graphics, controls, "");
            assert!(
                !only_erase(&output).selector.matches(&placement, 4),
                "{controls}"
            );
        }

        let output = command(&mut graphics, "a=d,d=I,i=1,p=2", "");
        let erase = only_erase(&output);
        assert!(!erase.visible_only);
        graphics.finish_erase(erase.purge, &HashSet::from([key]), &HashSet::from([key]));
        assert_eq!(graphics.stored_bytes, 4);
        graphics.finish_erase(erase.purge, &HashSet::from([key]), &HashSet::new());
        assert_eq!(graphics.stored_bytes, 0);
        let output = command(&mut graphics, "a=p,i=1", "");
        assert!(output.responses.starts_with(b"\x1b_Gi=1;ENOENT:"));
    }

    #[test]
    fn byte_and_count_quotas_and_reset_are_bounded() {
        let mut graphics = KittyGraphics::default();
        for id in 1..=MAX_IMAGES + 4 {
            transmit(&mut graphics, &format!("a=t,i={id},q=2,s=1,v=1"), &[0; 4]);
        }
        assert_eq!(graphics.images.len(), MAX_IMAGES);
        assert_eq!(graphics.images[0].id, 5);

        let output = command(&mut graphics, "a=t,i=1,s=10000,v=10000", "");
        assert!(output.responses.starts_with(b"\x1b_Gi=1;EFBIG:"));
        command(&mut graphics, "a=t,i=1,s=1,v=1,m=1", "AAAA");
        graphics.clear();
        assert!(graphics.images.is_empty() && graphics.transmission.is_none());
        assert_eq!(graphics.stored_bytes, 0);
    }
}
