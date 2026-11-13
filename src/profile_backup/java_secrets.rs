//! Secret envelopes written by Java KorTTY 2.26.0.

use std::io;

use aes_gcm::{Aes256Gcm, KeyInit, Nonce, Tag, aead::AeadInPlace};
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{
        DecodePaddingMode,
        general_purpose::{GeneralPurpose, GeneralPurposeConfig},
    },
};
use kortty_vault::{MAX_MASTER_PASSWORD_BYTES, Secret};
use roxmltree::{Document, Node, ParsingOptions};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub(in crate::profile_backup) mod keys;
pub(in crate::profile_backup) mod passwords;

const ITERATIONS: u32 = 310_000;
const SALT_BYTES: usize = 32;
const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;
const TAG_BYTES: usize = 16;
const ENVELOPE_OVERHEAD: usize = NONCE_BYTES + TAG_BYTES;
const MAX_MASTER_FILE_BYTES: usize = 4096;
const MAX_XML_NODES: u32 = 32_768;
#[cfg(test)]
const MAX_FIELD_BYTES: usize = 64 * 1024;
#[cfg(test)]
const MAX_TEMPORARY_KEY_BYTES: usize = 128 * 1024;
#[cfg(test)]
const TEMPORARY_KEY_PREFIX: &str = "enc:";
const JAVA_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

pub(super) struct JavaSecrets<'a> {
    password: &'a Secret,
    master_key: Option<Zeroizing<[u8; KEY_BYTES]>>,
}

impl<'a> JavaSecrets<'a> {
    pub(super) fn open(
        master_properties: Option<&[u8]>,
        source_password: &'a Secret,
    ) -> io::Result<Self> {
        if source_password.is_empty() || source_password.len() > MAX_MASTER_PASSWORD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The Java KorTTY master password has an invalid length.",
            ));
        }
        let master_key = master_properties
            .map(|properties| verify_master(properties, source_password))
            .transpose()?;
        Ok(Self {
            password: source_password,
            master_key,
        })
    }

    #[cfg(test)]
    fn decrypt_field(&self, encoded: &str) -> io::Result<Secret> {
        self.decrypt_field_with_limit(encoded, MAX_FIELD_BYTES)
    }

    fn decrypt_field_with_limit(&self, encoded: &str, max_plaintext: usize) -> io::Result<Secret> {
        let (salt, ciphertext) = encoded.split_once(':').ok_or_else(invalid_envelope)?;
        let salt = decode_fixed::<SALT_BYTES>(salt).ok_or_else(invalid_envelope)?;
        let envelope = decode_envelope(ciphertext, max_plaintext)?;
        let key = derive_key(self.password, &salt);
        decrypt(envelope, &key)
    }

    #[cfg(test)]
    fn decrypt_temporary_key(&self, encoded: &str) -> io::Result<Secret> {
        let ciphertext = encoded
            .strip_prefix(TEMPORARY_KEY_PREFIX)
            .ok_or_else(invalid_envelope)?;
        let key = self.master_key.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "The Java KorTTY backup has no master.key for its temporary SSH key.",
            )
        })?;
        decrypt(decode_envelope(ciphertext, MAX_TEMPORARY_KEY_BYTES)?, key)
    }
}

pub(super) fn parse_xml<'a>(
    bytes: &'a [u8],
    limit: usize,
    expected_root: &str,
) -> io::Result<Document<'a>> {
    if bytes.len() > limit {
        return Err(invalid_xml(
            "Java KorTTY source exceeds its XML size limit.",
        ));
    }
    let xml = std::str::from_utf8(bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes))
        .map_err(|_| invalid_xml("Java KorTTY source XML must use UTF-8."))?;
    let document = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_XML_NODES,
            entity_resolver: None,
        },
    )
    .map_err(|_| invalid_xml("Invalid Java KorTTY source XML."))?;
    let root = document.root_element();
    if root.tag_name().namespace().is_some() || !root.has_tag_name(expected_root) {
        return Err(invalid_xml("Unexpected Java KorTTY source XML root."));
    }
    Ok(document)
}

fn invalid_xml(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    field: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children().filter(move |child| {
        child.is_element() && child.tag_name().namespace().is_none() && child.has_tag_name(field)
    })
}

fn optional_child<'a, 'input>(
    node: Node<'a, 'input>,
    field: &'static str,
) -> Result<Option<Node<'a, 'input>>, ()> {
    let mut found = children(node, field);
    let first = found.next();
    if found.next().is_some() {
        return Err(());
    }
    Ok(first)
}

fn scalar<'a>(node: Node<'a, '_>, field: &'static str) -> Result<Option<&'a str>, ()> {
    Ok(raw_scalar(node, field)?.map(str::trim))
}

fn raw_scalar<'a>(node: Node<'a, '_>, field: &'static str) -> Result<Option<&'a str>, ()> {
    let Some(node) = optional_child(node, field)? else {
        return Ok(None);
    };
    if node.children().any(|child| child.is_element()) {
        return Err(());
    }
    let mut text = node
        .children()
        .filter(|child| child.is_text())
        .filter_map(|child| child.text());
    let value = text.next().unwrap_or_default();
    if text.next().is_some() {
        return Err(());
    }
    Ok(Some(value))
}

fn verify_master(properties: &[u8], password: &Secret) -> io::Result<Zeroizing<[u8; KEY_BYTES]>> {
    if properties.len() > MAX_MASTER_FILE_BYTES {
        return Err(invalid_master());
    }
    let properties = std::str::from_utf8(properties).map_err(|_| invalid_master())?;
    let mut salt = None;
    let mut hash = None;
    for line in properties.split(['\r', '\n']) {
        let line = line.trim_start_matches([' ', '\t', '\x0c']);
        if line.is_empty() || line.starts_with(['#', '!']) {
            continue;
        }
        let (name, value) = line.split_once('=').ok_or_else(invalid_master)?;
        let slot = match name {
            "salt" => &mut salt,
            "hash" => &mut hash,
            _ => continue,
        };
        if slot.replace(value).is_some() {
            return Err(invalid_master());
        }
    }

    let salt = decode_property::<SALT_BYTES>(salt)?;
    let hash = decode_property::<KEY_BYTES>(hash)?;
    let key = derive_key(password, &salt);
    if !bool::from(key.ct_eq(&*hash)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Java KorTTY master password does not match master.key.",
        ));
    }
    Ok(key)
}

fn decode_property<const N: usize>(value: Option<&str>) -> io::Result<Zeroizing<[u8; N]>> {
    // Properties.store escapes the trailing Base64 padding in these two properties.
    let value = value.ok_or_else(invalid_master)?;
    let value = Zeroizing::new(value.replace("\\=", "="));
    decode_fixed(value.as_str()).ok_or_else(invalid_master)
}

fn decode_fixed<const N: usize>(encoded: &str) -> Option<Zeroizing<[u8; N]>> {
    if encoded.len() > N.div_ceil(3) * 4 {
        return None;
    }
    let mut bytes = Zeroizing::new([0; N]);
    (JAVA_BASE64.decode_slice(encoded, &mut *bytes).ok()? == N).then_some(bytes)
}

fn derive_key(password: &Secret, salt: &[u8; SALT_BYTES]) -> Zeroizing<[u8; KEY_BYTES]> {
    let mut key = Zeroizing::new([0; KEY_BYTES]);
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, ITERATIONS, &mut *key);
    key
}

fn decode_envelope(encoded: &str, max_plaintext: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let max_envelope = max_plaintext + ENVELOPE_OVERHEAD;
    if encoded.len() > max_envelope.div_ceil(3) * 4 {
        return Err(invalid_envelope());
    }
    let decoded = Zeroizing::new(
        JAVA_BASE64
            .decode(encoded)
            .map_err(|_| invalid_envelope())?,
    );
    if !(ENVELOPE_OVERHEAD..=max_envelope).contains(&decoded.len()) {
        return Err(invalid_envelope());
    }
    Ok(decoded)
}

fn decrypt(mut envelope: Zeroizing<Vec<u8>>, key: &[u8; KEY_BYTES]) -> io::Result<Secret> {
    let (nonce, body) = envelope.split_at_mut(NONCE_BYTES);
    let (ciphertext, tag) = body.split_at_mut(body.len() - TAG_BYTES);
    Aes256Gcm::new(key.into())
        .decrypt_in_place_detached(
            Nonce::from_slice(nonce),
            b"",
            ciphertext,
            Tag::from_slice(tag),
        )
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "A Java KorTTY secret could not be authenticated.",
            )
        })?;
    let plaintext = std::str::from_utf8(ciphertext).map_err(|_| invalid_envelope())?;
    Ok(Secret::new(plaintext.to_owned()))
}

fn invalid_master() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Java KorTTY master.key.",
    )
}

fn invalid_envelope() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid Java KorTTY secret envelope.",
    )
}

#[cfg(test)]
mod tests;
