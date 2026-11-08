#![cfg_attr(fuzzing, allow(dead_code))]
#![cfg_attr(fuzzing, allow(unreachable_code))]
#![cfg_attr(fuzzing, allow(unused_variables))]

#[allow(unused_imports)]
use {
    crate::error::*,
    log::{debug, error, info, log, trace, warn},
};

use ed25519_dalek as dalek;
use ed25519_dalek::{Signer, Verifier};
use zeroize::ZeroizeOnDrop;

use crate::*;
use packets::{Ed25519PubKey, Ed25519Sig, PubKey, Signature};
use sshnames::*;
use sshwire::{BinString, Blob, SSHDecode, SSHEncode, WireError, WireResult};

use core::mem::discriminant;

#[cfg(feature = "ecdsa-userkeys")]
use alloc::boxed::Box;
#[cfg(feature = "ecdsa-userkeys")]
use crate::user_ecdsa::{EcdsaCurve, EcdsaUserKey, EcdsaUserSignature};

// RSA requires alloc.
#[cfg(feature = "rsa")]
use packets::RSAPubKey;
#[cfg(feature = "rsa")]
use rsa::signature::{DigestSigner as _, DigestVerifier as _};

#[cfg(feature = "ecdsa256")]
use crate::packets::ECDSAPubKey;
#[cfg(all(feature = "ecdsa256", not(feature = "ecdsa-hostkeys")))]
use ecdsa::VerifyingKey;
#[cfg(feature = "ecdsa256")]
use ecdsa::signature::hazmat::PrehashSigner as _;
#[cfg(all(feature = "ecdsa256", not(feature = "ecdsa-hostkeys")))]
use ecdsa::signature::hazmat::PrehashVerifier as _;
#[cfg(feature = "ecdsa256")]
use p256::NistP256;

// only required for some configurations
#[allow(unused_imports)]
use digest::Digest;

// TODO remove once we use byupdate.
// signatures are for hostkey (32 byte sessiid) or pubkey (auth packet || sessid).
// we assume a max 40 character username here.
const MAX_ED25519_SIG_MSG: usize = 1
    + 4
    + 40
    + 4
    + 14
    + 4
    + 9
    + 1
    + 4
    + SSH_NAME_CURVE25519_LIBSSH.len()
    + 4
    + 32
    + 32;

#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy)]
pub enum SigType {
    Ed25519,
    #[cfg(feature = "rsa")]
    RSA,
    #[cfg(any(feature = "ecdsa256", feature = "ecdsa-hostkeys"))]
    ECDSA256,
    #[cfg(feature = "ecdsa-hostkeys")]
    ECDSA384,
    #[cfg(feature = "ecdsa-hostkeys")]
    ECDSA521,
}

#[cfg(any(feature = "ecdsa256", feature = "ecdsa-hostkeys"))]
pub(crate) fn copy_right_aligned(src: &[u8], dest: &mut [u8]) -> Result<(), ()> {
    let Some(offset) = dest.len().checked_sub(src.len()) else { return Err(()) };
    dest[offset..].copy_from_slice(src);
    Ok(())
}

impl SigType {
    /// Must be a valid name
    pub fn from_name(name: &'static str) -> Result<Self> {
        match name {
            SSH_NAME_ED25519 => Ok(SigType::Ed25519),
            #[cfg(feature = "rsa")]
            SSH_NAME_RSA_SHA256 => Ok(SigType::RSA),
            #[cfg(any(feature = "ecdsa256", feature = "ecdsa-hostkeys"))]
            SSH_NAME_ECDSA256 => Ok(SigType::ECDSA256),
            #[cfg(feature = "ecdsa-hostkeys")]
            SSH_NAME_ECDSA384 => Ok(SigType::ECDSA384),
            #[cfg(feature = "ecdsa-hostkeys")]
            SSH_NAME_ECDSA521 => Ok(SigType::ECDSA521),
            _ => Error::bug(),
        }
    }

    /// Returns a valid name
    pub fn algorithm_name(&self) -> &'static str {
        match self {
            SigType::Ed25519 => SSH_NAME_ED25519,
            #[cfg(feature = "rsa")]
            SigType::RSA => SSH_NAME_RSA_SHA256,
            #[cfg(any(feature = "ecdsa256", feature = "ecdsa-hostkeys"))]
            SigType::ECDSA256 => SSH_NAME_ECDSA256,
            #[cfg(feature = "ecdsa-hostkeys")]
            SigType::ECDSA384 => SSH_NAME_ECDSA384,
            #[cfg(feature = "ecdsa-hostkeys")]
            SigType::ECDSA521 => SSH_NAME_ECDSA521,
        }
    }

    #[cfg(fuzzing)]
    fn fuzz_fake_verify(&self, sig: &Signature) -> Result<()> {
        let b = match &sig {
            Signature::Ed25519(e) => e.sig.0,
            #[cfg(feature = "rsa")]
            Signature::RSA(e) => e.sig.0,
            #[cfg(any(feature = "ecdsa256", feature = "ecdsa-hostkeys"))]
            Signature::ECDSA256(e) => e.0.r.as_ref(),
            #[cfg(feature = "ecdsa-hostkeys")]
            Signature::ECDSA384(e) | Signature::ECDSA521(e) => e.0.r.as_ref(),
            Signature::Unknown(_) => panic!(),
        };

        if b.get(..3) == Some(b"bad") { Err(Error::BadSig) } else { Ok(()) }
    }

    /// Returns `Ok(())` on success
    pub fn verify(
        &self,
        pubkey: &PubKey,
        msg: &dyn SSHEncode,
        sig: &Signature,
    ) -> Result<()> {
        // Check that the signature type is known
        let sig_type = sig.sig_type().map_err(|_| Error::BadSig)?;

        // `self` is the expected signature type from kex/auth packet
        // This would also get caught by SignatureMismatch below
        // but that error message is intended for mismatch key vs sig.
        if discriminant(&sig_type) != discriminant(self) {
            warn!(
                "Received {:?} signature, expecting {}",
                sig.algorithm_name(),
                self.algorithm_name()
            );
            return Err(Error::BadSig);
        }

        let ret = match (self, pubkey, sig) {
            (SigType::Ed25519, PubKey::Ed25519(k), Signature::Ed25519(s)) => {
                Self::verify_ed25519(k, msg, s)
            }

            #[cfg(feature = "rsa")]
            (SigType::RSA, PubKey::RSA(k), Signature::RSA(s)) => {
                Self::verify_rsa(k, msg, s)
            }

            #[cfg(feature = "ecdsa-hostkeys")]
            (SigType::ECDSA256, PubKey::ECDSA256(key), Signature::ECDSA256(s)) => {
                crate::host_ecdsa::verify_p256(&key.point, msg, &s.0)
            }

            #[cfg(feature = "ecdsa-hostkeys")]
            (SigType::ECDSA384, PubKey::ECDSA384(key), Signature::ECDSA384(s)) => {
                crate::host_ecdsa::verify_p384(&key.point, msg, &s.0)
            }

            #[cfg(feature = "ecdsa-hostkeys")]
            (SigType::ECDSA521, PubKey::ECDSA521(key), Signature::ECDSA521(s)) => {
                crate::host_ecdsa::verify_p521(&key.point, msg, &s.0)
            }

            #[cfg(all(feature = "ecdsa256", not(feature = "ecdsa-hostkeys")))]
            (
                SigType::ECDSA256,
                PubKey::ECDSA256(key),
                Signature::ECDSA256(s),
            ) => {
                let key = VerifyingKey::<NistP256>::from_sec1_bytes(&key.point)
                    .map_err(|_| Error::BadKey)?;
                Self::verify_ecdsa(&key, msg, &s.0)
            }

            _ => {
                warn!(
                    "Signature \"{:?}\" doesn't match key type \"{:?}\"",
                    sig.algorithm_name(),
                    pubkey.algorithm_name(),
                );
                Err(Error::BadSig)
            }
        };

        #[cfg(fuzzing)]
        return self.fuzz_fake_verify(sig);

        ret
    }

    fn verify_ed25519(
        k: &Ed25519PubKey,
        msg: &dyn SSHEncode,
        s: &Ed25519Sig,
    ) -> Result<()> {
        let k: &[u8; 32] = &k.key.0;
        let k = dalek::VerifyingKey::from_bytes(k).map_err(|_| Error::BadKey)?;

        let s: &[u8; 64] = s.sig.0.try_into().map_err(|_| Error::BadSig)?;
        let s: dalek::Signature = s.into();
        // TODO: pending merge of https://github.com/dalek-cryptography/curve25519-dalek/pull/556
        // In the interim we use a fixed buffer.
        // dalek::hazmat::raw_verify_byupdate(
        //     &k,
        //     |h: &mut sha2::Sha512| {
        //         sshwire::hash_ser(h, msg).map_err(|_| dalek::SignatureError::new())
        //     },
        //     &s,
        // )
        // .map_err(|_| Error::BadSig)
        let mut buf = [0; MAX_ED25519_SIG_MSG];
        let l = sshwire::write_ssh(&mut buf, msg)?;
        let buf = &buf[..l];
        k.verify(buf, &s).map_err(|_| Error::BadSig)
    }

    #[cfg(feature = "rsa")]
    fn verify_rsa(
        k: &packets::RSAPubKey,
        msg: &dyn SSHEncode,
        s: &packets::RSASig,
    ) -> Result<()> {
        let verifying_key =
            rsa::pkcs1v15::VerifyingKey::<sha2_modern::Sha256>::new(k.key.clone());
        let signature = s.sig.0.try_into().map_err(|e| {
            trace!("RSA bad signature: {e}");
            Error::BadSig
        })?;

        verifying_key
            .verify_digest(
                |h| {
                    sshwire::hash_ser(h, msg)
                        .map_err(|_| rsa::signature::Error::new())
                },
                &signature,
            )
            .map_err(|e| {
                trace!("RSA verify failed: {e}");
                Error::BadSig
            })
    }

    #[cfg(all(feature = "ecdsa256", not(feature = "ecdsa-hostkeys")))]
    fn verify_ecdsa<C: ecdsa::EcdsaCurve + ecdsa::elliptic_curve::CurveArithmetic>(
        k: &VerifyingKey<C>,
        msg: &dyn SSHEncode,
        sig: &packets::ECDSASig,
    ) -> Result<()>
    where
        ecdsa::SignatureSize<C>: ecdsa::elliptic_curve::array::ArraySize,
    {
        // RFC5656 defined r and s as mpint, so SSH will trim leading zeros.
        // ecdsa crate requires exact sized inputs, so copy them in
        // zero padded (big endian).
        let mut pad_r = ecdsa::elliptic_curve::FieldBytes::<C>::default();
        let mut pad_s = ecdsa::elliptic_curve::FieldBytes::<C>::default();
        copy_right_aligned(sig.r.as_ref(), &mut pad_r).map_err(|_| {
            debug!("Bad signature r");
            error::BadSig.build()
        })?;
        copy_right_aligned(sig.s.as_ref(), &mut pad_s).map_err(|_| {
            debug!("Bad signature s");
            error::BadSig.build()
        })?;

        let sig =
            ecdsa::Signature::<C>::from_scalars(pad_r, pad_s).map_err(|_| {
                trace!("ECDSA bad signature");
                error::BadSig.build()
            })?;

        // TODO: once ecdsa is non-rc and all the crates like digest are aligned,
        // can use verify_digest() instead of hazmat::verify_prehashed.
        // k.verify_digest(|&mut d| {
        //     sshwire::hash_ser(d, msg).map_err(|_| ecdsa::Error::new())
        // }, &signature).map_err(|_| {
        //     trace!("ECDSA verify failed");
        //     Error::BadSig
        // })

        let mut h = sha2::Sha256::new();
        sshwire::hash_ser(&mut h, msg)?;
        let h = h.finalize();

        k.verify_prehash(&h, &sig).map_err(|_| {
            trace!("ECDSA verify failed");
            Error::BadSig
        })
    }
}

pub enum OwnedSig {
    // just store raw bytes here.
    Ed25519([u8; 64]),
    #[cfg(feature = "rsa")]
    RSA(Box<[u8]>),
    #[cfg(feature = "ecdsa256")]
    ECDSA256 {
        r: [u8; 32],
        s: [u8; 32],
    },
    #[cfg(feature = "ecdsa-userkeys")]
    PortableEcdsa(EcdsaUserSignature),
}

#[cfg(feature = "rsa")]
impl From<rsa::pkcs1v15::Signature> for OwnedSig {
    fn from(s: rsa::pkcs1v15::Signature) -> Self {
        OwnedSig::RSA(s.into())
    }
}

impl TryFrom<Signature<'_>> for OwnedSig {
    type Error = Error;
    fn try_from(s: Signature) -> Result<Self> {
        match s {
            Signature::Ed25519(s) => {
                let s: [u8; 64] = s.sig.0.try_into().map_err(|_| Error::BadSig)?;
                Ok(OwnedSig::Ed25519(s))
            }
            #[cfg(feature = "rsa")]
            Signature::RSA(s) => Ok(OwnedSig::RSA(s.sig.0.into())),
            #[cfg(feature = "ecdsa256")]
            Signature::ECDSA256(sig) => {
                let sig = sig.0;
                let mut r = [0; 32];
                let mut s = [0; 32];
                copy_right_aligned(sig.r.as_ref(), &mut r).map_err(|_| {
                    debug!("Bad signature r");
                    error::BadSig.build()
                })?;
                copy_right_aligned(sig.s.as_ref(), &mut s).map_err(|_| {
                    debug!("Bad signature s");
                    error::BadSig.build()
                })?;
                Ok(OwnedSig::ECDSA256 { r, s })
            }
            #[cfg(all(feature = "ecdsa-userkeys", not(feature = "ecdsa256")))]
            Signature::ECDSA256(sig) => EcdsaUserSignature::from_wire(EcdsaCurve::NistP256, sig.0)
                .map(OwnedSig::PortableEcdsa),
            #[cfg(feature = "ecdsa-userkeys")]
            Signature::ECDSA384(sig) => EcdsaUserSignature::from_wire(EcdsaCurve::NistP384, sig.0)
                .map(OwnedSig::PortableEcdsa),
            #[cfg(feature = "ecdsa-userkeys")]
            Signature::ECDSA521(sig) => EcdsaUserSignature::from_wire(EcdsaCurve::NistP521, sig.0)
                .map(OwnedSig::PortableEcdsa),
            Signature::Unknown(u) => {
                debug!("Unknown {u} signature");
                Err(Error::UnknownMethod { kind: "signature" })
            }
            #[cfg(all(feature = "ecdsa-hostkeys", not(feature = "ecdsa-userkeys")))]
            _ => Err(Error::BadSig),
        }
    }
}
/// Signing key types.
#[derive(Debug, Clone, Copy)]
pub enum KeyType {
    Ed25519,
    #[cfg(feature = "rsa")]
    RSA,
    #[cfg(feature = "ecdsa256")]
    ECDSA256,
}

/// A SSH signing key.
///
/// This may hold the private key part locally
/// or potentially send the signing requests to an SSH agent or other entity.
#[derive(ZeroizeOnDrop, Clone, PartialEq, Eq)]
pub enum SignKey {
    // TODO: we could just have the 32 byte seed here to save memory, but
    // computing the public part may be slow.
    Ed25519(dalek::SigningKey),

    #[zeroize(skip)]
    AgentEd25519(dalek::VerifyingKey),

    #[cfg(feature = "rsa")]
    // TODO zeroize doesn't seem supported? though BigUint has Zeroize
    RSA(rsa::RsaPrivateKey),

    #[cfg(feature = "rsa")]
    #[zeroize(skip)]
    AgentRSA(rsa::RsaPublicKey),

    #[cfg(feature = "ecdsa256")]
    ECDSA256(ecdsa::SigningKey<p256::NistP256>),

    #[cfg(feature = "ecdsa256")]
    #[zeroize(skip)]
    AgentECDSA256(ecdsa::VerifyingKey<p256::NistP256>),

    #[cfg(feature = "ecdsa-userkeys")]
    #[zeroize(skip)] // The RustCrypto SecretKey in each curve variant clears itself on drop.
    PortableEcdsa(Box<EcdsaUserKey>),
}

impl SignKey {
    /// Loads a NIST curve scalar and verifies its associated SEC1 point.
    #[cfg(feature = "ecdsa-userkeys")]
    pub fn ecdsa_private_key(curve: EcdsaCurve, scalar: &[u8], public: &[u8]) -> Result<Self> {
        EcdsaUserKey::from_scalar(curve, scalar, [Some(public), None])
            .map(|key| Self::PortableEcdsa(Box::new(key)))
    }

    /// Loads an ECDSA PKCS#8 key with named curve parameters.
    #[cfg(feature = "ecdsa-userkeys")]
    pub fn ecdsa_pkcs8(
        info: p256_portable::elliptic_curve::pkcs8::PrivateKeyInfo<'_>,
    ) -> Result<Self> {
        EcdsaUserKey::from_pkcs8(info).map(|key| Self::PortableEcdsa(Box::new(key)))
    }

    /// Loads a DER SEC1 private key with named curve parameters.
    #[cfg(feature = "ecdsa-userkeys")]
    pub fn ecdsa_sec1(der: &[u8]) -> Result<Self> {
        EcdsaUserKey::from_sec1(der).map(|key| Self::PortableEcdsa(Box::new(key)))
    }

    pub fn generate(ty: KeyType, bits: Option<usize>) -> Result<Self> {
        match ty {
            KeyType::Ed25519 => {
                if bits.unwrap_or(256) != 256 {
                    return Err(Error::msg("Bad key size"));
                }
                let k = dalek::SigningKey::generate(&mut rand_core::OsRng);
                Ok(Self::Ed25519(k))
            }

            #[cfg(feature = "rsa")]
            KeyType::RSA => {
                let bits = bits.unwrap_or(config::RSA_DEFAULT_KEYSIZE);
                let size_range =
                    config::RSA_MIN_KEYSIZE..=rsa::RsaPublicKey::MAX_SIZE;
                if !(size_range.contains(&bits) && bits.is_multiple_of(8)) {
                    return Err(Error::msg("Bad key size"));
                }

                let k = rsa::RsaPrivateKey::new(&mut random::rng(), bits).map_err(
                    |e| {
                        debug!("RSA key generation error {e}");
                        // RNG shouldn't fail, keysize has been checked
                        Error::build_bug()
                    },
                )?;
                Ok(Self::RSA(k))
            }
            #[cfg(feature = "ecdsa256")]
            KeyType::ECDSA256 => {
                use ecdsa::elliptic_curve::Generate;
                let k =
                    p256::SecretKey::generate_from_rng(&mut crate::random::rng());
                Ok(Self::ECDSA256(k.into()))
            }
        }
    }

    pub fn pubkey(&self) -> PubKey<'_> {
        match self {
            SignKey::Ed25519(k) => {
                let pubk = k.verifying_key().to_bytes();
                PubKey::Ed25519(Ed25519PubKey { key: Blob(pubk) })
            }

            SignKey::AgentEd25519(pk) => {
                PubKey::Ed25519(Ed25519PubKey { key: Blob(pk.to_bytes()) })
            }

            #[cfg(feature = "rsa")]
            SignKey::RSA(k) => PubKey::RSA(RSAPubKey { key: k.into() }),

            #[cfg(feature = "rsa")]
            SignKey::AgentRSA(pk) => PubKey::RSA(RSAPubKey { key: pk.clone() }),

            #[cfg(feature = "ecdsa256")]
            SignKey::ECDSA256(k) => Self::p256_public_key(k.verifying_key()),

            #[cfg(feature = "ecdsa256")]
            SignKey::AgentECDSA256(pk) => Self::p256_public_key(pk),

            #[cfg(feature = "ecdsa-userkeys")]
            SignKey::PortableEcdsa(key) => key.pubkey(),
        }
    }

    #[cfg(feature = "ecdsa256")]
    fn p256_public_key(key: &ecdsa::VerifyingKey<NistP256>) -> PubKey<'_> {
        let encoded = key.to_sec1_point(false);
        // An uncompressed P-256 point is exactly 65 bytes.
        let point = heapless::Vec::from_slice(encoded.as_bytes()).unwrap();
        PubKey::ECDSA256(ECDSAPubKey { point })
    }

    #[cfg(feature = "openssh-key")]
    pub fn from_openssh(k: impl AsRef<[u8]>) -> Result<Self> {
        let k = ssh_key::PrivateKey::from_openssh(k)
            .map_err(|_| Error::msg("Unsupported OpenSSH key"))?;

        k.try_into()
    }

    pub fn from_agent_pubkey(pk: &PubKey) -> Result<Self> {
        match pk {
            PubKey::Ed25519(k) => {
                let k: dalek::VerifyingKey =
                    k.key.0.as_slice().try_into().map_err(|_| Error::BadKey)?;
                Ok(Self::AgentEd25519(k))
            }

            #[cfg(feature = "rsa")]
            PubKey::RSA(k) => Ok(Self::AgentRSA(k.key.clone())),
            #[cfg(feature = "ecdsa256")]
            PubKey::ECDSA256(k) => ecdsa::VerifyingKey::from_sec1_bytes(&k.point)
                .map(Self::AgentECDSA256)
                .map_err(|_| Error::BadKey),

            PubKey::Unknown(_) => Err(Error::msg("Unsupported agent key")),
            #[cfg(feature = "ecdsa-hostkeys")]
            _ => Err(Error::msg("Unsupported agent key")),
        }
    }

    /// Returns whether this `SignKey` can create a given signature type
    pub(crate) fn can_sign(&self, sig_type: SigType) -> bool {
        match self {
            SignKey::Ed25519(_) | SignKey::AgentEd25519(_) => {
                matches!(sig_type, SigType::Ed25519)
            }

            #[cfg(feature = "rsa")]
            SignKey::RSA(_) | SignKey::AgentRSA(_) => {
                matches!(sig_type, SigType::RSA)
            }
            #[cfg(feature = "ecdsa256")]
            SignKey::ECDSA256(_) | SignKey::AgentECDSA256(_) => {
                matches!(sig_type, SigType::ECDSA256)
            }
            #[cfg(feature = "ecdsa-userkeys")]
            SignKey::PortableEcdsa(key) => key.curve().sig_type().algorithm_name() == sig_type.algorithm_name(),
        }
    }

    pub(crate) fn sign(&self, msg: &impl SSHEncode) -> Result<OwnedSig> {
        let sig: OwnedSig = match self {
            SignKey::Ed25519(k) => {
                // TODO: pending merge of https://github.com/dalek-cryptography/curve25519-dalek/pull/556
                // let exk: dalek::hazmat::ExpandedSecretKey = (&k.to_bytes()).into();
                // let sig = dalek::hazmat::raw_sign_byupdate(
                //     &exk,
                //     |h: &mut sha2::Sha512| {
                //         sshwire::hash_ser(h, msg)
                //             .map_err(|_| dalek::SignatureError::new())
                //     },
                //     &k.verifying_key(),
                // )
                // .trap()?;
                let mut buf = [0; MAX_ED25519_SIG_MSG];
                let l = sshwire::write_ssh(&mut buf, msg)?;
                let buf = &buf[..l];
                let sig = k.sign(buf);

                OwnedSig::Ed25519(sig.to_bytes())
            }

            #[cfg(feature = "rsa")]
            SignKey::RSA(k) => {
                let signing_key =
                    rsa::pkcs1v15::SigningKey::<sha2_modern::Sha256>::new(k.clone());
                let sig = signing_key
                    .try_sign_digest(|h| {
                        sshwire::hash_ser(h, msg)
                            .map_err(|_| rsa::signature::Error::new())
                    })
                    .map_err(|e| {
                        trace!("RSA signing failed: {e:?}");
                        Error::build_bug()
                    })?;
                OwnedSig::RSA(sig.into())
            }

            #[cfg(feature = "ecdsa256")]
            SignKey::ECDSA256(k) => {
                let mut h = sha2::Sha256::new();
                sshwire::hash_ser(&mut h, msg)?;
                let h = h.finalize();
                let sig: ecdsa::Signature<NistP256> =
                    k.sign_prehash(&h).map_err(|_| {
                        trace!("ECDSA signing failed");
                        Error::build_bug()
                    })?;
                let (r, s) = sig.split_bytes();
                OwnedSig::ECDSA256 { r: r.into(), s: s.into() }
            }

            #[cfg(feature = "ecdsa-userkeys")]
            SignKey::PortableEcdsa(key) => OwnedSig::PortableEcdsa(key.sign(msg)?),

            // callers should check for agent keys first
            SignKey::AgentEd25519(_) => unreachable!(),
            #[cfg(feature = "rsa")]
            SignKey::AgentRSA(_) => unreachable!(),
            #[cfg(feature = "ecdsa256")]
            SignKey::AgentECDSA256(_) => unreachable!(),
        };

        // {
        //     // Faults in signing can expose the private key. We verify the signature
        //     // just created to avoid this problem.
        //     // TODO: Maybe this needs to be configurable for slow platforms?
        //     let vsig: Signature = (&sig).into();
        //     let sig_type = vsig.sig_type().unwrap();
        //     sig_type.verify(&self.pubkey(), msg, &vsig, parse_ctx)?;
        // }

        Ok(sig)
    }

    pub(crate) fn is_agent(&self) -> bool {
        match self {
            SignKey::Ed25519(_) => false,
            #[cfg(feature = "rsa")]
            SignKey::RSA(_) => false,
            #[cfg(feature = "ecdsa256")]
            SignKey::ECDSA256(_) => false,
            #[cfg(feature = "ecdsa-userkeys")]
            SignKey::PortableEcdsa(_) => false,

            SignKey::AgentEd25519(_) => true,
            #[cfg(feature = "rsa")]
            SignKey::AgentRSA(_) => true,
            #[cfg(feature = "ecdsa256")]
            SignKey::AgentECDSA256(_) => true,
        }
    }
}

impl core::fmt::Debug for SignKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let s = match self {
            Self::Ed25519(_) => "Ed25519",
            Self::AgentEd25519(_) => "AgentEd25519",
            #[cfg(feature = "rsa")]
            Self::RSA(_) => "RSA",
            #[cfg(feature = "rsa")]
            Self::AgentRSA(_) => "AgentRSA",
            #[cfg(feature = "ecdsa256")]
            Self::ECDSA256(_) => "ECDSA256",
            #[cfg(feature = "ecdsa256")]
            Self::AgentECDSA256(_) => "AgentECDSA256",
            #[cfg(feature = "ecdsa-userkeys")]
            Self::PortableEcdsa(key) => key.curve().sig_type().algorithm_name(),
        };
        write!(f, "SignKey::{s}")
    }
}

/// An unspecified encoding of the private key.
///
/// This does not correspond to a SSH RFC, only used internally.
impl sshwire::SSHEncode for SignKey {
    // An arbitrary encoding, similar to
    fn enc(&self, s: &mut dyn sshwire::SSHSink) -> sshwire::WireResult<()> {
        match self {
            SignKey::Ed25519(k) => {
                SSH_NAME_ED25519.enc(s)?;
                BinString(&k.to_bytes()).enc(s)?;
            }
            #[cfg(feature = "rsa")]
            SignKey::RSA(k) => {
                use rsa::traits::{PrivateKeyParts, PublicKeyParts};

                SSH_NAME_RSA.enc(s)?;
                let [p, q]: &[_; _] =
                    k.primes().try_into().map_err(|_| WireError::BadKey)?;
                k.n().enc(s)?;
                k.e().enc(s)?;
                k.d().enc(s)?;
                p.enc(s)?;
                q.enc(s)?;
            }
            #[cfg(feature = "ecdsa256")]
            SignKey::ECDSA256(k) => {
                SSH_NAME_ECDSA256.enc(s)?;
                // Using a fixed length BinString rather than Mpint since
                // ecdsa crate decoding requires a fixed length input.
                sshwire::BinString(&k.to_bytes()).enc(s)?;
            }
            #[cfg(feature = "ecdsa-userkeys")]
            SignKey::PortableEcdsa(key) => key.encode(s)?,
            _ => {
                trace!("Can't encode agent signkeys");
                return Err(WireError::BadKey);
            }
        }
        Ok(())
    }
}

/// Matches `SSHEncode`.
///
/// This does not correspond to a SSH RFC, only used internally.
impl<'de> SSHDecode<'de> for SignKey {
    fn dec<S>(s: &mut S) -> WireResult<Self>
    where
        S: sshwire::SSHSource<'de>,
    {
        let t = <&str>::dec(s)?;
        Ok(match t {
            SSH_NAME_ED25519 => {
                let k = BinString::dec(s)?;
                let k = k.0.try_into().map_err(|_| WireError::BadKey)?;
                let k = dalek::SigningKey::from_bytes(k);
                Self::Ed25519(k)
            }
            #[cfg(feature = "rsa")]
            SSH_NAME_RSA => {
                let n = SSHDecode::dec(s)?;
                let e = SSHDecode::dec(s)?;
                let d = SSHDecode::dec(s)?;
                let p = SSHDecode::dec(s)?;
                let q = SSHDecode::dec(s)?;
                let k = rsa::RsaPrivateKey::from_components(n, e, d, vec![p, q])
                    .map_err(|_| WireError::BadKey)?;
                Self::RSA(k)
            }
            #[cfg(feature = "ecdsa256")]
            SSH_NAME_ECDSA256 => {
                let d = BinString::dec(s)?;
                let d = d.0.try_into().map_err(|_| WireError::BadKey)?;
                Self::ECDSA256(
                    p256::SecretKey::from_bytes(d)
                        .map_err(|_| WireError::BadKey)?
                        .into(),
                )
            }
            #[cfg(all(feature = "ecdsa-userkeys", not(feature = "ecdsa256")))]
            SSH_NAME_ECDSA256 => Self::decode_portable_ecdsa(EcdsaCurve::NistP256, s)?,
            #[cfg(feature = "ecdsa-userkeys")]
            SSH_NAME_ECDSA384 => Self::decode_portable_ecdsa(EcdsaCurve::NistP384, s)?,
            #[cfg(feature = "ecdsa-userkeys")]
            SSH_NAME_ECDSA521 => Self::decode_portable_ecdsa(EcdsaCurve::NistP521, s)?,
            _ => return Err(WireError::UnknownVariant),
        })
    }
}

#[cfg(feature = "ecdsa-userkeys")]
impl SignKey {
    fn decode_portable_ecdsa<'de>(
        curve: EcdsaCurve,
        source: &mut impl sshwire::SSHSource<'de>,
    ) -> WireResult<Self> {
        EcdsaUserKey::from_scalar(curve, BinString::dec(source)?.0, [None, None])
            .map(|key| Self::PortableEcdsa(Box::new(key)))
            .map_err(|_| WireError::BadKey)
    }
}

#[cfg(feature = "openssh-key")]
impl TryFrom<ssh_key::PrivateKey> for SignKey {
    type Error = Error;
    fn try_from(k: ssh_key::PrivateKey) -> Result<Self> {
        match k.key_data() {
            ssh_key::private::KeypairData::Ed25519(k) => {
                Ok(SignKey::Ed25519(k.private.to_bytes().into()))
            }

            #[cfg(feature = "rsa")]
            ssh_key::private::KeypairData::Rsa(k) => {
                Ok(SignKey::RSA(k.try_into().map_err(|_| Error::BadKey)?))
            }
            _ => {
                debug!("Unknown ssh-key algorithm {}", k.algorithm().as_str());
                Err(Error::NotAvailable { what: "ssh key algorithm" })
            }
        }
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::sshwire::{read_ssh, ssh_push_vec};

    // TODO: tests for sign()/verify() and invalid signatures

    #[test]
    fn signkey_enc_dec() {
        let tys = [
            KeyType::Ed25519,
            #[cfg(feature = "rsa")]
            KeyType::RSA,
            #[cfg(feature = "ecdsa256")]
            KeyType::ECDSA256,
        ];

        // Multiple iterations since some seen failures depend
        // on random key structure.
        for _ in 0..20 {
            for ty in tys {
                let k = SignKey::generate(ty, None).unwrap();
                let mut v = vec![];
                ssh_push_vec(&mut v, &k).unwrap();
                let (dk, l) = read_ssh(&v, None).unwrap();

                assert_eq!(l, v.len());
                assert_eq!(k, dk);
            }
        }
    }
}
