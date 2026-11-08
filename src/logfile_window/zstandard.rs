use std::io::{self, BufRead, BufReader, Cursor, Read, Write};

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};
use ruzstd::encoding::{CompressionLevel, compress_to_vec};

use super::catalog::check_cancel;

const MAX_WINDOW: u64 = 8 * 1024 * 1024;
const MAX_SUPPORTED_WINDOW: u64 = 128 * 1024 * 1024;
const MAX_BLOCK_SIZE: u64 = 128 * 1024;
const MAX_FRAMES: usize = 4096;
const MAX_BLOCKS: usize = 65_536;
const MAX_TOTAL_BLOCKS: usize = 256 * 1024;
pub(crate) const ENCODE_CHUNK_BYTES: usize = 256 * 1024;

pub(crate) struct Decoder<R, C> {
    input: BufReader<R>,
    decoder: FrameDecoder,
    cancelled: C,
    active: bool,
    frames: usize,
    blocks: usize,
    decoded: u64,
    expected_size: Option<u64>,
    max_block_size: u32,
}

impl<R: Read, C: Fn() -> bool> Decoder<R, C> {
    pub(crate) fn new(input: R, cancelled: C) -> Self {
        Self::with_window_limit(input, cancelled, MAX_WINDOW)
    }

    pub(crate) fn with_window_limit(input: R, cancelled: C, max_window: u64) -> Self {
        let mut decoder = FrameDecoder::new();
        decoder.set_max_window_size(max_window.min(MAX_SUPPORTED_WINDOW));
        Self {
            input: BufReader::new(input),
            decoder,
            cancelled,
            active: false,
            frames: 0,
            blocks: 0,
            decoded: 0,
            expected_size: None,
            max_block_size: 0,
        }
    }

    fn start_frame(&mut self) -> io::Result<bool> {
        if self.frames == MAX_FRAMES {
            return Err(invalid("The logfile has too many zstd frames"));
        }
        // Preserve the descriptor byte because ruzstd exposes the content
        // size but reports zero for both a missing field and a declared zero.
        let mut prefix = [0; 6];
        self.input.read_exact(&mut prefix)?;
        self.frames += 1;
        match self
            .decoder
            .reset(Cursor::new(prefix).chain(&mut self.input))
        {
            Ok(()) => {
                let descriptor = prefix[4];
                if descriptor & 0x08 != 0 {
                    return Err(invalid("The zstd frame sets a reserved descriptor bit"));
                }
                // Bits 7–6 select the size field, and bit 5 is Single_Segment.
                self.expected_size = (descriptor & 0xe0 != 0).then(|| self.decoder.content_size());
                let window = if descriptor & 0x20 != 0 {
                    self.decoder.content_size()
                } else {
                    let base = 1_u64 << (10 + (prefix[5] >> 3));
                    base + (base / 8) * u64::from(prefix[5] & 7)
                };
                self.max_block_size = window.min(MAX_BLOCK_SIZE) as u32;
                self.active = true;
                self.decoded = 0;
                Ok(true)
            }
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                self.skip_frame(u64::from(length))?;
                Ok(false)
            }
            Err(error) => Err(codec(error)),
        }
    }

    fn skip_frame(&mut self, mut remaining: u64) -> io::Result<()> {
        while remaining != 0 {
            check_cancel(&self.cancelled)?;
            let bytes = self.input.fill_buf()?;
            if bytes.is_empty() {
                return Err(invalid("The skippable zstd frame is truncated"));
            }
            let skipped = remaining.min(bytes.len() as u64);
            self.input.consume(skipped as usize);
            remaining -= skipped;
        }
        Ok(())
    }

    fn decode_block(&mut self) -> io::Result<()> {
        // Replay only the inspected prefix; ruzstd still decodes the block and
        // checks the frame checksum using the original stream bytes.
        let mut prefix = [0; 8];
        self.input.read_exact(&mut prefix[..3])?;
        let header = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], 0]);
        let size = header >> 3;
        if size > self.max_block_size {
            return Err(invalid("The zstd logfile block exceeds its frame limit"));
        }
        let mut prefix_size = 3;
        if ((header >> 1) & 3) == 2 {
            prefix_size += self.check_literals(size, &mut prefix[3..])?;
        }
        self.decoder
            .decode_blocks(
                Cursor::new(&prefix[..prefix_size]).chain(&mut self.input),
                BlockDecodingStrategy::UptoBlocks(1),
            )
            .map_err(codec)?;
        Ok(())
    }

    fn check_literals(&mut self, block_size: u32, prefix: &mut [u8]) -> io::Result<usize> {
        if block_size == 0 {
            return Err(invalid("The compressed zstd block has no literals header"));
        }
        self.input.read_exact(&mut prefix[..1])?;
        let raw_or_rle = (prefix[0] & 3) < 2;
        let format = (prefix[0] >> 2) & 3;
        // RFC 8878 § 3.1.1.3.1.1 defines two families of literals headers.
        let (bytes, shift, bits) = match (raw_or_rle, format) {
            (true, 0 | 2) => (1, 3, 5),
            (true, 1) => (2, 4, 12),
            (true, _) => (3, 4, 20),
            (false, 0 | 1) => (3, 4, 10),
            (false, 2) => (4, 4, 14),
            (false, _) => (5, 4, 18),
        };
        if bytes > block_size as usize {
            return Err(invalid("The compressed zstd literals header is truncated"));
        }
        self.input.read_exact(&mut prefix[1..bytes])?;
        let header = u32::from_le_bytes([prefix[0], prefix[1], prefix[2], 0]);
        let regenerated = (header >> shift) & ((1 << bits) - 1);
        // In ruzstd 0.9.1 a compressed block with no sequences can bypass
        // the decoded block size check. Enforce its literals bound up front.
        if regenerated > self.max_block_size {
            return Err(invalid(
                "The zstd logfile literals exceed their block limit",
            ));
        }
        Ok(bytes)
    }
}

impl<R: Read, C: Fn() -> bool> Read for Decoder<R, C> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            check_cancel(&self.cancelled)?;
            if !self.active {
                if self.input.fill_buf()?.is_empty() {
                    return if self.frames == 0 {
                        Err(invalid("Missing zstd frame"))
                    } else {
                        Ok(0)
                    };
                }
                if !self.start_frame()? {
                    continue;
                }
            }
            if self.decoder.can_collect() != 0 {
                let read = self.decoder.read(output)?;
                self.decoded += read as u64;
                if self.expected_size.is_some_and(|size| self.decoded > size) {
                    return Err(invalid("The zstd logfile size does not match its frame"));
                }
                return Ok(read);
            }
            if self.decoder.is_finished() {
                if let Some(expected) = self.decoder.get_checksum_from_data()
                    && self.decoder.get_calculated_checksum() != Some(expected)
                {
                    return Err(invalid("The zstd logfile checksum does not match"));
                }
                if self.expected_size.is_some_and(|size| size != self.decoded) {
                    return Err(invalid("The zstd logfile size does not match its frame"));
                }
                self.active = false;
            } else {
                if self.decoder.blocks_decoded() == MAX_BLOCKS {
                    return Err(invalid("The zstd logfile frame has too many blocks"));
                }
                if self.blocks == MAX_TOTAL_BLOCKS {
                    return Err(invalid("The zstd logfile has too many blocks in total"));
                }
                self.decode_block()?;
                self.blocks += 1;
            }
        }
    }
}

pub(crate) struct Encoder<W> {
    target: W,
    chunk: Vec<u8>,
    emitted: bool,
}

impl<W: Write> Encoder<W> {
    pub(crate) fn new(target: W) -> Self {
        Self {
            target,
            chunk: Vec::with_capacity(ENCODE_CHUNK_BYTES),
            emitted: false,
        }
    }

    fn emit(&mut self) -> io::Result<()> {
        // The upstream compressor unwraps its I/O. Its reader and writer here are
        // bounded memory buffers, and fallible file I/O stays outside that API.
        let compressed = encode_chunk(&self.chunk);
        self.target.write_all(&compressed)?;
        self.chunk.clear();
        self.emitted = true;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> io::Result<()> {
        if !self.chunk.is_empty() || !self.emitted {
            self.emit()?;
        }
        self.target.flush()
    }
}

pub(crate) fn encode_chunk(chunk: &[u8]) -> Vec<u8> {
    compress_to_vec(chunk, CompressionLevel::Fastest)
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.chunk.len() == ENCODE_CHUNK_BYTES {
            self.emit()?;
        }
        let size = data.len().min(ENCODE_CHUNK_BYTES - self.chunk.len());
        self.chunk.extend_from_slice(&data[..size]);
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.chunk.is_empty() {
            self.emit()?;
        }
        self.target.flush()
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn codec(error: FrameDecoderError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    const MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

    fn skip(data: &[u8]) -> Vec<u8> {
        let mut frame = 0x184d2a5e_u32.to_le_bytes().to_vec();
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(data);
        frame
    }

    fn empty_blocks(count: usize) -> Vec<u8> {
        let mut frame = MAGIC.to_vec();
        frame.extend_from_slice(&[0, 0]);
        frame.resize(frame.len() + (count - 1) * 3, 0);
        frame.extend_from_slice(&[1, 0, 0]);
        frame
    }

    fn decode(input: &[u8]) -> io::Result<Vec<u8>> {
        let mut decoded = Vec::new();
        Decoder::new(input, || false).read_to_end(&mut decoded)?;
        Ok(decoded)
    }

    #[test]
    fn skippable_frames_are_ignored_and_truncation_and_checksum_errors_are_rejected() {
        let mut encoded = Vec::new();
        let mut encoder = Encoder::new(&mut encoded);
        let log = vec![b'x'; ENCODE_CHUNK_BYTES + 1];
        encoder.write_all(&log).unwrap();
        encoder.finish().unwrap();

        let mut input = skip(b"metadata before data");
        input.extend_from_slice(&encoded);
        input.extend(skip(b"metadata between frames"));
        input.extend_from_slice(&encoded);
        input.extend(skip(b"metadata after data"));
        assert_eq!(decode(&input).unwrap(), [log.as_slice(), &log].concat());
        input.pop();
        assert_eq!(
            decode(&input).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        *encoded.last_mut().unwrap() ^= 1;
        assert!(
            decode(&encoded)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    fn a_known_size_of_zero_is_distinct_from_an_unspecified_size() {
        let mut input = MAGIC.to_vec();
        // A multi-segment frame with a 1 KiB window and four-byte size of zero.
        input.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 1, 0, 0]);
        assert!(decode(&input).unwrap().is_empty());
        input[10] = 9; // A final raw block holding one byte.
        input.push(b'x');
        assert!(decode(&input).unwrap_err().to_string().contains("size"));
        input[4] = 0;
        input.drain(6..10);
        assert_eq!(decode(&input).unwrap(), b"x");
        input[4] = 0x08;
        assert!(decode(&input).unwrap_err().to_string().contains("reserved"));
    }

    #[test]
    fn counts_all_blocks_and_checks_cancellation_during_one_read() {
        let mut input = empty_blocks(MAX_BLOCKS).repeat(MAX_TOTAL_BLOCKS / MAX_BLOCKS);
        assert!(decode(&input).unwrap().is_empty());
        input.extend(empty_blocks(1));
        assert!(
            decode(&input)
                .unwrap_err()
                .to_string()
                .contains("blocks in total")
        );

        for input in [empty_blocks(20), skip(&[0; 24 * 1024])] {
            let polls = Cell::new(0);
            let mut decoder = Decoder::new(input.as_slice(), || {
                polls.set(polls.get() + 1);
                polls.get() == 4
            });
            let error = decoder.read(&mut [0; 1]).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            assert_eq!(polls.get(), 4);
        }
    }

    #[test]
    fn compressed_literals_must_fit_the_window_and_global_block_limit() {
        let mut input = [
            0x28, 0xb5, 0x2f, 0xfd, 0, 0, 0x2d, 0, 0, 0xfd, 0xff, 0xff, b'x', 0,
        ];
        // A 14-byte frame formerly regenerated 1,048,575 bytes in a 1 KiB window.
        assert!(decode(&input).unwrap_err().to_string().contains("literals"));

        input[9..12].copy_from_slice(&[0x0d, 0x40, 0]);
        assert_eq!(decode(&input).unwrap(), vec![b'x'; 1024]);
        input[9] = 0x1d;
        assert!(decode(&input).unwrap_err().to_string().contains("literals"));

        input[5] = 0x40; // 256 KiB window, so the global 128 KiB limit applies.
        input[9..12].copy_from_slice(&[0x0d, 0, 0x20]);
        assert_eq!(decode(&input).unwrap(), vec![b'x'; 128 * 1024]);
        input[9] = 0x1d;
        assert!(decode(&input).unwrap_err().to_string().contains("literals"));
    }
}
