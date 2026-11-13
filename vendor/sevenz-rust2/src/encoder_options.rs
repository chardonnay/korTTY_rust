// Modified by korTTY to use archive-scoped AES keys and fallible per-stream randomness.

use std::{fmt::Debug, num::NonZeroU64};

#[cfg(feature = "aes256")]
use std::{fmt, io, sync::Arc};
#[cfg(feature = "aes256")]
use zeroize::Zeroizing;

#[cfg(feature = "ppmd")]
use ppmd_rust::{PPMD7_MAX_MEM_SIZE, PPMD7_MAX_ORDER, PPMD7_MIN_MEM_SIZE, PPMD7_MIN_ORDER};

#[cfg(feature = "compress")]
use crate::EncoderConfiguration;
#[cfg(feature = "aes256")]
use crate::Password;

#[cfg(feature = "compress")]
#[derive(Debug, Clone)]
/// Options for LZMA compression.
pub struct LzmaOptions(pub(crate) lzma_rust2::LzmaOptions);

impl Default for LzmaOptions {
    fn default() -> Self {
        Self(lzma_rust2::LzmaOptions::with_preset(6))
    }
}

#[cfg(feature = "compress")]
impl LzmaOptions {
    /// Creates LZMA options with the specified compression level.
    ///
    /// # Arguments
    /// * `level` - Compression level (0-9, clamped to this range)
    pub fn from_level(level: u32) -> Self {
        Self(lzma_rust2::LzmaOptions::with_preset(level))
    }

    /// Sets the dictionary size used when encoding.
    ///
    /// Will be clamped between 4096..=4294967280.
    ///
    /// Encoding returns an invalid-input error for dictionary sizes above 1073741823 bytes
    /// on 64-bit targets or 268435454 bytes on 32-bit targets.
    pub fn set_dictionary_size(&mut self, dict_size: u32) {
        self.0.dict_size = dict_size.clamp(lzma_rust2::DICT_SIZE_MIN, lzma_rust2::DICT_SIZE_MAX);
    }

    /// Sets the nice length of a match.
    ///
    /// Will be clamped between 8..=273.
    pub fn set_nice_len(&mut self, nice_len: u32) {
        self.0.nice_len = nice_len.clamp(
            lzma_rust2::LzmaOptions::NICE_LEN_MIN,
            lzma_rust2::LzmaOptions::NICE_LEN_MAX,
        );
    }
}

#[cfg(feature = "compress")]
#[derive(Debug, Clone)]
/// Options for LZMA2 compression.
pub struct Lzma2Options {
    pub(crate) options: lzma_rust2::Lzma2Options,
    pub(crate) threads: u32,
}

impl Default for Lzma2Options {
    fn default() -> Self {
        Self {
            options: lzma_rust2::Lzma2Options::with_preset(6),
            threads: 1,
        }
    }
}

#[cfg(feature = "compress")]
impl Lzma2Options {
    /// Creates LZMA2 options with the specified compression level.
    /// Encoded using a single thread.
    ///
    /// # Arguments
    /// * `level` - Compression level (0-9, clamped to this range)
    pub fn from_level(level: u32) -> Self {
        Self {
            options: lzma_rust2::Lzma2Options::with_preset(level),
            threads: 1,
        }
    }

    /// Creates LZMA2 options with the specified compression level.
    /// Encoded using a multi-threading.
    ///
    /// # Arguments
    /// * `level` - Compression level (0-9, clamped to this range)
    /// * `threads` - Count of threads used to compress the data
    /// * `chunk_size` - Size of each independent chunk of uncompressed data.
    ///   The more streams can be created, the more effective is
    ///   the multi threading, but the worse the compression ratio
    ///   will be (value will be clamped to have at least the size of the dictionary).
    pub fn from_level_mt(level: u32, threads: u32, chunk_size: u64) -> Self {
        let mut options = lzma_rust2::Lzma2Options::with_preset(level);
        options.set_chunk_size(NonZeroU64::new(
            chunk_size.max(options.lzma_options.dict_size as u64),
        ));
        Self { options, threads }
    }

    /// Sets the dictionary size used when encoding.
    ///
    /// Will be clamped between 4096..=4294967280.
    ///
    /// Encoding returns an invalid-input error for dictionary sizes above 1073741823 bytes
    /// on 64-bit targets or 268435454 bytes on 32-bit targets.
    pub fn set_dictionary_size(&mut self, dict_size: u32) {
        self.options.lzma_options.dict_size =
            dict_size.clamp(lzma_rust2::DICT_SIZE_MIN, lzma_rust2::DICT_SIZE_MAX);
    }

    /// Sets the nice length of a match.
    ///
    /// Will be clamped between 8..=273.
    pub fn set_nice_len(&mut self, nice_len: u32) {
        self.options.lzma_options.nice_len = nice_len.clamp(
            lzma_rust2::LzmaOptions::NICE_LEN_MIN,
            lzma_rust2::LzmaOptions::NICE_LEN_MAX,
        );
    }
}

#[cfg(feature = "bzip2")]
#[derive(Debug, Copy, Clone)]
/// Options for BZIP2 compression.
pub struct Bzip2Options(pub(crate) u32);

#[cfg(feature = "bzip2")]
impl Bzip2Options {
    /// Creates BZIP2 options with the specified compression level.
    ///
    /// # Arguments
    /// * `level` - Compression level (typically 1-9)
    pub const fn from_level(level: u32) -> Self {
        Self(level)
    }
}

#[cfg(feature = "bzip2")]
impl Default for Bzip2Options {
    fn default() -> Self {
        Self(6)
    }
}

#[cfg(any(feature = "brotli", feature = "lz4"))]
const MINIMAL_SKIPPABLE_FRAME_SIZE: u32 = 64 * 1024;
#[cfg(feature = "brotli")]
const DEFAULT_SKIPPABLE_FRAME_SIZE: u32 = 128 * 1024;

#[cfg(feature = "brotli")]
#[derive(Debug, Copy, Clone)]
/// Options for Brotli compression.
pub struct BrotliOptions {
    pub(crate) quality: u32,
    pub(crate) window: u32,
    pub(crate) skippable_frame_size: u32,
}

#[cfg(feature = "brotli")]
impl BrotliOptions {
    /// Creates Brotli options with the specified quality and window size.
    ///
    /// # Arguments
    /// * `quality` - Compression quality (0-11, clamped to this range)
    /// * `window` - Window size (10-24, clamped to this range)
    pub const fn from_quality_window(quality: u32, window: u32) -> Self {
        let quality = if quality > 11 { 11 } else { quality };
        let window = if window > 24 { 24 } else { window };
        Self {
            quality,
            window,
            skippable_frame_size: DEFAULT_SKIPPABLE_FRAME_SIZE,
        }
    }

    /// Set's the skippable frame size. The size is defined as the size of uncompressed data a frame
    /// contains. A value of 0 deactivates skippable frames and uses the native brotli bitstream.
    /// If a value is set, then a similar skippable frame format used by LZ4 and ZSTD is used.
    ///
    /// Af value between 1..=64KiB will be set to 64KiB.
    ///
    /// This was first implemented by zstdmt. The default value is 128 KiB.
    pub fn with_skippable_frame_size(mut self, skippable_frame_size: u32) -> Self {
        if skippable_frame_size == 0 {
            self.skippable_frame_size = 0;
        } else {
            self.skippable_frame_size =
                u32::max(skippable_frame_size, MINIMAL_SKIPPABLE_FRAME_SIZE);
        }

        self
    }
}

#[cfg(feature = "brotli")]
impl Default for BrotliOptions {
    fn default() -> Self {
        Self {
            quality: 11,
            window: 22,
            skippable_frame_size: DEFAULT_SKIPPABLE_FRAME_SIZE,
        }
    }
}

#[cfg(feature = "compress")]
#[derive(Debug, Copy, Clone)]
/// Options for Delta filter compression.
pub struct DeltaOptions(pub(crate) u32);

#[cfg(feature = "compress")]
impl DeltaOptions {
    /// Creates Delta options with the specified distance.
    ///
    /// # Arguments
    /// * `distance` - Delta distance (1-256, clamped to this range, 0 becomes 1)
    pub const fn from_distance(distance: u32) -> Self {
        let distance = if distance == 0 {
            1
        } else if distance > 256 {
            256
        } else {
            distance
        };
        Self(distance)
    }
}

#[cfg(feature = "compress")]
impl Default for DeltaOptions {
    fn default() -> Self {
        Self(1)
    }
}

#[cfg(feature = "deflate")]
#[derive(Debug, Copy, Clone)]
/// Options for Deflate compression.
pub struct DeflateOptions(pub(crate) u32);

#[cfg(feature = "deflate")]
impl DeflateOptions {
    /// Creates Deflate options with the specified compression level.
    ///
    /// # Arguments
    /// * `level` - Compression level (0-9, clamped to this range)
    pub const fn from_level(level: u32) -> Self {
        let level = if level > 9 { 9 } else { level };
        Self(level)
    }
}

#[cfg(feature = "deflate")]
impl Default for DeflateOptions {
    fn default() -> Self {
        Self(6)
    }
}

#[cfg(feature = "lz4")]
#[derive(Debug, Copy, Clone, Default)]
/// Options for LZ4 compression.
pub struct Lz4Options {
    pub(crate) skippable_frame_size: u32,
}

#[cfg(feature = "lz4")]
impl Lz4Options {
    /// Set's the skippable frame size. The size is defined as the size of uncompressed data a frame
    /// contains. A value of 0 deactivates skippable frames and uses the native LZ4 bitstream.
    /// If a value is set, then the similar skippable frame format is used.
    ///
    /// Af value between 1..=64KiB will be set to 64KiB.
    ///
    /// This was first implemented by zstdmt.
    ///
    /// Defaults to not use the skippable frame format at all, since LZ4 is extremely fast and will
    /// most likely saturate IO even on a single thread.
    pub fn with_skippable_frame_size(mut self, skippable_frame_size: u32) -> Self {
        if skippable_frame_size == 0 {
            self.skippable_frame_size = 0;
        } else {
            self.skippable_frame_size =
                u32::max(skippable_frame_size, MINIMAL_SKIPPABLE_FRAME_SIZE);
        }

        self
    }
}

#[cfg(feature = "ppmd")]
#[derive(Debug, Copy, Clone)]
/// Options for PPMD compression.
pub struct PpmdOptions {
    pub(crate) order: u32,
    pub(crate) memory_size: u32,
}

#[cfg(feature = "ppmd")]
impl PpmdOptions {
    /// Creates PPMD options with the specified compression level.
    ///
    /// # Arguments
    /// * `level` - Compression level (0-9, clamped to this range)
    pub const fn from_level(level: u32) -> Self {
        const ORDERS: [u32; 10] = [3, 4, 4, 5, 5, 6, 8, 16, 24, 32];

        let level = if level > 9 { 9 } else { level };
        let order = ORDERS[level as usize];
        let memory_size = 1 << (level + 19);

        Self { order, memory_size }
    }

    /// Creates PPMD options with specific order and memory size parameters.
    ///
    /// # Arguments
    /// * `order` - Model order (clamped to valid PPMD range)
    /// * `memory_size` - Memory size in bytes (clamped to valid PPMD range)
    pub const fn from_order_memory_size(order: u32, memory_size: u32) -> Self {
        let order = if order > PPMD7_MAX_ORDER {
            PPMD7_MAX_ORDER
        } else if order < PPMD7_MIN_ORDER {
            PPMD7_MIN_ORDER
        } else {
            order
        };
        let memory_size = if memory_size > PPMD7_MAX_MEM_SIZE {
            PPMD7_MAX_MEM_SIZE
        } else if memory_size < PPMD7_MIN_MEM_SIZE {
            PPMD7_MIN_MEM_SIZE
        } else {
            memory_size
        };
        Self { order, memory_size }
    }
}

#[cfg(feature = "ppmd")]
impl Default for PpmdOptions {
    fn default() -> Self {
        Self::from_level(6)
    }
}

#[cfg(feature = "zstd")]
#[derive(Debug, Copy, Clone)]
/// Options for Zstandard compression.
pub struct ZstandardOptions(pub(crate) u32);

#[cfg(feature = "zstd")]
impl ZstandardOptions {
    /// Creates Zstandard options with the specified compression level.
    ///
    /// # Arguments
    /// * `level` - Compression level (typically 1-22)
    pub const fn from_level(level: u32) -> Self {
        let level = if level > 22 { 22 } else { level };
        Self(level)
    }
}

#[cfg(feature = "zstd")]
impl Default for ZstandardOptions {
    fn default() -> Self {
        Self(3)
    }
}

#[cfg(feature = "aes256")]
const KDF_CYCLES_POWER: u8 = 19;

/// Owns the derived AES-256 key and random salt for one archive operation.
#[cfg(feature = "aes256")]
pub struct ArchiveAes {
    key: Arc<Zeroizing<[u8; 32]>>,
    salt: [u8; 16],
}

#[cfg(feature = "aes256")]
impl ArchiveAes {
    /// Derives an archive key from the supplied password and a fresh random salt.
    pub fn new(password: &str) -> io::Result<Self> {
        Self::from_password(&Password::new(password))
    }

    pub(crate) fn from_password(password: &Password) -> io::Result<Self> {
        if password.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A 7z password is required",
            ));
        }
        let salt = random_bytes()?;
        let key = crate::encryption::derive_key(KDF_CYCLES_POWER, &salt, password.as_slice());
        Ok(Self {
            key: Arc::new(key),
            salt,
        })
    }

    /// Creates options with a fresh random IV for a single member stream or header.
    pub fn stream(&self) -> io::Result<AesEncoderOptions> {
        Ok(AesEncoderOptions {
            key: Arc::clone(&self.key),
            iv: random_bytes()?,
            salt: self.salt,
        })
    }
}

#[cfg(feature = "aes256")]
fn random_bytes() -> io::Result<[u8; 16]> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| io::Error::other("Could not obtain randomness for 7z encryption"))?;
    Ok(bytes)
}

/// AES-256 options for one stream, including a freshly generated IV.
#[cfg(feature = "aes256")]
#[derive(Clone)]
pub struct AesEncoderOptions {
    key: Arc<Zeroizing<[u8; 32]>>,
    pub(crate) iv: [u8; 16],
    salt: [u8; 16],
}

#[cfg(feature = "aes256")]
impl fmt::Debug for AesEncoderOptions {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AesEncoderOptions(<redacted>)")
    }
}

#[cfg(feature = "aes256")]
impl AesEncoderOptions {
    pub(crate) fn key(&self) -> &[u8; 32] {
        &self.key
    }

    #[cfg(test)]
    pub(crate) fn properties(&self) -> [u8; 34] {
        let mut props = [0u8; 34];
        self.write_properties(&mut props);
        props
    }

    #[inline]
    pub(crate) fn write_properties(&self, props: &mut [u8]) {
        assert!(props.len() >= 34);
        props[0] = KDF_CYCLES_POWER | 0xC0;
        props[1] = 0xFF;
        props[2..18].copy_from_slice(&self.salt);
        props[18..34].copy_from_slice(&self.iv);
    }
}

/// Encoder-specific options for various compression and encryption methods.
#[derive(Debug, Clone)]
pub enum EncoderOptions {
    #[cfg(feature = "compress")]
    /// Delta filter options.
    Delta(DeltaOptions),
    #[cfg(feature = "compress")]
    /// LZMA compression options.
    Lzma(LzmaOptions),
    #[cfg(feature = "compress")]
    /// LZMA2 compression options.
    Lzma2(Lzma2Options),
    #[cfg(feature = "brotli")]
    /// Brotli compression options.
    Brotli(BrotliOptions),
    #[cfg(feature = "bzip2")]
    /// BZIP2 compression options.
    Bzip2(Bzip2Options),
    #[cfg(feature = "deflate")]
    /// Deflate compression options.
    Deflate(DeflateOptions),
    #[cfg(feature = "lz4")]
    /// LZ4 compression options.
    Lz4(Lz4Options),
    #[cfg(feature = "ppmd")]
    /// PPMD compression options.
    Ppmd(PpmdOptions),
    #[cfg(feature = "zstd")]
    /// Zstandard compression options.
    Zstd(ZstandardOptions),
    #[cfg(feature = "aes256")]
    /// AES256 encryption options.
    Aes(AesEncoderOptions),
}

#[cfg(feature = "aes256")]
impl From<AesEncoderOptions> for EncoderOptions {
    fn from(value: AesEncoderOptions) -> Self {
        Self::Aes(value)
    }
}

#[cfg(all(feature = "aes256", feature = "compress"))]
impl From<AesEncoderOptions> for EncoderConfiguration {
    fn from(value: AesEncoderOptions) -> Self {
        Self::new(crate::EncoderMethod::AES256_SHA256).with_options(EncoderOptions::Aes(value))
    }
}

#[cfg(feature = "compress")]
impl From<DeltaOptions> for EncoderConfiguration {
    fn from(options: DeltaOptions) -> Self {
        Self::new(crate::EncoderMethod::DELTA_FILTER).with_options(EncoderOptions::Delta(options))
    }
}

#[cfg(feature = "compress")]
impl From<LzmaOptions> for EncoderConfiguration {
    fn from(options: LzmaOptions) -> Self {
        Self::new(crate::EncoderMethod::LZMA).with_options(EncoderOptions::Lzma(options))
    }
}

#[cfg(feature = "compress")]
impl From<Lzma2Options> for EncoderConfiguration {
    fn from(options: Lzma2Options) -> Self {
        Self::new(crate::EncoderMethod::LZMA2).with_options(EncoderOptions::Lzma2(options))
    }
}

#[cfg(feature = "bzip2")]
impl From<Bzip2Options> for EncoderConfiguration {
    fn from(options: Bzip2Options) -> Self {
        Self::new(crate::EncoderMethod::BZIP2).with_options(EncoderOptions::Bzip2(options))
    }
}

#[cfg(feature = "brotli")]
impl From<BrotliOptions> for EncoderConfiguration {
    fn from(options: BrotliOptions) -> Self {
        Self::new(crate::EncoderMethod::BROTLI).with_options(EncoderOptions::Brotli(options))
    }
}

#[cfg(feature = "deflate")]
impl From<DeflateOptions> for EncoderConfiguration {
    fn from(options: DeflateOptions) -> Self {
        Self::new(crate::EncoderMethod::DEFLATE).with_options(EncoderOptions::Deflate(options))
    }
}

#[cfg(feature = "lz4")]
impl From<Lz4Options> for EncoderConfiguration {
    fn from(options: Lz4Options) -> Self {
        Self::new(crate::EncoderMethod::LZ4).with_options(EncoderOptions::Lz4(options))
    }
}

#[cfg(feature = "ppmd")]
impl From<PpmdOptions> for EncoderConfiguration {
    fn from(options: PpmdOptions) -> Self {
        Self::new(crate::EncoderMethod::PPMD).with_options(EncoderOptions::Ppmd(options))
    }
}

#[cfg(feature = "zstd")]
impl From<ZstandardOptions> for EncoderConfiguration {
    fn from(options: ZstandardOptions) -> Self {
        Self::new(crate::EncoderMethod::ZSTD).with_options(EncoderOptions::Zstd(options))
    }
}

#[cfg(feature = "compress")]
impl From<DeltaOptions> for EncoderOptions {
    fn from(o: DeltaOptions) -> Self {
        Self::Delta(o)
    }
}

#[cfg(feature = "compress")]
impl From<Lzma2Options> for EncoderOptions {
    fn from(o: Lzma2Options) -> Self {
        Self::Lzma2(o)
    }
}

#[cfg(feature = "bzip2")]
impl From<Bzip2Options> for EncoderOptions {
    fn from(o: Bzip2Options) -> Self {
        Self::Bzip2(o)
    }
}

#[cfg(feature = "brotli")]
impl From<BrotliOptions> for EncoderOptions {
    fn from(o: BrotliOptions) -> Self {
        Self::Brotli(o)
    }
}

#[cfg(feature = "deflate")]
impl From<DeflateOptions> for EncoderOptions {
    fn from(o: DeflateOptions) -> Self {
        Self::Deflate(o)
    }
}

#[cfg(feature = "lz4")]
impl From<Lz4Options> for EncoderOptions {
    fn from(o: Lz4Options) -> Self {
        Self::Lz4(o)
    }
}

#[cfg(feature = "ppmd")]
impl From<PpmdOptions> for EncoderOptions {
    fn from(o: PpmdOptions) -> Self {
        Self::Ppmd(o)
    }
}

#[cfg(feature = "zstd")]
impl From<ZstandardOptions> for EncoderOptions {
    fn from(o: ZstandardOptions) -> Self {
        Self::Zstd(o)
    }
}

impl EncoderOptions {
    /// Gets the LZMA & LZMA2 dictionary size for this encoder option.
    ///
    /// Returns the dictionary size if this is an LZMA & LZMA2 option, or a default value otherwise.
    pub fn get_lzma_dict_size(&self) -> u32 {
        match self {
            #[cfg(feature = "compress")]
            EncoderOptions::Lzma(o) => o.0.dict_size,
            #[cfg(feature = "compress")]
            EncoderOptions::Lzma2(o) => o.options.lzma_options.dict_size,
            #[allow(unused)]
            _ => 0,
        }
    }
}
