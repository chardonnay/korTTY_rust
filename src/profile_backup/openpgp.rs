//! Native profile ZIP encrypted to one verified OpenPGP recipient.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Cursor, Read, Seek, Write};
use std::path::Path;

use kortty_vault::{MAX_ARCHIVE_PASSWORD_BYTES, Secret};
use pgp::armor::{BlockType, Dearmor, DearmorOptions};
use pgp::composed::{
    DecryptionOptions, Esk, Message, PacketBodyReader, PublicOrSecret, SignedSecretKey, TheRing,
};
use pgp::crypto::{aead::AeadAlgorithm, sym::SymmetricKeyAlgorithm};
use pgp::packet::{
    DataMode, LiteralDataHeader, PacketHeader, PublicKeyEncryptedSessionKey,
    SymEncryptedProtectedDataConfig,
};
use pgp::types::{
    CompressionAlgorithm, PacketLength, Password, PkeskVersion, S2kParams, SecretParams,
    Seipdv1ReadMode, StringToKey, Tag,
};
use zeroize::Zeroizing;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

use super::archive::java;
use super::{Snapshot, archive};
use crate::private_files::FileStamp;
use crate::{profile, snippets::archive::encrypt_profile_zip};

#[cfg(test)]
pub(super) mod tests;

const COMMENT: &str = "kortty profile backup v1";
const MESSAGE_ARMOR_HEADER: &[u8] = b"-----BEGIN PGP MESSAGE-----";
const LITERAL: &str = "kortty-backup.zip";
const ZIP_MEMBER: &str = "profile.snapshot";
const ZIP_HEADER: usize = 30 + ZIP_MEMBER.len();
const ZIP_CENTRAL: usize = 46 + ZIP_MEMBER.len();
const ZIP_FOOTER: usize = 22;
const MAX_ZIP: usize = archive::MAX_PAYLOAD_BYTES + ZIP_HEADER + ZIP_CENTRAL + ZIP_FOOTER;
const ENCRYPTION_OVERHEAD_LIMIT: usize = 4 * 1024 * 1024;
const MAX_BINARY: usize = MAX_ZIP + ENCRYPTION_OVERHEAD_LIMIT;
const MAX_ARMORED: usize = armor_limit(MAX_BINARY);
const MAX_PROFILE_ARMORED: usize = armor_limit(java::MAX_ZIP_BYTES + ENCRYPTION_OVERHEAD_LIMIT);
const MAX_SECRET_KEY: usize = 1024 * 1024;
const MAX_SESSION_KEY_PACKET: u32 = 16 * 1024;

pub(super) fn write_to(
    output: &mut File,
    snapshot: &Snapshot,
    root: &Path,
    recipient: &str,
) -> io::Result<()> {
    let payload = archive::encode_snapshot(snapshot)?;
    let zip = write_zip(&payload)?;
    drop(payload);
    encrypt_profile_zip(root, recipient, &zip, output, LITERAL, COMMENT)
}

pub(super) enum ProfileBackup {
    Native(Snapshot),
    Java(java::Members),
}

struct Literal {
    bytes: Zeroizing<Vec<u8>>,
    compressed: bool,
}

pub(super) fn read_profile(
    source: &Path,
    private_key: &Path,
    password: &Secret,
) -> io::Result<ProfileBackup> {
    let source = profile::open_preference(source)?;
    let source_size = file_size(&source, MAX_PROFILE_ARMORED)?;
    let literal = read_document(
        source,
        source_size,
        private_key,
        password,
        java::MAX_ZIP_BYTES,
        true,
    )?;
    if !literal.compressed
        && let Ok(snapshot) = read_zip(&literal.bytes)
    {
        return Ok(ProfileBackup::Native(snapshot));
    }
    java::decode_openpgp_zip(&literal.bytes).map(ProfileBackup::Java)
}

/// Decrypts an immutable, already reviewed connection archive to a single binary literal.
/// The caller bounds the outer file separately and validates the returned document format.
pub(crate) fn decrypt_openpgp_document(
    source: &[u8],
    private_key: &Path,
    password: &Secret,
    max_payload: usize,
) -> io::Result<Zeroizing<Vec<u8>>> {
    if password.len() > MAX_ARCHIVE_PASSWORD_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The OpenPGP passphrase exceeds 1024 UTF-8 bytes.",
        ));
    }
    if !private_key.is_absolute() || !(1..=MAX_ZIP).contains(&max_payload) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OpenPGP import requires an absolute private key path and a valid size limit",
        ));
    }
    if !(1..=armor_limit(max_payload + ENCRYPTION_OVERHEAD_LIMIT)).contains(&source.len()) {
        return Err(invalid("OpenPGP message exceeds the size limit"));
    }
    read_document(
        source,
        source.len(),
        private_key,
        password,
        max_payload,
        false,
    )
    .map(|literal| literal.bytes)
}

const fn armor_limit(binary: usize) -> usize {
    binary.div_ceil(3) * 4 + binary / 16 + 16 * 1024
}

fn read_document(
    source: impl Read,
    source_size: usize,
    private_key: &Path,
    password: &Secret,
    max_payload: usize,
    allow_compressed: bool,
) -> io::Result<Literal> {
    let max_binary = max_payload + ENCRYPTION_OVERHEAD_LIMIT;
    let mut ciphertext = Vec::with_capacity(source_size.min(max_binary + 1));
    with_binary_limit(
        source,
        Armor::Message,
        false,
        armor_limit(max_binary),
        |reader| {
            reader
                .take((max_binary + 1) as u64)
                .read_to_end(&mut ciphertext)?;
            if ciphertext.len() > max_binary {
                return Err(invalid("OpenPGP message exceeds the size limit"));
            }
            Ok(())
        },
    )?;
    validate_packets_limit(&mut ciphertext.as_slice(), max_binary)?;

    let key = read_secret_key(private_key)?;
    decrypt_literal(&ciphertext, &key, password, max_payload, allow_compressed)
}

/// Recognizes only the native envelope, without requiring its decryption key.
/// The ASCII armor comment is a format hint and does not authenticate a producer.
pub(super) fn validate_ciphertext(file: &mut File) -> io::Result<()> {
    file.rewind()?;
    let result = file_size(file, MAX_ARMORED)
        .and_then(|_| with_binary(&mut *file, Armor::Message, true, validate_packets));
    let rewind = file.rewind();
    result.and(rewind)
}

/// Classifies the outer format only. The complete message is checked during import.
pub(super) fn recognizes_header(prefix: &[u8]) -> bool {
    prefix.starts_with(MESSAGE_ARMOR_HEADER)
        || PacketHeader::try_from_reader(prefix).is_ok_and(is_recipient_header)
}

#[derive(Clone, Copy)]
enum Armor {
    Message,
    PrivateKey,
}

fn with_binary<R: Read>(
    file: R,
    armor: Armor,
    require_comment: bool,
    read_binary: impl FnOnce(&mut dyn BufRead) -> io::Result<()>,
) -> io::Result<()> {
    let limit = match armor {
        Armor::Message => MAX_ARMORED,
        Armor::PrivateKey => MAX_SECRET_KEY,
    };
    with_binary_limit(file, armor, require_comment, limit, read_binary)
}

fn with_binary_limit<R: Read>(
    file: R,
    armor: Armor,
    require_comment: bool,
    limit: usize,
    read_binary: impl FnOnce(&mut dyn BufRead) -> io::Result<()>,
) -> io::Result<()> {
    let (prefix, block) = match armor {
        Armor::Message => (MESSAGE_ARMOR_HEADER, BlockType::Message),
        Armor::PrivateKey => (
            b"-----BEGIN PGP PRIVATE KEY BLOCK-----".as_slice(),
            BlockType::PrivateKey,
        ),
    };
    let mut input = BufReader::new(file.take((limit + 1) as u64));
    if input.fill_buf()?.starts_with(prefix) {
        let mut decoded = Dearmor::with_options(input, DearmorOptions::new().set_limit(16 * 1024));
        decoded
            .read_header()
            .map_err(|_| invalid("invalid OpenPGP armor header"))?;
        if decoded.typ != Some(block)
            || (require_comment
                && decoded
                    .headers
                    .get("Comment")
                    .map(Vec::as_slice)
                    .is_none_or(|values| values != [COMMENT]))
        {
            return Err(invalid(
                "OpenPGP file is not a native profile backup envelope",
            ));
        }
        let mut bytes = BufReader::new(&mut decoded);
        read_binary(&mut bytes)?;
        require_eof(&mut bytes)?;
        let (_, _, _, mut remainder) = decoded.into_parts();
        require_eof(&mut remainder)?;
        input = remainder.into_inner();
    } else if require_comment {
        return Err(invalid("OpenPGP file has no native profile backup marker"));
    } else {
        read_binary(&mut input)?;
        require_eof(&mut input)?;
    }
    if input.get_ref().limit() == 0 {
        return Err(invalid("OpenPGP input exceeds the size limit"));
    }
    Ok(())
}

fn validate_packets(input: &mut dyn BufRead) -> io::Result<()> {
    validate_packets_limit(input, MAX_BINARY)
}

fn validate_packets_limit(input: &mut dyn BufRead, max_binary: usize) -> io::Result<()> {
    let header = PacketHeader::try_from_reader(&mut *input)?;
    if !is_recipient_header(header) {
        return Err(invalid("expected one OpenPGP recipient packet"));
    }
    let mut recipient = PacketBodyReader::new(header, &mut *input)?;
    let key = PublicKeyEncryptedSessionKey::try_from_reader(header, &mut recipient)
        .map_err(|_| invalid("invalid OpenPGP recipient packet"))?;
    require_eof(&mut recipient)?;
    let seipd_version = match key.version() {
        PkeskVersion::V3 if key.id().is_ok_and(|id| !id.is_wildcard()) => 1,
        PkeskVersion::V6
            if key
                .fingerprint()
                .is_ok_and(|fingerprint| fingerprint.is_some()) =>
        {
            2
        }
        _ => return Err(invalid("unsupported OpenPGP recipient packet")),
    };

    let header = PacketHeader::try_from_reader(&mut *input)?;
    if header.tag() != Tag::SymEncryptedProtectedData
        || matches!(header.packet_length(), PacketLength::Indeterminate)
    {
        return Err(invalid(
            "expected OpenPGP integrity-protected encrypted data",
        ));
    }
    let mut encrypted = PacketBodyReader::new(header, &mut *input)?;
    let config = SymEncryptedProtectedDataConfig::try_from_reader(&mut encrypted)
        .map_err(|_| invalid("invalid OpenPGP encrypted packet header"))?;
    // v1 needs an encrypted prefix, MDC and literal. v2 needs the data chunk
    // authentication and final authentication tag after its configuration.
    let minimum = match config {
        SymEncryptedProtectedDataConfig::V1 if seipd_version == 1 => 41,
        SymEncryptedProtectedDataConfig::V2 { sym_alg, aead, .. }
            if seipd_version == 2
                && matches!(
                    sym_alg,
                    SymmetricKeyAlgorithm::AES128
                        | SymmetricKeyAlgorithm::AES192
                        | SymmetricKeyAlgorithm::AES256
                )
                && matches!(
                    aead,
                    AeadAlgorithm::Ocb | AeadAlgorithm::Eax | AeadAlgorithm::Gcm
                ) =>
        {
            33
        }
        _ => {
            return Err(invalid(
                "unsupported OpenPGP encrypted packet configuration",
            ));
        }
    };
    let remaining = io::copy(&mut encrypted.take(max_binary as u64), &mut io::sink())?;
    if !(minimum..max_binary as u64).contains(&remaining) {
        return Err(invalid("invalid OpenPGP encrypted packet size"));
    }
    require_eof(input)
}

fn is_recipient_header(header: PacketHeader) -> bool {
    header.tag() == Tag::PublicKeyEncryptedSessionKey
        && matches!(
            header.packet_length(),
            PacketLength::Fixed(1..=MAX_SESSION_KEY_PACKET)
        )
}

fn read_secret_key(path: &Path) -> io::Result<SignedSecretKey> {
    let mut file = profile::open_preference(path)?;
    file_size(&file, MAX_SECRET_KEY)?;
    let stamp = FileStamp::read(&file)?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(MAX_SECRET_KEY + 1));
    with_binary(&mut file, Armor::PrivateKey, false, |input| {
        input.read_to_end(&mut bytes)?;
        Ok(())
    })?;
    stamp.require_unchanged(&file)?;
    stamp.require_unchanged(&profile::open_preference(path)?)?;
    let mut keys = PublicOrSecret::from_bytes_many(bytes.as_slice())
        .map_err(|_| invalid("invalid OpenPGP private key"))?;
    match (keys.next(), keys.next()) {
        (Some(Ok(PublicOrSecret::Secret(key))), None) => Ok(key),
        _ => Err(invalid(
            "select a file containing exactly one OpenPGP private key",
        )),
    }
}

fn decrypt_literal(
    ciphertext: &[u8],
    key: &SignedSecretKey,
    password: &Secret,
    max_payload: usize,
    allow_compressed: bool,
) -> io::Result<Literal> {
    let message =
        Message::from_bytes(ciphertext).map_err(|_| invalid("invalid OpenPGP message"))?;
    let Message::Encrypted { esk, .. } = &message else {
        return Err(invalid("expected an encrypted OpenPGP message"));
    };
    let [Esk::PublicKeyEncryptedSessionKey(recipient)] = esk.as_slice() else {
        return Err(invalid("expected one OpenPGP recipient"));
    };
    let primary = recipient
        .match_identity(key.primary_key.public_key())
        .then_some(key.primary_key.secret_params());
    let subkeys = key.secret_subkeys.iter().filter_map(|subkey| {
        recipient
            .match_identity(subkey.key.public_key())
            .then_some(subkey.key.secret_params())
    });
    let mut matches = primary.into_iter().chain(subkeys);
    let selected = matches
        .next()
        .ok_or_else(|| invalid("the private key does not match the OpenPGP recipient"))?;
    if matches.next().is_some() {
        return Err(invalid(
            "the OpenPGP recipient matches multiple private key components",
        ));
    }
    validate_s2k(selected)?;

    let password = Password::from(password.as_str());
    let ring = TheRing {
        secret_keys: vec![key],
        key_passwords: vec![&password],
        // Verify the complete MDC before rPGP parses any v1 plaintext. The
        // encrypted data body is smaller than the already bounded ciphertext.
        decrypt_options: DecryptionOptions::new().set_seipdv1_read_mode(
            Seipdv1ReadMode::CheckFirst {
                max_message_size: ciphertext.len(),
            },
        ),
        ..TheRing::default()
    };
    let (message, _) = message
        .decrypt_the_ring(ring, true)
        .map_err(|_| invalid("could not unlock or decrypt the OpenPGP document"))?;
    match message {
        Message::Literal { mut reader, .. } => {
            check_binary_literal(reader.data_header())?;
            let bytes = read_literal_bytes(&mut reader, max_payload)?;
            // Reading the parent forces crypto EOF and rejects trailing packets.
            require_eof(reader.get_mut().get_mut())?;
            Ok(Literal {
                bytes,
                compressed: false,
            })
        }
        Message::Compressed { reader, .. } if allow_compressed => {
            let mut packet = reader.into_inner();
            let mut method = [0];
            packet.read_exact(&mut method)?;
            let bytes = match CompressionAlgorithm::from(method[0]) {
                CompressionAlgorithm::Uncompressed => {
                    read_literal_packet(&mut packet, max_payload)?
                }
                CompressionAlgorithm::ZIP => read_literal_packet(
                    flate2::bufread::DeflateDecoder::new(&mut packet),
                    max_payload,
                )?,
                CompressionAlgorithm::ZLIB => read_literal_packet(
                    flate2::bufread::ZlibDecoder::new(&mut packet),
                    max_payload,
                )?,
                CompressionAlgorithm::BZip2 => {
                    read_literal_packet(bzip2::bufread::BzDecoder::new(&mut packet), max_payload)?
                }
                _ => return Err(invalid("unsupported OpenPGP compression method")),
            };
            require_eof(&mut packet)?;
            require_eof(packet.get_mut())?;
            Ok(Literal {
                bytes,
                compressed: true,
            })
        }
        _ => Err(invalid("expected a single OpenPGP literal document")),
    }
}

fn read_literal_packet(reader: impl Read, max_payload: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut reader = BufReader::new(reader);
    let header = PacketHeader::try_from_reader(&mut reader)?;
    if header.tag() != Tag::LiteralData
        || matches!(header.packet_length(), PacketLength::Indeterminate)
    {
        return Err(invalid(
            "expected one binary literal packet inside OpenPGP compression",
        ));
    }
    let mut body = PacketBodyReader::new(header, &mut reader)?;
    check_binary_literal(&LiteralDataHeader::try_from_reader(&mut body)?)?;
    let bytes = read_literal_bytes(&mut body, max_payload)?;
    require_eof(&mut reader)?;
    Ok(bytes)
}

fn check_binary_literal(header: &LiteralDataHeader) -> io::Result<()> {
    // Literal filenames are optional and never used as local paths.
    if header.mode() != DataMode::Binary {
        return Err(invalid("unexpected OpenPGP literal data mode"));
    }
    Ok(())
}

fn read_literal_bytes(reader: impl Read, max_payload: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut payload = Zeroizing::new(Vec::new());
    reader
        .take((max_payload + 1) as u64)
        .read_to_end(&mut payload)?;
    if payload.len() > max_payload {
        return Err(invalid("OpenPGP literal document exceeds the size limit"));
    }
    Ok(payload)
}

fn validate_s2k(secret: &SecretParams) -> io::Result<()> {
    let SecretParams::Encrypted(secret) = secret else {
        return Ok(());
    };
    let s2k = match secret.string_to_key_params() {
        S2kParams::Aead { s2k, .. }
        | S2kParams::Cfb { s2k, .. }
        | S2kParams::MalleableCfb { s2k, .. } => s2k,
        _ => return Ok(()),
    };
    // Accept RFC 9106's 64 MiB parameter set as used by rPGP's v6 keys.
    if let StringToKey::Argon2 { t, p, m_enc, .. } = s2k
        && (*t > 4 || *p > 4 || *m_enc > 16)
    {
        return Err(invalid(
            "OpenPGP private key password derivation exceeds the resource limit",
        ));
    }
    Ok(())
}

fn write_zip(payload: &[u8]) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        payload.len() + ZIP_HEADER + ZIP_CENTRAL + ZIP_FOOTER,
    ));
    let mut zip = ZipWriter::new(Cursor::new(&mut *bytes));
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .unix_permissions(0o600);
    zip.start_file(ZIP_MEMBER, options)?;
    zip.write_all(payload)?;
    zip.finish()?;
    Ok(bytes)
}

fn read_zip(bytes: &[u8]) -> io::Result<Snapshot> {
    let size = bytes
        .len()
        .checked_sub(ZIP_HEADER + ZIP_CENTRAL + ZIP_FOOTER)
        .filter(|size| *size <= archive::MAX_PAYLOAD_BYTES)
        .ok_or_else(|| invalid("invalid OpenPGP profile ZIP size"))?;
    let central = ZIP_HEADER + size;
    let mut footer = [0; ZIP_FOOTER];
    footer[..4].copy_from_slice(b"PK\x05\x06");
    footer[8..12].copy_from_slice(&[1, 0, 1, 0]);
    footer[12..16].copy_from_slice(&(ZIP_CENTRAL as u32).to_le_bytes());
    footer[16..20].copy_from_slice(&(central as u32).to_le_bytes());
    if !bytes.ends_with(&footer) {
        return Err(invalid(
            "expected the native single-member profile ZIP directory",
        ));
    }
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    zip.validate_header_consistency()?;
    if zip.len() != 1 || zip.offset() != 0 {
        return Err(invalid("invalid OpenPGP profile ZIP directory"));
    }
    let mut member = zip.by_index(0)?;
    if member.name_raw() != ZIP_MEMBER.as_bytes()
        || !member.is_file()
        || member.encrypted()
        || member
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 != 0o100000)
        || member.compression() != CompressionMethod::Stored
        || member.size() != size as u64
        || member.compressed_size() != size as u64
        || member.data_start() != Some(ZIP_HEADER as u64)
        || member.header_start() != 0
        || member.central_header_start() != central as u64
    {
        return Err(invalid("invalid OpenPGP profile ZIP member"));
    }
    archive::read_payload(&mut member, size)
}

fn require_eof(input: &mut dyn BufRead) -> io::Result<()> {
    if !input.fill_buf()?.is_empty() {
        return Err(invalid("unexpected trailing OpenPGP data"));
    }
    Ok(())
}

fn file_size(file: &File, limit: usize) -> io::Result<usize> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || !(1..=limit as u64).contains(&metadata.len()) {
        return Err(invalid("invalid OpenPGP file size or type"));
    }
    Ok(metadata.len() as usize)
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
