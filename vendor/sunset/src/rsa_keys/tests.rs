use std::sync::LazyLock;

use crate::packets::{PubKey, Signature};
use crate::sshwire::{read_ssh, ssh_push_vec};
use crate::{KeyType, SignKey};

use super::*;

pub(crate) static KEY: LazyLock<SignKey> =
    LazyLock::new(|| SignKey::generate(KeyType::RSA, Some(2048)).unwrap());

#[test]
fn rsa_sha2_signatures_roundtrip_and_reject_hash_key_and_message_substitution() {
    let key = &*KEY;
    let public = key.pubkey();
    assert_eq!(public.algorithm_name().unwrap(), "ssh-rsa");
    let mut wire = Vec::new();
    ssh_push_vec(&mut wire, &public).unwrap();
    let (decoded, used): (PubKey<'_>, _) = read_ssh(&wire, None).unwrap();
    assert_eq!(used, wire.len());
    assert_eq!(decoded, public);
    let mut private = zeroize::Zeroizing::new(Vec::new());
    ssh_push_vec(&mut private, key).unwrap();
    let (decoded, used): (SignKey, _) = read_ssh(&private, None).unwrap();
    assert_eq!(used, private.len());
    assert_eq!(&decoded, key);

    let wrong_key = SignKey::generate(KeyType::Ed25519, None).unwrap();
    for (algorithm, other) in [
        (SigType::RSA256, SigType::RSA512),
        (SigType::RSA512, SigType::RSA256),
    ] {
        let signature = key
            .sign_with_type(&"approved transcript", algorithm)
            .unwrap();
        let signature = Signature::from(&signature);
        let mut wire = Vec::new();
        ssh_push_vec(&mut wire, &signature).unwrap();
        let (decoded, used): (Signature<'_>, _) = read_ssh(&wire, None).unwrap();
        assert_eq!(used, wire.len());
        assert_eq!(
            decoded.algorithm_name().unwrap(),
            algorithm.algorithm_name()
        );
        algorithm
            .verify(&public, &"approved transcript", &decoded)
            .unwrap();
        assert!(
            other
                .verify(&public, &"approved transcript", &decoded)
                .is_err()
        );
        assert!(
            algorithm
                .verify(&public, &"changed transcript", &decoded)
                .is_err()
        );
        assert!(
            algorithm
                .verify(&wrong_key.pubkey(), &"approved transcript", &decoded)
                .is_err()
        );
    }
    assert!(SigType::from_name("ssh-rsa").is_err());
    assert!(Signature::check_pubkey_algorithm(&public, "ssh-rsa").is_err());
}

#[test]
fn rsa_wire_keys_reject_negative_oversized_and_weak_moduli() {
    use crate::sshwire::{BinString, Mpint, SSHSink, WireResult};

    struct TestKey<'a>(&'a [u8]);
    impl SSHEncode for TestKey<'_> {
        fn enc(&self, sink: &mut dyn SSHSink) -> WireResult<()> {
            "ssh-rsa".enc(sink)?;
            Mpint::new(&[1, 0, 1]).enc(sink)?;
            // The raw string deliberately bypasses Mpint's non-negative encoding.
            BinString(self.0).enc(sink)
        }
    }

    for modulus in [
        vec![0x80],
        vec![1; crate::config::RSA_MAX_KEYSIZE / 8 + 1],
        vec![0x7f; 128],
    ] {
        let mut bytes = Vec::new();
        ssh_push_vec(&mut bytes, &TestKey(&modulus)).unwrap();
        assert!(read_ssh::<PubKey<'_>>(&bytes, None).is_err());
    }
}

#[test]
fn rsa_sha256_accepts_omitted_signature_zero_but_not_extra_padding() {
    // Public fixture derived with the OS RSA provider from RustCrypto RSA 0.10.0's
    // published tests/examples/pkcs1/rsa2048-priv.der. No private material is stored.
    let bytes = |hex: &str| {
        hex.as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>()
    };
    let modulus = bytes(concat!(
        "b6c42c515f10a6aaf282c63edbe24243a170f3fa2633bd4833637f47ca4f6f36e",
        "03a5d29efc3191ac80f390d874b39e30f414fcec1fca0ed81e547edc2cd382c76",
        "f61c9018973db9fa537972a7c701f6b77e0982dfc15fc01927ee5e7cd94b4f599",
        "ff07013a7c8281bdf22dcbc9ad7cabb7c4311c982f58edb7213ad4558b332266",
        "d743aed8192d1884cadb8b14739a8dada66dc970806d9c7ac450cb13d0d7c575",
        "fb198534fc61bc41bc0f0574e0e0130c7bbbfbdfdc9f6a6e2e3e2aff1cbeac89",
        "ba57884528d55cfb08327a1e8c89f4e003cf2888e933241d9d695bcbbacdc90b4",
        "4e3e095fa37058ea25b13f5e295cbeac6de838ab8c50af61e298975b872f",
    ));
    let signature = bytes(concat!(
        "00022df9310902aeadc3aaae2e3182efdf7e6c479ab24b1565d5ca94a3edede57",
        "7a3464b3babc5fd8f06ffc3b92ea75b4874a4be4797ca04c24e747d513900bd2",
        "cdc850623257a766d921908654c77422082e87e5dcab4629e898df7e5a51e405",
        "08f5cb9fa6ac73abcaea5f4f1049c57ed3dc9cb456b9280f8352e6f6f9c0bc8",
        "38b70e91a11988c139594437a2f0ccde9a1d72dce36b9f0f70deb3c45c118642",
        "263d0c8a9dfad11623621f8dcc82a33734ee723c65d092143d23142a1ee8fb37",
        "58e63eb79a740b3dee1bc39e9ea737f6819809605f664ddb02ea50205f3bbe71d",
        "882184b04cf421e81d726cc7a7e050ff78e6c1e1e82cbc0262f2f8d622444bd",
    ));
    let public = public_components(&modulus, &[1, 0, 1]).unwrap();
    let message = "short RSA signature 164";
    assert_eq!(signature.len(), public.size());
    assert_eq!(signature[0], 0);
    verify(&public, &message, &signature, SigType::RSA256).unwrap();
    verify(&public, &message, &signature[1..], SigType::RSA256).unwrap();
    assert!(verify(&public, &message, &signature[1..], SigType::RSA512).is_err());
    let mut oversized = vec![0];
    oversized.extend_from_slice(&signature);
    assert!(verify(&public, &message, &oversized, SigType::RSA256).is_err());
    assert!(verify(&public, &message, &[], SigType::RSA256).is_err());
}
