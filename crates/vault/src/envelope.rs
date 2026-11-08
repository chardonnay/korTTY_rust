use std::collections::BTreeMap;
use std::str;

use argon2::{Algorithm, Argon2, Block, Params, Version};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use zeroize::Zeroizing;

use super::{
    Credential, CredentialKind, Entry, Error, ID_BYTES, MAX_DESCRIPTION_BYTES, MAX_ENTRIES,
    MAX_ENVIRONMENT_BYTES, MAX_LABEL_BYTES, MAX_MASTER_PASSWORD_BYTES, MAX_PATTERN_BYTES,
    MAX_SECRET_BYTES, MAX_USERNAME_BYTES, random, validate_entry,
};

const MAGIC: &[u8; 8] = b"korttvlt";
const VERSION: u8 = 1;
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 4;
const KEY_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;
const TAG_BYTES: usize = 16;
const COUNT_BYTES: usize = size_of::<u32>();
const SALT_START: usize = MAGIC.len() + 1 + 3 * size_of::<u32>();
const NONCE_START: usize = SALT_START + SALT_BYTES;
const HEADER_BYTES: usize = NONCE_START + NONCE_BYTES;
pub(super) const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

pub(super) type Key = Zeroizing<[u8; KEY_BYTES]>;
pub(super) type Salt = [u8; SALT_BYTES];

pub(super) fn derive_key(password: &[u8], salt: &Salt) -> Result<Key, Error> {
    if password.is_empty() || password.len() > MAX_MASTER_PASSWORD_BYTES {
        return Err(Error::Authentication);
    }
    let parameters =
        Params::new(MEMORY_KIB, PASSES, LANES, Some(KEY_BYTES)).map_err(|_| Error::Crypto)?;
    let block_count = parameters.block_count();
    let mut blocks = Zeroizing::new(Vec::new());
    blocks
        .try_reserve_exact(block_count)
        .map_err(|_| Error::Limit)?;
    blocks.resize_with(block_count, Block::new);
    let mut key = Zeroizing::new([0; KEY_BYTES]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, parameters)
        .hash_password_into_with_memory(password, salt, key.as_mut(), blocks.as_mut_slice())
        .map_err(|_| Error::Crypto)?;
    Ok(key)
}

pub(super) fn authenticate(
    snapshot: &[u8],
    master: &[u8],
) -> Result<(Salt, Key, BTreeMap<String, Entry>), Error> {
    let salt = salt(snapshot)?;
    let key = derive_key(master, &salt)?;
    let entries = decode(&open(snapshot, &key)?)?;
    Ok((salt, key, entries))
}

pub(super) fn salt(snapshot: &[u8]) -> Result<Salt, Error> {
    if snapshot.len() > MAX_FILE_BYTES {
        return Err(Error::Limit);
    }
    if snapshot.len() < HEADER_BYTES + COUNT_BYTES + TAG_BYTES || !snapshot.starts_with(MAGIC) {
        return Err(Error::InvalidFormat);
    }
    if snapshot[MAGIC.len()] != VERSION {
        return Err(Error::UnsupportedVersion);
    }
    let mut parameters = &snapshot[MAGIC.len() + 1..SALT_START];
    if read_u32(&mut parameters)? != MEMORY_KIB
        || read_u32(&mut parameters)? != PASSES
        || read_u32(&mut parameters)? != LANES
    {
        return Err(Error::InvalidFormat);
    }
    snapshot[SALT_START..NONCE_START]
        .try_into()
        .map_err(|_| Error::InvalidFormat)
}

pub(super) fn seal<'a>(
    key: &Key,
    salt: &Salt,
    entries: impl Iterator<Item = &'a Entry> + Clone,
) -> Result<Vec<u8>, Error> {
    let mut length = HEADER_BYTES + COUNT_BYTES + TAG_BYTES;
    let mut count = 0u32;
    for entry in entries.clone() {
        count += 1;
        length += ID_BYTES
            + 1
            + fields(entry)
                .iter()
                .map(|value| COUNT_BYTES + value.len())
                .sum::<usize>();
        if length > MAX_FILE_BYTES || count as usize > MAX_ENTRIES {
            return Err(Error::Limit);
        }
    }
    let nonce = random::<NONCE_BYTES>()?;
    // Reserve the full size so appending a field cannot leave plaintext in a freed allocation.
    let mut bytes = Zeroizing::new(Vec::new());
    bytes.try_reserve_exact(length).map_err(|_| Error::Limit)?;
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    for number in [MEMORY_KIB, PASSES, LANES] {
        bytes.extend_from_slice(&number.to_be_bytes());
    }
    bytes.extend_from_slice(salt);
    bytes.extend_from_slice(&nonce);
    bytes.extend_from_slice(&count.to_be_bytes());
    for entry in entries {
        bytes.extend_from_slice(entry.metadata.id.as_bytes());
        bytes.push(match entry.metadata.kind {
            CredentialKind::Password => 0,
            CredentialKind::KeyPassphrase => 1,
            CredentialKind::ArchivePassword => 2,
        });
        for field in fields(entry) {
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| Error::Crypto)?;
    let (header, payload) = bytes.split_at_mut(HEADER_BYTES);
    let tag = cipher
        .encrypt_in_place_detached(&XNonce::from(nonce), header, payload)
        .map_err(|_| Error::Crypto)?;
    bytes.extend_from_slice(&tag);
    Ok(std::mem::take(&mut *bytes))
}

pub(super) fn open(snapshot: &[u8], key: &Key) -> Result<Zeroizing<Vec<u8>>, Error> {
    let (header, payload) = snapshot
        .split_at_checked(HEADER_BYTES)
        .ok_or(Error::InvalidFormat)?;
    let end = payload
        .len()
        .checked_sub(TAG_BYTES)
        .ok_or(Error::InvalidFormat)?;
    let (ciphertext, tag) = payload.split_at(end);
    let nonce = XNonce::from_slice(&header[NONCE_START..]);
    let tag = Tag::from_slice(tag);
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| Error::Crypto)?;
    let mut plaintext = Zeroizing::new(ciphertext.to_vec());
    cipher
        .decrypt_in_place_detached(nonce, header, plaintext.as_mut_slice(), tag)
        .map_err(|_| Error::Authentication)?;
    Ok(plaintext)
}

fn decode(mut plaintext: &[u8]) -> Result<BTreeMap<String, Entry>, Error> {
    let count = read_u32(&mut plaintext)? as usize;
    if count > MAX_ENTRIES {
        return Err(Error::InvalidFormat);
    }
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let id =
            str::from_utf8(take(&mut plaintext, ID_BYTES)?).map_err(|_| Error::InvalidFormat)?;
        let kind = match take(&mut plaintext, 1)?[0] {
            0 => CredentialKind::Password,
            1 => CredentialKind::KeyPassphrase,
            2 => CredentialKind::ArchivePassword,
            _ => return Err(Error::InvalidFormat),
        };
        let entry = Entry {
            metadata: Credential {
                id: id.to_owned(),
                label: text(&mut plaintext, MAX_LABEL_BYTES)?.to_owned(),
                username: text(&mut plaintext, MAX_USERNAME_BYTES)?.to_owned(),
                server_pattern: text(&mut plaintext, MAX_PATTERN_BYTES)?.to_owned(),
                environment: text(&mut plaintext, MAX_ENVIRONMENT_BYTES)?.to_owned(),
                description: text(&mut plaintext, MAX_DESCRIPTION_BYTES)?.to_owned(),
                kind,
            },
            secret: Zeroizing::new(text(&mut plaintext, MAX_SECRET_BYTES)?.to_owned()),
        };
        validate_entry(&entry).map_err(|_| Error::InvalidFormat)?;
        if entries.insert(id.to_owned(), entry).is_some() {
            return Err(Error::InvalidFormat);
        }
    }
    if !plaintext.is_empty() {
        return Err(Error::InvalidFormat);
    }
    Ok(entries)
}

fn fields(entry: &Entry) -> [&str; 6] {
    let metadata = &entry.metadata;
    [
        &metadata.label,
        &metadata.username,
        &metadata.server_pattern,
        &metadata.environment,
        &metadata.description,
        &entry.secret,
    ]
}

fn take<'a>(input: &mut &'a [u8], length: usize) -> Result<&'a [u8], Error> {
    let (prefix, rest) = input.split_at_checked(length).ok_or(Error::InvalidFormat)?;
    *input = rest;
    Ok(prefix)
}

fn read_u32(input: &mut &[u8]) -> Result<u32, Error> {
    let bytes = take(input, COUNT_BYTES)?
        .try_into()
        .map_err(|_| Error::InvalidFormat)?;
    Ok(u32::from_be_bytes(bytes))
}

fn text<'a>(input: &mut &'a [u8], limit: usize) -> Result<&'a str, Error> {
    let length = read_u32(input)? as usize;
    if length > limit {
        return Err(Error::InvalidFormat);
    }
    str::from_utf8(take(input, length)?).map_err(|_| Error::InvalidFormat)
}
