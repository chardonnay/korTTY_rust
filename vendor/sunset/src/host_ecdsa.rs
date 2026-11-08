//! Client host proof verification using the portable RustCrypto curve providers.

use p256_portable::ecdsa::signature::hazmat::PrehashVerifier as _;
use sha2::Digest as _;

use crate::error::{Error, Result};
use crate::packets::ECDSASig;
use crate::sign::copy_right_aligned;
use crate::sshwire::{self, SSHEncode};

macro_rules! verify_curve {
    ($verify:ident, $curve:ident, $digest:ty) => {
        pub(crate) fn $verify(
            point: &[u8],
            message: &dyn SSHEncode,
            signature: &ECDSASig<'_>,
        ) -> Result<()> {
            let key = $curve::ecdsa::VerifyingKey::from_sec1_bytes(point)
                .map_err(|_| Error::BadKey)?;
            let mut r = $curve::FieldBytes::default();
            let mut s = $curve::FieldBytes::default();
            copy_right_aligned(signature.r.as_ref(), &mut r)
                .map_err(|_| Error::BadSig)?;
            copy_right_aligned(signature.s.as_ref(), &mut s)
                .map_err(|_| Error::BadSig)?;
            let signature = $curve::ecdsa::Signature::from_scalars(r, s)
                .map_err(|_| Error::BadSig)?;
            let mut hash = <$digest>::new();
            sshwire::hash_ser(&mut hash, message)?;
            key.verify_prehash(&hash.finalize(), &signature)
                .map_err(|_| Error::BadSig)
        }
    };
}

// RFC 5656 §§3.1.2 and 6.2.1 select the digest by the curve size.
verify_curve!(verify_p256, p256_portable, sha2::Sha256);
verify_curve!(verify_p384, p384_portable, sha2::Sha384);
verify_curve!(verify_p521, p521_portable, sha2::Sha512);
