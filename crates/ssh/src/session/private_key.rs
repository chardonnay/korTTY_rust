//! Bounded, local PEM decoding and RSA key validation after SSH host verification.

use std::io;

use pkcs8::der::{Decode as _, asn1::Null};
use pkcs8::pkcs5::{EncryptionScheme, pbes2};
use pkcs8::{EncryptedPrivateKeyInfo, PrivateKeyInfo};
use ssh_key::Mpint;
use ssh_key::private::RsaKeypair;
use sunset::SignKey;
use sunset::rsa::{BoxedUint, PrivateKey as RsaPrivateKey, traits::PrivateKeyParts as _};
use zeroize::Zeroizing;

use super::ConnectError;

const MAX_PBKDF2_ROUNDS: u32 = 1_000_000;

pub(super) fn invalid_pem() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid Ed25519, named-curve ECDSA, or RSA private key in PEM format",
    )
}

pub(super) fn with_decrypted_pkcs8(
    der: &[u8],
    passphrase: &[u8],
    decode: impl FnOnce(&[u8]) -> Result<SignKey, ConnectError>,
) -> Result<SignKey, ConnectError> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "invalid encrypted PKCS#8 key");
    let encrypted = EncryptedPrivateKeyInfo::from_der(der).map_err(|_| invalid())?;
    let EncryptionScheme::Pbes2(parameters) = encrypted.encryption_algorithm else {
        return Err(unsupported_encryption());
    };
    let pbes2::Kdf::Pbkdf2(kdf) = parameters.kdf else {
        return Err(unsupported_encryption());
    };
    let key_size = match parameters.encryption {
        pbes2::EncryptionScheme::Aes128Cbc { .. } => 16,
        pbes2::EncryptionScheme::Aes192Cbc { .. } => 24,
        pbes2::EncryptionScheme::Aes256Cbc { .. } => 32,
        _ => return Err(unsupported_encryption()),
    };
    if !(8..=64).contains(&kdf.salt.len())
        || !(1..=MAX_PBKDF2_ROUNDS).contains(&kdf.iteration_count)
        || kdf.key_length.is_some_and(|length| length != key_size)
        || encrypted.encrypted_data.is_empty()
        || !encrypted.encrypted_data.len().is_multiple_of(16)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PKCS#8 encryption parameters exceed supported bounds",
        )
        .into());
    }

    // The PBKDF2 SHA-1 PRF is used only for compatibility with encrypted local keys.
    // SSH host and user signatures always use the transport's SHA-2 algorithms.
    let mut buffer = Zeroizing::new(encrypted.encrypted_data.to_vec());
    let denied = || {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "incorrect PKCS#8 key passphrase or damaged key",
        )
    };
    let plaintext = parameters
        .decrypt_in_place(passphrase, &mut buffer)
        .map_err(|_| denied())?;
    PrivateKeyInfo::from_der(plaintext).map_err(|_| denied())?;
    decode(plaintext)
}

fn unsupported_encryption() -> ConnectError {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "encrypted PKCS#8 requires PBES2 with PBKDF2 and AES-CBC",
    )
    .into()
}

pub(super) fn rsa_openssh(pair: &RsaKeypair) -> Result<SignKey, ConnectError> {
    fn positive(part: &Mpint) -> Result<&[u8], sunset::Error> {
        part.as_positive_bytes().ok_or(sunset::Error::BadKey)
    }

    let private = rsa_components(
        positive(&pair.public.n)?,
        positive(&pair.public.e)?,
        positive(&pair.private.d)?,
        [positive(&pair.private.p)?, positive(&pair.private.q)?],
        positive(&pair.private.iqmp)?,
    )
    .ok_or(sunset::Error::BadKey)?;
    Ok(SignKey::rsa_private_key(private)?)
}

pub(super) fn rsa_pkcs8(info: PrivateKeyInfo<'_>) -> Result<SignKey, ConnectError> {
    if info
        .algorithm
        .parameters
        .is_some_and(|value| value != Null.into())
    {
        return Err(invalid_pem().into());
    }
    rsa_pkcs1(info.private_key, info.public_key)
}

pub(super) fn rsa_pkcs1(der: &[u8], public: Option<&[u8]>) -> Result<SignKey, ConnectError> {
    let key = pkcs1::RsaPrivateKey::from_der(der).map_err(|_| invalid_pem())?;
    if key.version() != pkcs1::Version::TwoPrime {
        return Err(invalid_pem().into());
    }
    if let Some(public) = public {
        let public = pkcs1::RsaPublicKey::from_der(public).map_err(|_| invalid_pem())?;
        if public.modulus != key.modulus || public.public_exponent != key.public_exponent {
            return Err(invalid_pem().into());
        }
    }
    let signer = rsa_components(
        key.modulus.as_bytes(),
        key.public_exponent.as_bytes(),
        key.private_exponent.as_bytes(),
        [key.prime1.as_bytes(), key.prime2.as_bytes()],
        key.coefficient.as_bytes(),
    )
    .ok_or_else(invalid_pem)?;
    if !matches_component(signer.dp(), key.exponent1.as_bytes())
        || !matches_component(signer.dq(), key.exponent2.as_bytes())
    {
        return Err(invalid_pem().into());
    }
    Ok(SignKey::rsa_private_key(signer)?)
}

fn rsa_components(
    modulus: &[u8],
    exponent: &[u8],
    private_exponent: &[u8],
    [prime1, prime2]: [&[u8]; 2],
    coefficient: &[u8],
) -> Option<RsaPrivateKey> {
    let first = *modulus.first()?;
    if !(256..=1024).contains(&modulus.len()) || first == 0 || exponent.len() > 5 {
        return None;
    }
    let modulus_bits = u32::try_from(modulus.len()).ok()? * 8 - first.leading_zeros();
    if modulus_bits < 2048 {
        return None;
    }
    let prime_bits = modulus_bits.div_ceil(2);
    let signer = RsaPrivateKey::from_components(
        BoxedUint::from_be_slice(modulus, modulus_bits).ok()?,
        BoxedUint::from_be_slice(exponent, 64).ok()?,
        BoxedUint::from_be_slice(private_exponent, modulus_bits).ok()?,
        vec![
            BoxedUint::from_be_slice(prime1, prime_bits).ok()?,
            BoxedUint::from_be_slice(prime2, prime_bits).ok()?,
        ],
    )
    .ok()?;
    let actual = Zeroizing::new(signer.crt_coefficient()?);
    matches_component(Some(&*actual), coefficient).then_some(signer)
}

fn matches_component(actual: Option<&BoxedUint>, bytes: &[u8]) -> bool {
    actual.is_some_and(|actual| {
        BoxedUint::from_be_slice(bytes, actual.bits_precision())
            .map(Zeroizing::new)
            .is_ok_and(|expected| *expected == *actual)
    })
}
