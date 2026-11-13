//! RFC 8332 signatures using the portable RustCrypto providers.

use rsa::sha2::{Digest, Sha256, Sha512};
use rsa::traits::{PrivateKeyParts, PublicKeyParts};
use rsa::{BoxedUint, Pkcs1v15Sign, PrivateKey, PublicKey};

use crate::error::{Error, Result};
use crate::sign::{OwnedSig, SigType};
use crate::sshwire::{self, SSHEncode};

#[cfg(test)]
pub(crate) mod tests;

pub(crate) fn validate_private(key: &PrivateKey) -> Result<()> {
    if !(crate::config::RSA_MIN_KEYSIZE..=crate::config::RSA_MAX_KEYSIZE)
        .contains(&(key.n().bit_width_vartime() as usize))
        || key.primes().len() != 2
        || key.primes()[0] == key.primes()[1]
    {
        return Err(Error::BadKey);
    }
    key.validate().map_err(|_| Error::BadKey)
}

pub(crate) fn public_components(n: &[u8], e: &[u8]) -> Result<PublicKey> {
    for part in [n, e] {
        if part.is_empty() || part.len() > crate::config::RSA_MAX_KEYSIZE / 8 {
            return Err(Error::BadKey);
        }
    }
    let integer = |part: &[u8]| {
        BoxedUint::from_be_slice(part, (part.len() * 8) as u32).map_err(|_| Error::BadKey)
    };
    PublicKey::new_with_size_limits(
        integer(n)?,
        integer(e)?,
        crate::config::RSA_MIN_KEYSIZE..=crate::config::RSA_MAX_KEYSIZE,
    )
    .map_err(|_| Error::BadKey)
}

pub(crate) fn private_components(
    n: &[u8],
    e: &[u8],
    d: &[u8],
    p: &[u8],
    q: &[u8],
) -> Result<PrivateKey> {
    let public = public_components(n, e)?;
    let precision = public.n_bits_precision();
    if [d, p, q]
        .iter()
        .any(|part| part.is_empty() || part.len() > public.size())
    {
        return Err(Error::BadKey);
    }
    // Every secret starts at the same public-derived precision. The provider
    // applies its fixed two-prime width policy while validating the components.
    let integer =
        |part: &[u8]| BoxedUint::from_be_slice(part, precision).map_err(|_| Error::BadKey);
    let key = PrivateKey::from_components(
        public.n().as_ref().clone(),
        public.e().clone(),
        integer(d)?,
        alloc::vec![integer(p)?, integer(q)?],
    )
    .map_err(|_| Error::BadKey)?;
    validate_private(&key)?;
    Ok(key)
}

fn digest(
    message: &dyn SSHEncode,
    algorithm: SigType,
) -> Result<(Pkcs1v15Sign, alloc::vec::Vec<u8>)> {
    match algorithm {
        SigType::RSA256 => {
            let mut hash = Sha256::new();
            sshwire::hash_ser(&mut hash, message)?;
            Ok((Pkcs1v15Sign::new::<Sha256>(), hash.finalize().to_vec()))
        }
        SigType::RSA512 => {
            let mut hash = Sha512::new();
            sshwire::hash_ser(&mut hash, message)?;
            Ok((Pkcs1v15Sign::new::<Sha512>(), hash.finalize().to_vec()))
        }
        _ => Err(Error::BadSig),
    }
}

pub(crate) fn sign(
    key: &PrivateKey,
    message: &dyn SSHEncode,
    algorithm: SigType,
) -> Result<OwnedSig> {
    let (padding, hash) = digest(message, algorithm)?;
    // The provider blinds the private-key operation with OS randomness and checks
    // its result. No remote transcript can request an unblinded signing path.
    let signature = key
        .sign_with_rng(&mut getrandom::SysRng, padding, &hash)
        .map_err(|_| Error::BadSig)?
        .into_boxed_slice();
    match algorithm {
        SigType::RSA256 => Ok(OwnedSig::RSA256(signature)),
        SigType::RSA512 => Ok(OwnedSig::RSA512(signature)),
        _ => Err(Error::BadSig),
    }
}

pub(crate) fn verify(
    key: &PublicKey,
    message: &dyn SSHEncode,
    signature: &[u8],
    algorithm: SigType,
) -> Result<()> {
    let (padding, hash) = digest(message, algorithm)?;
    if signature.is_empty() || signature.len() > key.size() {
        return Err(Error::BadSig);
    }
    // SSH peers may omit leading zero octets from the unsigned signature integer.
    // RustCrypto expects exactly the modulus width.
    let mut padded;
    let signature = if signature.len() < key.size() {
        padded = alloc::vec![0; key.size()];
        padded[key.size() - signature.len()..].copy_from_slice(signature);
        padded.as_slice()
    } else {
        signature
    };
    key.verify(padding, &hash, signature)
        .map_err(|_| Error::BadSig)
}
