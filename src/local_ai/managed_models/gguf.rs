//! Bounded metadata validation and context discovery for GGUF files.

use std::fs::File;
use std::io::{self, BufReader, Read};
use std::sync::atomic::AtomicBool;

use super::{check_cancelled, invalid};

pub(super) const MIN_CONTEXT: u32 = 512;
pub(super) const MAX_CONTEXT: u32 = 2_097_152;
const DEFAULT_CONTEXT: u32 = 32_768;
const MAX_METADATA_BYTES: u64 = 512 * 1024 * 1024;
const MAX_METADATA_ENTRIES: u64 = 100_000;
const MAX_TENSORS: u64 = 1_000_000;
const MAX_ARRAY_ITEMS: u64 = 10_000_000;
const MAX_KEY_BYTES: u64 = 65_535;
const MAX_TENSOR_NAME_BYTES: u64 = 63;
const MAX_STRING_BYTES: u64 = 16 * 1024 * 1024;
const DEFAULT_ALIGNMENT: u64 = 32;
const STRING: u32 = 8;
const ARRAY: u32 = 9;

pub(super) fn context(file: &mut File, bytes: u64, cancelled: &AtomicBool) -> io::Result<u32> {
    inspect(file, bytes, Kind::Single, cancelled).map(|inspection| inspection.context)
}

pub(super) fn projector(file: &mut File, bytes: u64, cancelled: &AtomicBool) -> io::Result<()> {
    inspect(file, bytes, Kind::Projector, cancelled).map(|_| ())
}

pub(super) struct Inspection {
    pub(super) context: u32,
    pub(super) tensors: u64,
    pub(super) total_tensors: u64,
}

pub(super) fn inspect_split(
    file: &mut File,
    bytes: u64,
    index: usize,
    count: usize,
    cancelled: &AtomicBool,
) -> io::Result<Inspection> {
    inspect(file, bytes, Kind::Split { index, count }, cancelled)
}

enum Kind {
    Single,
    Split { index: usize, count: usize },
    Projector,
}

fn inspect(
    file: &mut File,
    bytes: u64,
    kind: Kind,
    cancelled: &AtomicBool,
) -> io::Result<Inspection> {
    let split = match kind {
        Kind::Split { index, count } => Some((index, count)),
        _ => None,
    };
    let projector = matches!(kind, Kind::Projector);
    let scan_bytes = bytes.min(MAX_METADATA_BYTES);
    let mut reader = Metadata {
        input: BufReader::new(file),
        remaining: scan_bytes,
        array_items: MAX_ARRAY_ITEMS,
        cancelled,
    };
    if reader.read::<4>()? != *b"GGUF" {
        return Err(invalid("File is not a GGUF model"));
    }
    if !matches!(reader.u32()?, 2..=3) {
        return Err(invalid("Unsupported GGUF version"));
    }
    let tensors = reader.u64()?;
    let entries = reader.u64()?;
    if tensors > MAX_TENSORS
        || (tensors == 0 && !split.is_some_and(|(index, _)| index == 0))
        || entries > MAX_METADATA_ENTRIES
    {
        return Err(invalid("Invalid GGUF tensor or metadata count"));
    }
    let mut context = None;
    let mut alignment = DEFAULT_ALIGNMENT;
    let mut split_keys = [None; 3];
    let mut architecture = None;
    let mut vision = None;
    for _ in 0..entries {
        let key = reader.key()?;
        let kind = reader.u32()?;
        let split_key = match key.as_str() {
            "split.no" => Some((0, 2)),
            "split.count" => Some((1, 2)),
            "split.tensors.count" => Some((2, 5)),
            _ => None,
        };
        if let Some((slot, expected_kind)) = split_key.filter(|_| split.is_some()) {
            if kind != expected_kind || split_keys[slot].is_some() {
                return Err(invalid("Invalid or duplicate GGUF shard metadata"));
            }
            split_keys[slot] = Some(reader.integer(kind)?);
        } else if projector && key == "general.architecture" {
            if kind != STRING || architecture.is_some() {
                return Err(invalid("Invalid GGUF projector architecture metadata"));
            }
            let length = reader.u64()?;
            architecture = Some(if length == 4 {
                reader.read::<4>()? == *b"clip"
            } else {
                reader.skip(length)?;
                false
            });
        } else if projector && key == "clip.has_vision_encoder" {
            if kind != 7 || vision.is_some() {
                return Err(invalid("Invalid GGUF vision encoder metadata"));
            }
            vision = Some(reader.read::<1>()? == [1]);
        } else if key == "general.alignment" {
            if kind != 4 {
                return Err(invalid("Invalid GGUF tensor alignment type"));
            }
            alignment = u64::from(reader.u32()?);
            if !alignment.is_power_of_two() {
                return Err(invalid("Invalid GGUF tensor alignment"));
            }
        } else if integer(kind)
            && (key.ends_with(".context_length")
                || matches!(key.as_str(), "split.count" | "split.no"))
        {
            let value = reader.integer(kind)?;
            match key.as_str() {
                "split.count" if value > 1 => {
                    return Err(invalid("Import a complete single-file GGUF model"));
                }
                "split.no" if value > 0 => {
                    return Err(invalid("Import a complete single-file GGUF model"));
                }
                key if key.ends_with(".context_length") && value > 0 && context.is_none() => {
                    context = Some(
                        value.clamp(u64::from(MIN_CONTEXT), u64::from(DEFAULT_CONTEXT)) as u32,
                    );
                }
                _ => {}
            }
        } else {
            reader.skip_value(kind, 0)?;
        }
    }
    if projector && (architecture != Some(true) || vision != Some(true)) {
        return Err(invalid(
            "Choose a llama.cpp GGUF projector with a vision encoder",
        ));
    }
    let total_tensors = if let Some((index, count)) = split {
        match split_keys {
            [Some(number), Some(parts), Some(total)]
                if number == index as u64
                    && parts == count as u64
                    && (1..=MAX_TENSORS).contains(&total) =>
            {
                total
            }
            _ => {
                return Err(invalid(
                    "The GGUF shard metadata does not match the sibling set",
                ));
            }
        }
    } else {
        tensors
    };
    let mut last_offset = 0;
    for _ in 0..tensors {
        let offset = reader.tensor_offset()?;
        if !offset.is_multiple_of(alignment) {
            return Err(invalid("GGUF tensor offset is unaligned"));
        }
        last_offset = last_offset.max(offset);
    }
    let tensor_data = (scan_bytes - reader.remaining).next_multiple_of(alignment);
    if bytes
        .checked_sub(tensor_data)
        .is_none_or(|size| tensors != 0 && last_offset >= size)
    {
        return Err(invalid("GGUF tensor offset exceeds the model file"));
    }
    Ok(Inspection {
        context: context.unwrap_or(DEFAULT_CONTEXT),
        tensors,
        total_tensors,
    })
}

struct Metadata<'a> {
    input: BufReader<&'a mut File>,
    remaining: u64,
    array_items: u64,
    cancelled: &'a AtomicBool,
}

impl Metadata<'_> {
    fn consume(&mut self, size: u64) -> io::Result<()> {
        check_cancelled(self.cancelled)?;
        self.remaining = self
            .remaining
            .checked_sub(size)
            .ok_or_else(|| invalid("GGUF metadata is truncated or exceeds its size limit"))?;
        Ok(())
    }

    fn read<const SIZE: usize>(&mut self) -> io::Result<[u8; SIZE]> {
        let mut bytes = [0; SIZE];
        self.consume(SIZE as u64)?;
        self.input.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    fn u32(&mut self) -> io::Result<u32> {
        self.read().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> io::Result<u64> {
        self.read().map(u64::from_le_bytes)
    }

    fn key(&mut self) -> io::Result<String> {
        let size = self.u64()?;
        if !(1..=MAX_KEY_BYTES).contains(&size) {
            return Err(invalid("Invalid GGUF metadata key length"));
        }
        self.consume(size)?;
        let mut bytes = vec![0; size as usize];
        self.input.read_exact(&mut bytes)?;
        String::from_utf8(bytes).map_err(|_| invalid("GGUF metadata key is not UTF-8"))
    }

    fn skip(&mut self, size: u64) -> io::Result<()> {
        self.consume(size)?;
        self.input.seek_relative(size as i64)
    }

    fn integer(&mut self, kind: u32) -> io::Result<u64> {
        Ok(match kind {
            0 => u64::from(u8::from_le_bytes(self.read()?)),
            1 => i8::from_le_bytes(self.read()?).max(0) as u64,
            2 => u64::from(u16::from_le_bytes(self.read()?)),
            3 => i16::from_le_bytes(self.read()?).max(0) as u64,
            4 => u64::from(self.u32()?),
            5 => i32::from_le_bytes(self.read()?).max(0) as u64,
            10 => self.u64()?,
            11 => i64::from_le_bytes(self.read()?).max(0) as u64,
            _ => return Err(invalid("Unsupported GGUF metadata integer")),
        })
    }

    fn tensor_offset(&mut self) -> io::Result<u64> {
        let name_size = self.u64()?;
        if !(1..=MAX_TENSOR_NAME_BYTES).contains(&name_size) {
            return Err(invalid("Invalid GGUF tensor name length"));
        }
        self.skip(name_size)?;
        let dimensions = self.u32()?;
        if dimensions > 4 {
            return Err(invalid("Invalid GGUF tensor dimension count"));
        }
        let mut elements = 1_i64;
        for _ in 0..dimensions {
            let dimension = i64::from_le_bytes(self.read()?);
            elements = elements
                .checked_mul(dimension)
                .filter(|count| *count > 0)
                .ok_or_else(|| invalid("Invalid GGUF tensor shape"))?;
        }
        let _ = self.u32()?; // The verified runtime interprets the tensor quantization.
        self.u64()
    }

    fn skip_value(&mut self, kind: u32, depth: u8) -> io::Result<()> {
        if let Some(size) = width(kind) {
            return self.skip(size);
        }
        match kind {
            STRING => {
                let size = self.u64()?;
                if size > MAX_STRING_BYTES {
                    return Err(invalid("GGUF metadata string exceeds its size limit"));
                }
                self.skip(size)
            }
            ARRAY if depth <= 2 => {
                let element = self.u32()?;
                let count = self.u64()?;
                self.array_items = self
                    .array_items
                    .checked_sub(count)
                    .ok_or_else(|| invalid("GGUF metadata arrays exceed their item limit"))?;
                if let Some(size) = width(element) {
                    return self.skip(count * size);
                }
                if !matches!(element, ARRAY | STRING) {
                    return Err(invalid("Unsupported GGUF metadata array element"));
                }
                for _ in 0..count {
                    self.skip_value(element, depth + 1)?;
                }
                Ok(())
            }
            _ => Err(invalid("Unsupported GGUF metadata value")),
        }
    }
}

fn integer(kind: u32) -> bool {
    matches!(kind, 0..=5 | 10 | 11)
}

fn width(kind: u32) -> Option<u64> {
    Some(match kind {
        0 | 1 | 7 => 1,
        2 | 3 => 2,
        4..=6 => 4,
        10..=12 => 8,
        _ => return None,
    })
}
