use crate::encrypt::{Cipher, Integ, KeyState, KeysSend};
use crate::kex::{Algos, KexOutput, SharedSecret};
use crate::rsa_keys::tests::KEY;
use crate::sshwire::Blob;
use crate::traffic::TrafOut;

use super::*;

#[test]
fn rsa_auth_selects_only_advertised_sha2_and_binds_the_confirmation() {
    let key = &*KEY;
    for (offered, expected) in [
        ("ssh-ed25519,ssh-rsa", None),
        (SSH_NAME_RSA_SHA256, Some(SSH_NAME_RSA_SHA256)),
        (SSH_NAME_RSA_SHA512, Some(SSH_NAME_RSA_SHA512)),
        ("rsa-sha2-256,rsa-sha2-512", Some(SSH_NAME_RSA_SHA512)),
    ] {
        let mut auth = CliAuth::default();
        assert!(auth.signature_for_key(key).unwrap().is_none());
        auth.handle_ext_info(&packets::ExtInfo {
            server_sig_algs: Some(offered.try_into().unwrap()),
        });
        let selected = auth.signature_for_key(key).unwrap();
        assert_eq!(selected.map(|kind| kind.algorithm_name()), expected);
    }

    let mut auth = CliAuth::default();
    auth.state = AuthState::RequestKey {
        key: key.clone(),
        signature: SigType::RSA512,
        defer_signature: false,
    };
    let wrong = UserauthPkOk {
        algo: SSH_NAME_RSA_SHA256,
        key: Blob(key.pubkey()),
    };
    let mut out = TrafOut::new_owned();
    let mut keys = KeyState::new_cleartext();
    let mut context = ParseContext::default();
    let session = SessId::from_slice(&[1; 32]).unwrap();
    assert!(
        auth.auth_pkok(&wrong, &session, &mut context, &mut out.sender(&mut keys))
            .is_err()
    );
}

#[test]
fn rsa_auth_retries_rejected_probe_once_without_sha1_or_hash_substitution() {
    let key = &*KEY;
    let mut auth = CliAuth::default();
    auth.handle_ext_info(&packets::ExtInfo {
        server_sig_algs: Some("rsa-sha2-512,rsa-sha2-256".try_into().unwrap()),
    });
    auth.state = AuthState::RequestKey {
        key: key.clone(),
        signature: SigType::RSA512,
        defer_signature: true,
    };
    let failure = packets::UserauthFailure {
        methods: SSH_AUTHMETHOD_PUBLICKEY.try_into().unwrap(),
        partial: false,
    };
    let mut context = ParseContext::default();
    let mut out = TrafOut::new_owned();
    let mut keys = KeyState::new_cleartext();
    let session = SessId::from_slice(&[1; 32]).unwrap();
    let algorithms = Algos::<Client> {
        kex: SharedSecret::from_name(SSH_NAME_CURVE25519).unwrap(),
        hostsig: SigType::RSA512,
        cipher_enc: Cipher::ChaPoly,
        cipher_dec: Cipher::ChaPoly,
        integ_enc: Integ::ChaPoly,
        integ_dec: Integ::ChaPoly,
        discard_next: false,
        send_ext_info: true,
        strict_kex: true,
        _cs: core::marker::PhantomData,
    };
    let exchange = KexOutput::new_test(b"RSA auth fixture", &session);
    keys.rekey_send(KeysSend::new(&exchange, &session, &algorithms));
    let event = auth
        .failure(&failure, &mut context, &mut out.sender(&mut keys))
        .unwrap();
    assert!(matches!(event, DispatchEvent::None));
    assert!(matches!(
        auth.state,
        AuthState::RequestKey {
            signature: SigType::RSA256,
            defer_signature: true,
            ..
        }
    ));
    let accepted = UserauthPkOk {
        algo: SSH_NAME_RSA_SHA256,
        key: Blob(key.pubkey()),
    };
    let event = auth
        .auth_pkok(
            &accepted,
            &session,
            &mut context,
            &mut out.sender(&mut keys),
        )
        .unwrap();
    assert!(matches!(
        event,
        DispatchEvent::CliEvent(CliEventId::AgentSign)
    ));
    let deferred = auth.sign_deferred_private_key(&session).unwrap();
    let message = auth.fetch_agentsign_msg(&session).unwrap();
    SigType::RSA256
        .verify(&key.pubkey(), &message, &Signature::from(&deferred))
        .unwrap();
    let event = auth
        .failure(&failure, &mut context, &mut out.sender(&mut keys))
        .unwrap();
    assert!(matches!(event, DispatchEvent::CliEvent(CliEventId::Pubkey)));
    assert!(matches!(auth.state, AuthState::Idle));

    let signature = key
        .sign_with_type(&"reviewed message", SigType::RSA256)
        .unwrap();
    assert!(req_packet_pubkey("user", key, SigType::RSA512, Some(&signature), true).is_err());
    let request = req_packet_pubkey("user", key, SigType::RSA256, Some(&signature), true).unwrap();
    let AuthMethod::PubKey(method) = request.method else {
        panic!("expected publickey");
    };
    assert_eq!(method.sig_algo, SSH_NAME_RSA_SHA256);
    assert_eq!(method.pubkey.0.algorithm_name().unwrap(), SSH_NAME_RSA);
    assert_eq!(
        method.sig.unwrap().0.algorithm_name().unwrap(),
        SSH_NAME_RSA_SHA256
    );
}
