use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

use crate::ClipboardError;

/// Maximum UTF-8 text size accepted by kortty's clipboard policy.
pub const MAX_CLIPBOARD_TEXT_BYTES: usize = 256 * 1024;
const MAX_ENCODED_BYTES: usize = MAX_CLIPBOARD_TEXT_BYTES.div_ceil(3) * 4;
pub(crate) const MAX_BODY_BYTES: usize = MAX_ENCODED_BYTES + 64 * 1024;
const DECODER: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub(crate) fn is_blank(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

pub(crate) fn decode(payload: &[u8]) -> Option<Result<String, ClipboardError>> {
    let separator = payload.iter().position(|&byte| byte == b';')?;
    let (selection, data) = payload.split_at(separator);
    if !selection
        .iter()
        .all(|byte| matches!(byte, b'c' | b'p' | b'q' | b's' | b'0'..=b'7'))
    {
        return None;
    }
    let data = &data[1..];
    if data == b"?" {
        return None;
    }
    let mut encoded = Vec::with_capacity(data.len().min(MAX_ENCODED_BYTES));
    for byte in data.iter().copied().filter(|&byte| !is_blank(byte)) {
        if encoded.len() == MAX_ENCODED_BYTES {
            return Some(Err(ClipboardError::TooLarge));
        }
        encoded.push(byte);
    }
    if encoded.is_empty() {
        return None;
    }
    Some(decode_text(&encoded))
}

fn decode_text(encoded: &[u8]) -> Result<String, ClipboardError> {
    let bytes = DECODER
        .decode(encoded)
        .map_err(|_| ClipboardError::Invalid)?;
    if bytes.len() > MAX_CLIPBOARD_TEXT_BYTES {
        return Err(ClipboardError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|_| ClipboardError::Io)
}
