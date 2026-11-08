//! Offline, fingerprint-pinned OpenPGP recipient selection and encryption.

use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pgp::armor::{BlockType, Dearmor, DearmorOptions, Headers};
use pgp::composed::{ArmorOptions, Deserializable, MessageBuilder, SignedPublicKey};
use pgp::crypto::aead::{AeadAlgorithm, ChunkSize};
use pgp::crypto::ecc_curve::ECCCurve;
use pgp::crypto::hash::HashAlgorithm;
use pgp::crypto::public_key::PublicKeyAlgorithm;
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use pgp::packet::{
    Packet, PacketParser, PacketTrait, PublicKey, PublicSubkey, Signature, SignatureType,
    SubpacketData,
};
use pgp::types::{EncryptionKey, KeyDetails, KeyVersion, PublicParams, SignedUser, Tag};
use rand::{SeedableRng, rngs::StdRng};
use rsa::traits::PublicKeyParts as _;
use zeroize::Zeroizing;

use super::{absolute, invalid, profile};
use crate::snippets::recipients::{self, Choice};

pub(crate) const MAX_CERTIFICATE_BYTES: u64 = 1024 * 1024;
const MAX_PACKETS: usize = 256;
const MAX_FINGERPRINT_INPUT: usize = 128;
const MAX_DISPLAY_USER_ID_BYTES: usize = 256;

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct CertificateInfo {
    pub(crate) fingerprint: String,
    pub(crate) key_id: String,
    /// A self-certified display label, independent of user verification of the fingerprint.
    pub(crate) user_id: Option<String>,
    pub(crate) revoked: bool,
    source: PathBuf,
    certificate: Arc<str>,
}

impl CertificateInfo {
    pub(in crate::snippets) fn matches_source(&self, source: &Path, certificate: &str) -> bool {
        self.source == source && self.certificate.as_ref() == certificate
    }
}

impl fmt::Debug for CertificateInfo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertificateInfo")
            .field("fingerprint", &self.fingerprint)
            .field("key_id", &self.key_id)
            .field("user_id", &self.user_id)
            .field("revoked", &self.revoked)
            .finish_non_exhaustive()
    }
}

#[derive(Eq, PartialEq)]
pub(super) enum Recipient {
    File { path: PathBuf, fingerprint: String },
    Saved { path: PathBuf, choice: Choice },
}

impl Recipient {
    pub(super) fn new(path: PathBuf, fingerprint: &str) -> io::Result<Self> {
        absolute(&path)?;
        Ok(Self::File {
            path,
            fingerprint: normalize_fingerprint(fingerprint)?,
        })
    }

    pub(super) fn saved(path: PathBuf, choice: Choice) -> io::Result<Self> {
        absolute(&path)?;
        Ok(Self::Saved { path, choice })
    }

    pub(super) fn load(&self) -> io::Result<SignedPublicKey> {
        match self {
            Self::File { path, fingerprint } => load_file(path, fingerprint),
            Self::Saved { path, choice } => {
                let certificate = recipients::certificate(path, choice)?;
                pinned_certificate(certificate.as_bytes(), choice.fingerprint())
            }
        }
    }
}

pub(crate) fn normalize_fingerprint(fingerprint: &str) -> io::Result<String> {
    if fingerprint.len() > MAX_FINGERPRINT_INPUT {
        return Err(invalid(
            "Enter the complete OpenPGP certificate fingerprint",
        ));
    }
    let fingerprint: String = fingerprint
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    if !matches!(fingerprint.len(), 40 | 64)
        || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid(
            "Enter a 40- or 64-digit OpenPGP certificate fingerprint",
        ));
    }
    Ok(fingerprint.to_ascii_uppercase())
}

pub(crate) fn enroll_certificate(path: &Path, fingerprint: &str) -> io::Result<(String, bool)> {
    let cert = load_file(path, fingerprint)?;
    let revoked = primary_revoked(&cert);
    if !revoked {
        Encryption::select(&cert)?;
    }
    Ok((encode_certificate(&cert)?, revoked))
}

fn encode_certificate(cert: &SignedPublicKey) -> io::Result<String> {
    let certificate = cert
        .to_armored_string(Default::default())
        .map_err(|_| invalid("Could not encode the OpenPGP public certificate"))?;
    if certificate.len() as u64 > MAX_CERTIFICATE_BYTES {
        return Err(invalid("The armored OpenPGP certificate exceeds 1 MiB"));
    }
    Ok(certificate)
}

/// Reads a local certificate without pinning its identity or modifying the catalog.
pub(crate) fn inspect_certificate(path: &Path) -> io::Result<CertificateInfo> {
    let cert = parse_certificate(&read_certificate(path)?)?;
    if !matches!(cert.version(), KeyVersion::V4 | KeyVersion::V6) {
        return Err(invalid("Unsupported OpenPGP certificate primary key"));
    }
    let now = current_time()?;
    let revoked = primary_revoked(&cert);
    if !revoked {
        Encryption::select_at(&cert, now)?;
    }
    let fingerprint = format!("{:X}", cert.fingerprint());
    let key_id = recipients::key_id(&fingerprint).to_owned();
    let user_id = cert
        .details
        .users
        .iter()
        .filter_map(|user| {
            let label = user.id.as_str()?.trim();
            if label.is_empty()
                || label.len() > MAX_DISPLAY_USER_ID_BYTES
                || label.chars().any(hidden_display_character)
            {
                return None;
            }
            let signature = user_self_signature(&cert.primary_key, user, now)?;
            usable_signature(signature, &cert.primary_key, now).then_some((label, signature))
        })
        .max_by_key(|(_, signature)| (signature.is_primary(), signature.created()))
        .map(|(label, _)| label.to_owned());
    Ok(CertificateInfo {
        fingerprint,
        key_id,
        user_id,
        revoked,
        source: path.to_path_buf(),
        certificate: Arc::from(encode_certificate(&cert)?),
    })
}

fn primary_revoked(cert: &SignedPublicKey) -> bool {
    cert.details.revocation_signatures.iter().any(|signature| {
        signature.typ() == Some(SignatureType::KeyRevocation)
            && signature.verify_key(&cert.primary_key).is_ok()
    })
}

fn hidden_display_character(ch: char) -> bool {
    ch.is_control()
        || matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn load_file(path: &Path, fingerprint: &str) -> io::Result<SignedPublicKey> {
    pinned_certificate(&read_certificate(path)?, fingerprint)
}

fn read_certificate(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
    absolute(path)?;
    let file = profile::open_preference(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_CERTIFICATE_BYTES {
        return Err(invalid(
            "Choose a regular OpenPGP public certificate of at most 1 MiB",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CERTIFICATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CERTIFICATE_BYTES {
        return Err(invalid("The OpenPGP certificate exceeds 1 MiB"));
    }
    Ok(bytes)
}

fn pinned_certificate(bytes: &[u8], fingerprint: &str) -> io::Result<SignedPublicKey> {
    let cert = parse_certificate(bytes)?;
    if format!("{:X}", cert.fingerprint()) != fingerprint {
        return Err(invalid(
            "The OpenPGP certificate fingerprint does not match",
        ));
    }
    Ok(cert)
}

pub(crate) fn validate_backup_certificate(bytes: &[u8], fingerprint: &str) -> io::Result<()> {
    if bytes.len() as u64 > MAX_CERTIFICATE_BYTES {
        return Err(invalid("The OpenPGP certificate exceeds 1 MiB"));
    }
    pinned_certificate(bytes, fingerprint).map(|_| ())
}

fn parse_certificate(bytes: &[u8]) -> io::Result<SignedPublicKey> {
    if bytes.trim_ascii_start().starts_with(b"-----BEGIN") {
        let mut armor = Dearmor::with_options(
            bytes,
            DearmorOptions::new().set_limit(MAX_CERTIFICATE_BYTES as usize),
        );
        armor
            .read_header()
            .map_err(|_| invalid("Invalid OpenPGP armor"))?;
        if armor.typ != Some(BlockType::PublicKey) {
            return Err(invalid("Select an OpenPGP public certificate"));
        }
        let mut decoded = Zeroizing::new(Vec::new());
        armor
            .read_to_end(&mut decoded)
            .map_err(|_| invalid("Invalid OpenPGP armor"))?;
        let (_, _, _, mut remainder) = armor.into_parts();
        let mut trailing = Zeroizing::new(Vec::new());
        remainder.read_to_end(&mut trailing)?;
        if trailing.iter().any(|byte| !byte.is_ascii_whitespace()) {
            return Err(invalid("The OpenPGP file contains additional data"));
        }
        parse_packets(decoded.as_slice())
    } else {
        parse_packets(bytes)
    }
}

fn parse_packets(input: impl BufRead) -> io::Result<SignedPublicKey> {
    let mut packets = Vec::new();
    for packet in PacketParser::new(input) {
        let packet = packet.map_err(|_| invalid("Invalid OpenPGP certificate packet"))?;
        if packets.len() == MAX_PACKETS {
            return Err(invalid("The OpenPGP certificate has too many packets"));
        }
        if !matches!(
            packet.tag(),
            Tag::PublicKey
                | Tag::PublicSubkey
                | Tag::UserId
                | Tag::UserAttribute
                | Tag::Signature
                | Tag::Marker
                | Tag::Padding
        ) {
            return Err(invalid(
                "Select one OpenPGP public certificate without secret key packets",
            ));
        }
        packets.push(packet);
    }
    if packets
        .iter()
        .filter(|packet| matches!(packet, Packet::PublicKey(_)))
        .count()
        != 1
    {
        return Err(invalid("Select a single OpenPGP public certificate"));
    }
    SignedPublicKey::from_packets(packets.into_iter().map(Ok).peekable())
        .next()
        .and_then(Result::ok)
        .ok_or_else(|| invalid("Invalid OpenPGP public certificate"))
}

pub(super) struct Encryption<'a> {
    key: Component<'a>,
    cipher: SymmetricKeyAlgorithm,
    aead: Option<AeadAlgorithm>,
}

enum Component<'a> {
    Primary(&'a PublicKey),
    Subkey(&'a PublicSubkey),
}

impl<'a> Encryption<'a> {
    pub(super) fn select(cert: &'a SignedPublicKey) -> io::Result<Self> {
        Self::select_at(cert, current_time()?)
    }

    fn select_at(cert: &'a SignedPublicKey, now: u64) -> io::Result<Self> {
        if !matches!(cert.version(), KeyVersion::V4 | KeyVersion::V6)
            || !strong_primary(&cert.primary_key)
            || u64::from(cert.created_at().as_secs()) > now
        {
            return Err(invalid("Unsupported OpenPGP certificate primary key"));
        }
        if !cert.details.revocation_signatures.is_empty() {
            return Err(invalid("The OpenPGP certificate is revoked"));
        }

        let primary = &cert.primary_key;
        let direct = newest(
            cert.details
                .direct_signatures
                .iter()
                .filter(|signature| signature.typ() == Some(SignatureType::Key))
                .filter(|signature| signature.verify_key(primary).is_ok()),
            now,
        )?;
        let metadata = if cert.version() == KeyVersion::V6 {
            direct
        } else {
            let user = cert
                .details
                .users
                .iter()
                .filter_map(|user| user_self_signature(primary, user, now))
                .max_by_key(|signature| (signature.is_primary(), signature.created()));
            if direct.is_some_and(|signature| !usable_signature(signature, primary, now)) {
                return Err(invalid(
                    "The OpenPGP certificate is expired or its primary signature is unsupported",
                ));
            }
            user.or(direct)
        }
        .filter(|signature| usable_signature(signature, primary, now))
        .ok_or_else(|| invalid("The OpenPGP certificate lacks a valid current self-signature"))?;

        let subkey = cert
            .public_subkeys
            .iter()
            .filter(|subkey| subkey.key.version() == cert.version())
            .filter(|subkey| strong_encryption_key(&subkey.key))
            .filter(|subkey| {
                subkey
                    .signatures
                    .iter()
                    .all(|signature| signature.typ() != Some(SignatureType::SubkeyRevocation))
            })
            .filter(|subkey| {
                newest(
                    subkey.signatures.iter().filter(|signature| {
                        signature.typ() == Some(SignatureType::SubkeyBinding)
                            && signature
                                .verify_subkey_binding(primary, &subkey.key)
                                .is_ok()
                    }),
                    now,
                )
                .ok()
                .flatten()
                .is_some_and(|signature| {
                    usable_signature(signature, &subkey.key, now) && permits_encryption(signature)
                })
            })
            .max_by_key(|subkey| subkey.key.created_at());
        let key = match subkey {
            Some(subkey) => Component::Subkey(&subkey.key),
            None if strong_encryption_key(primary)
                && permits_encryption(direct.unwrap_or(metadata)) =>
            {
                Component::Primary(primary)
            }
            None => {
                return Err(invalid(
                    "The OpenPGP certificate has no valid encryption key",
                ));
            }
        };

        let features = metadata
            .features()
            .or_else(|| direct.and_then(Signature::features));
        let aead = features.is_some_and(|features| features.seipd_v2())
            || cert.version() == KeyVersion::V6 && features.is_none();
        if cert.version() == KeyVersion::V6 && !aead {
            return Err(invalid(
                "The version 6 certificate does not allow supported AEAD encryption",
            ));
        }
        let (cipher, aead) = if aead {
            let preferences = direct
                .filter(|signature| !signature.preferred_aead_algs().is_empty())
                .unwrap_or(metadata);
            let (cipher, algorithm) = preferences
                .preferred_aead_algs()
                .iter()
                .copied()
                .find(|(cipher, algorithm)| {
                    is_aes(*cipher)
                        && matches!(
                            algorithm,
                            AeadAlgorithm::Ocb | AeadAlgorithm::Eax | AeadAlgorithm::Gcm
                        )
                })
                .unwrap_or((SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Ocb));
            (cipher, Some(algorithm))
        } else {
            let preferences = direct
                .filter(|signature| !signature.preferred_symmetric_algs().is_empty())
                .unwrap_or(metadata);
            let cipher = preferences
                .preferred_symmetric_algs()
                .iter()
                .copied()
                .find(|cipher| is_aes(*cipher))
                .unwrap_or(SymmetricKeyAlgorithm::AES128);
            (cipher, None)
        };
        Ok(Self { key, cipher, aead })
    }

    pub(super) fn write(self, archive: &[u8], output: impl Write) -> io::Result<()> {
        self.write_named(archive, output, "kortty-snippets.zip", None)
    }

    fn write_named(
        self,
        payload: &[u8],
        output: impl Write,
        name: &str,
        armor: Option<ArmorOptions<'_>>,
    ) -> io::Result<()> {
        let mut seed = Zeroizing::new([0; 32]);
        getrandom::fill(seed.as_mut()).map_err(io::Error::other)?;
        let rng = StdRng::from_seed(*seed);
        let envelope = (name, armor);
        match self.key {
            Component::Primary(key) => {
                encrypt(key, payload, output, rng, self.cipher, self.aead, envelope)
            }
            Component::Subkey(key) => {
                encrypt(key, payload, output, rng, self.cipher, self.aead, envelope)
            }
        }
    }
}

fn current_time() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(io::Error::other)
}

fn user_self_signature<'a>(
    primary: &PublicKey,
    user: &'a SignedUser,
    now: u64,
) -> Option<&'a Signature> {
    let verified = || {
        user.signatures.iter().filter(|signature| {
            signature
                .verify_certification(primary, Tag::UserId, &user.id)
                .is_ok()
        })
    };
    if verified().any(|signature| signature.typ() == Some(SignatureType::CertRevocation)) {
        return None;
    }
    newest(verified(), now).ok().flatten()
}

pub(crate) fn encrypt_profile_zip(
    root: &Path,
    fingerprint: &str,
    archive: &[u8],
    output: impl Write,
    name: &'static str,
    comment: &str,
) -> io::Result<()> {
    let certificate =
        recipients::certificate_by_fingerprint(&root.join(recipients::FILE_NAME), fingerprint)?;
    let key = pinned_certificate(certificate.as_bytes(), &normalize_fingerprint(fingerprint)?)?;
    let headers = Headers::from([("Comment".to_owned(), vec![comment.to_owned()])]);
    let armor = ArmorOptions {
        headers: Some(&headers),
        include_checksum: false,
    };
    Encryption::select(&key)?.write_named(archive, output, name, Some(armor))
}

/// Encrypts the supplied document or archive for the unchanged, fingerprint-pinned selection.
pub(crate) fn encrypt_saved_payload(
    root: &Path,
    recipient: &Choice,
    payload: &[u8],
    output: impl Write,
    name: &str,
) -> io::Result<()> {
    let certificate =
        Recipient::saved(root.join(recipients::FILE_NAME), recipient.clone())?.load()?;
    Encryption::select(&certificate)?.write_named(payload, output, name, None)
}

fn newest<'a>(
    signatures: impl Iterator<Item = &'a Signature>,
    now: u64,
) -> io::Result<Option<&'a Signature>> {
    let mut newest: Option<(u64, &Signature)> = None;
    let mut conflict = false;
    for signature in signatures {
        let Some(created) = signature.created().map(|time| u64::from(time.as_secs())) else {
            continue;
        };
        if created > now {
            continue;
        }
        match newest {
            None => newest = Some((created, signature)),
            Some((previous, _)) if created > previous => {
                newest = Some((created, signature));
                conflict = false;
            }
            Some((previous, other)) if created == previous && signature != other => conflict = true,
            _ => {}
        }
    }
    if conflict {
        return Err(invalid(
            "The OpenPGP certificate has conflicting current signatures",
        ));
    }
    Ok(newest.map(|(_, signature)| signature))
}

fn usable_signature(signature: &Signature, key: &impl KeyDetails, now: u64) -> bool {
    let created = u64::from(key.created_at().as_secs());
    let Some(signed) = signature.created().map(|time| u64::from(time.as_secs())) else {
        return false;
    };
    if !(created..=now).contains(&signed)
        || !matches!(
            signature.hash_alg(),
            Some(
                HashAlgorithm::Sha256
                    | HashAlgorithm::Sha384
                    | HashAlgorithm::Sha512
                    | HashAlgorithm::Sha3_256
                    | HashAlgorithm::Sha3_512
            )
        )
        || !signature.config().is_some_and(|config| {
            config.hashed_subpackets().all(|part| {
                !part.is_critical
                    || matches!(
                        part.data,
                        SubpacketData::SignatureCreationTime(_)
                            | SubpacketData::SignatureExpirationTime(_)
                            | SubpacketData::KeyExpirationTime(_)
                            | SubpacketData::KeyFlags(_)
                            | SubpacketData::Features(_)
                            | SubpacketData::IsPrimary(_)
                            | SubpacketData::IssuerFingerprint(_)
                            | SubpacketData::IssuerKeyId(_)
                            | SubpacketData::PreferredSymmetricAlgorithms(_)
                            | SubpacketData::PreferredAeadAlgorithms(_)
                    )
            })
        })
    {
        return false;
    }
    let valid_until = |start: u64, duration: Option<pgp::types::Duration>| {
        duration.is_none_or(|duration| {
            duration.as_secs() == 0 || start + u64::from(duration.as_secs()) > now
        })
    };
    valid_until(created, signature.key_expiration_time())
        && valid_until(signed, signature.signature_expiration_time())
}

fn strong_primary(key: &PublicKey) -> bool {
    match key.public_params() {
        PublicParams::RSA(params) => params.key.n().bits() >= 3072,
        PublicParams::ECDSA(params) => strong_curve(params.curve()),
        PublicParams::Ed25519(_) | PublicParams::Ed448(_) => true,
        PublicParams::EdDSALegacy(params) => params.curve() == ECCCurve::Ed25519Legacy,
        _ => false,
    }
}

fn strong_encryption_key(key: &impl KeyDetails) -> bool {
    match (key.algorithm(), key.public_params()) {
        (PublicKeyAlgorithm::RSA | PublicKeyAlgorithm::RSAEncrypt, PublicParams::RSA(params)) => {
            params.key.n().bits() >= 3072
        }
        (PublicKeyAlgorithm::ECDH, PublicParams::ECDH(params)) => {
            params.curve() == ECCCurve::Curve25519Legacy || strong_curve(params.curve())
        }
        (PublicKeyAlgorithm::X25519, PublicParams::X25519(_))
        | (PublicKeyAlgorithm::X448, PublicParams::X448(_)) => true,
        _ => false,
    }
}

fn strong_curve(curve: ECCCurve) -> bool {
    matches!(
        curve,
        ECCCurve::P256
            | ECCCurve::P384
            | ECCCurve::P521
            | ECCCurve::BrainpoolP256r1
            | ECCCurve::BrainpoolP384r1
            | ECCCurve::BrainpoolP512r1
    )
}

fn permits_encryption(signature: &Signature) -> bool {
    let flags = signature.key_flags();
    flags.encrypt_storage() || flags.encrypt_comms()
}

fn is_aes(cipher: SymmetricKeyAlgorithm) -> bool {
    matches!(
        cipher,
        SymmetricKeyAlgorithm::AES128
            | SymmetricKeyAlgorithm::AES192
            | SymmetricKeyAlgorithm::AES256
    )
}

fn encrypt(
    key: &impl EncryptionKey,
    payload: &[u8],
    output: impl Write,
    mut rng: StdRng,
    cipher: SymmetricKeyAlgorithm,
    aead: Option<AeadAlgorithm>,
    (name, armor): (&str, Option<ArmorOptions<'_>>),
) -> io::Result<()> {
    let builder = MessageBuilder::from_reader(name.as_bytes().to_vec(), payload);
    let result = if let Some(aead) = aead {
        let mut builder = builder.seipd_v2(&mut rng, cipher, aead, ChunkSize::default());
        builder
            .encrypt_to_key(&mut rng, key)
            .map_err(|_| invalid("OpenPGP could not use the selected encryption key"))?;
        match armor {
            Some(options) => builder.to_armored_writer(&mut rng, options, output),
            None => builder.to_writer(&mut rng, output),
        }
    } else {
        let mut builder = builder.seipd_v1(&mut rng, cipher);
        builder
            .encrypt_to_key(&mut rng, key)
            .map_err(|_| invalid("OpenPGP could not use the selected encryption key"))?;
        match armor {
            Some(options) => builder.to_armored_writer(&mut rng, options, output),
            None => builder.to_writer(&mut rng, output),
        }
    };
    result.map_err(|_| io::Error::other("Could not write the encrypted OpenPGP payload"))
}
