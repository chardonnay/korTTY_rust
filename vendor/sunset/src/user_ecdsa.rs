//! Portable RustCrypto user signatures for the NIST curves used by RFC 5656.

use p256_portable::ecdsa::signature::hazmat::PrehashSigner as _;
use p256_portable::elliptic_curve::{pkcs8, sec1::ToEncodedPoint as _};
use pkcs8::AssociatedOid as _;
use pkcs8::der::Decode as _;
use sec1::EcPrivateKey;
use sha2::Digest as _;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::packets::{ECDSAPubKey, ECDSASig, PubKey, Signature};
use crate::sign::SigType;
use crate::sshwire::{self, BinString, Blob, Mpint, SSHEncode, SSHSink, WireResult};

#[derive(Clone, Copy)]
pub enum EcdsaCurve {
    NistP256,
    NistP384,
    NistP521,
}

impl EcdsaCurve {
    pub(crate) fn sig_type(self) -> SigType {
        match self {
            Self::NistP256 => SigType::ECDSA256,
            Self::NistP384 => SigType::ECDSA384,
            Self::NistP521 => SigType::ECDSA521,
        }
    }

    fn from_oid(oid: pkcs8::ObjectIdentifier) -> Result<Self> {
        match oid {
            p256_portable::NistP256::OID => Ok(Self::NistP256),
            p384_portable::NistP384::OID => Ok(Self::NistP384),
            p521_portable::NistP521::OID => Ok(Self::NistP521),
            _ => Err(Error::BadKey),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Secret {
    P256(p256_portable::SecretKey),
    P384(p384_portable::SecretKey),
    P521(p521_portable::SecretKey),
}

#[derive(Clone, PartialEq)]
pub struct EcdsaUserKey {
    secret: Secret,
    public: PubKey<'static>,
}

impl Eq for EcdsaUserKey {}

macro_rules! with_public_point {
    ($curve:ident, $kind:ident, $pubkey:ident, $secret:expr, $expected:expr) => {{
        let secret = $secret;
        let public = secret.public_key();
        for encoded in $expected.into_iter().flatten() {
            let included =
                $curve::PublicKey::from_sec1_bytes(encoded).map_err(|_| Error::BadKey)?;
            if included != public {
                return Err(Error::BadKey);
            }
        }
        let encoded = public.to_encoded_point(false);
        let point = heapless::Vec::from_slice(encoded.as_bytes()).map_err(|_| Error::BadKey)?;
        Ok(Self {
            secret: Secret::$kind(secret),
            public: PubKey::$pubkey(ECDSAPubKey { point }),
        })
    }};
}

impl EcdsaUserKey {
    pub(crate) fn from_scalar(
        curve: EcdsaCurve,
        scalar: &[u8],
        public: [Option<&[u8]>; 2],
    ) -> Result<Self> {
        match curve {
            EcdsaCurve::NistP256 => with_public_point!(
                p256_portable,
                P256,
                ECDSA256,
                p256_portable::SecretKey::from_slice(scalar).map_err(|_| Error::BadKey)?,
                public
            ),
            EcdsaCurve::NistP384 => with_public_point!(
                p384_portable,
                P384,
                ECDSA384,
                p384_portable::SecretKey::from_slice(scalar).map_err(|_| Error::BadKey)?,
                public
            ),
            EcdsaCurve::NistP521 => with_public_point!(
                p521_portable,
                P521,
                ECDSA521,
                p521_portable::SecretKey::from_slice(scalar).map_err(|_| Error::BadKey)?,
                public
            ),
        }
    }

    pub(crate) fn from_pkcs8(info: pkcs8::PrivateKeyInfo<'_>) -> Result<Self> {
        if info.algorithm.oid != p256_portable::elliptic_curve::ALGORITHM_OID {
            return Err(Error::BadKey);
        }
        let oid = info.algorithm.parameters_oid().map_err(|_| Error::BadKey)?;
        let curve = EcdsaCurve::from_oid(oid)?;
        let key = EcPrivateKey::from_der(info.private_key).map_err(|_| Error::BadKey)?;
        if key
            .parameters
            .is_some_and(|params| params.named_curve() != Some(oid))
        {
            return Err(Error::BadKey);
        }
        Self::from_scalar(curve, key.private_key, [key.public_key, info.public_key])
    }

    pub(crate) fn from_sec1(der: &[u8]) -> Result<Self> {
        let key = EcPrivateKey::from_der(der).map_err(|_| Error::BadKey)?;
        let oid = key
            .parameters
            .and_then(|params| params.named_curve())
            .ok_or(Error::BadKey)?;
        let curve = EcdsaCurve::from_oid(oid)?;
        Self::from_scalar(curve, key.private_key, [key.public_key, None])
    }

    pub(crate) fn pubkey(&self) -> PubKey<'static> {
        self.public.clone()
    }

    pub(crate) fn curve(&self) -> EcdsaCurve {
        match self.secret {
            Secret::P256(_) => EcdsaCurve::NistP256,
            Secret::P384(_) => EcdsaCurve::NistP384,
            Secret::P521(_) => EcdsaCurve::NistP521,
        }
    }

    pub(crate) fn sign(&self, message: &impl SSHEncode) -> Result<EcdsaUserSignature> {
        let curve = self.curve();
        match &self.secret {
            Secret::P256(key) => {
                let key = p256_portable::ecdsa::SigningKey::from(key);
                let mut digest = sha2::Sha256::new();
                sshwire::hash_ser(&mut digest, message)?;
                let signature: p256_portable::ecdsa::Signature = key
                    .sign_prehash(&digest.finalize())
                    .map_err(|_| Error::BadSig)?;
                let (r, s) = signature.split_bytes();
                EcdsaUserSignature::new(curve, &r, &s)
            }
            Secret::P384(key) => {
                let key = p384_portable::ecdsa::SigningKey::from(key);
                let mut digest = sha2::Sha384::new();
                sshwire::hash_ser(&mut digest, message)?;
                let signature: p384_portable::ecdsa::Signature = key
                    .sign_prehash(&digest.finalize())
                    .map_err(|_| Error::BadSig)?;
                let (r, s) = signature.split_bytes();
                EcdsaUserSignature::new(curve, &r, &s)
            }
            Secret::P521(key) => {
                let mut digest = sha2::Sha512::new();
                sshwire::hash_ser(&mut digest, message)?;
                let signature = sign_p521(key, &digest.finalize())?;
                let (r, s) = signature.split_bytes();
                EcdsaUserSignature::new(curve, &r, &s)
            }
        }
    }

    pub(crate) fn encode(&self, output: &mut dyn SSHSink) -> WireResult<()> {
        self.curve().sig_type().algorithm_name().enc(output)?;
        match &self.secret {
            Secret::P256(key) => BinString(&Zeroizing::new(key.to_bytes())[..]).enc(output),
            Secret::P384(key) => BinString(&Zeroizing::new(key.to_bytes())[..]).enc(output),
            Secret::P521(key) => BinString(&Zeroizing::new(key.to_bytes())[..]).enc(output),
        }
    }
}

fn sign_p521(
    key: &p521_portable::SecretKey,
    digest: &[u8],
) -> Result<p521_portable::ecdsa::Signature> {
    use ecdsa_portable::hazmat::{bits2field, sign_prehashed};
    use p521_portable::{FieldBytes, NistP521, Scalar};

    let digest = bits2field::<NistP521>(digest).map_err(|_| Error::BadSig)?;
    let private = Zeroizing::new(key.to_nonzero_scalar());
    let mut bytes = Zeroizing::new(FieldBytes::default());
    for _ in 0..16 {
        crate::random::fill_random(&mut bytes[..])?;
        // Sample uniformly from 521 bits before rejecting zero or values
        // outside the scalar field. This keeps entropy failures fallible.
        bytes[0] &= 1;
        if let Some(nonce) = Option::<Scalar>::from(Scalar::from_bytes(&bytes))
            && !bool::from(nonce.is_zero())
        {
            let nonce = Zeroizing::new(nonce);
            let (signature, _) = sign_prehashed::<NistP521, _>(private.as_ref(), *nonce, &digest)
                .map_err(|_| Error::BadSig)?;
            return Ok(signature);
        }
    }
    Err(Error::msg("could not sample a P-521 ECDSA nonce"))
}

pub struct EcdsaUserSignature {
    curve: EcdsaCurve,
    r: heapless::Vec<u8, 66>,
    s: heapless::Vec<u8, 66>,
}

impl EcdsaUserSignature {
    fn new(curve: EcdsaCurve, r: &[u8], s: &[u8]) -> Result<Self> {
        let size = match curve {
            EcdsaCurve::NistP256 => 32,
            EcdsaCurve::NistP384 => 48,
            EcdsaCurve::NistP521 => 66,
        };
        if r.len() > size || s.len() > size {
            return Err(Error::BadSig);
        }
        Ok(Self {
            curve,
            r: heapless::Vec::from_slice(r).map_err(|_| Error::BadSig)?,
            s: heapless::Vec::from_slice(s).map_err(|_| Error::BadSig)?,
        })
    }

    pub(crate) fn from_wire(curve: EcdsaCurve, signature: ECDSASig<'_>) -> Result<Self> {
        Self::new(curve, signature.r.as_ref(), signature.s.as_ref())
    }

    pub(crate) fn as_wire(&self) -> Signature<'_> {
        let signature = Blob(ECDSASig {
            r: Mpint::new(&self.r),
            s: Mpint::new(&self.s),
        });
        match self.curve {
            EcdsaCurve::NistP256 => Signature::ECDSA256(signature),
            EcdsaCurve::NistP384 => Signature::ECDSA384(signature),
            EcdsaCurve::NistP521 => Signature::ECDSA521(signature),
        }
    }
}
