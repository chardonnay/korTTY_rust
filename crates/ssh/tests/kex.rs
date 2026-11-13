use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use kortty_ssh::{
    Authentication, Connect, ConnectError, Endpoint, SshSession, TrustMode, TrustStore, WindowSize,
};
use sha2::{Digest, Sha256};
use sunset::ed25519_dalek::{Signer, SigningKey};

#[test]
fn rejects_signed_zero_x25519_before_first_use_approval() {
    let error = reject_peer("zero", |stream| send_zero_point_reply(stream, None));
    assert!(
        matches!(error, ConnectError::Protocol(sunset::Error::BadKex)),
        "{error:?}"
    );
}

#[test]
fn ignores_wrong_kex_guess_before_decoding_or_host_key_approval() {
    let decoy = SigningKey::from_bytes(&[43; 32]);
    let key = ed25519_host_key(decoy.verifying_key().as_bytes());
    for (name, guess) in [
        ("guessed-host-key", ed25519_reply(&key, &[9; 32], &[0; 64])),
        ("malformed-guess", vec![31]),
    ] {
        let error = reject_peer(name, |stream| send_zero_point_reply(stream, Some(guess)));
        assert!(
            matches!(error, ConnectError::Protocol(sunset::Error::BadKex)),
            "{error:?}"
        );
    }
}

#[test]
fn rejects_ed25519_identity_key_before_first_use_approval() {
    let error = reject_peer("ed25519-identity", |stream| {
        let (mut stream, _, _, _) = exchange_kex_init(stream, true, "ssh-ed25519", false);
        assert_eq!(read_packet(&mut stream).first(), Some(&30));
        let mut identity = [0; 32];
        identity[0] = 1;
        let mut signature = [0; 64];
        signature[..32].fill(0x66);
        signature[0] = 0x58;
        signature[32] = 1;
        let key = ed25519_host_key(&identity);
        write_packet(stream.get_mut(), &ed25519_reply(&key, &[9; 32], &signature));
        assert_closed(&mut stream);
    });
    assert!(
        matches!(error, ConnectError::Protocol(sunset::Error::BadSig)),
        "{error:?}"
    );
}

#[test]
fn rejects_legacy_kex_before_sending_ephemeral_key_or_credentials() {
    let error = reject_peer("legacy", |stream| {
        // This protocol test double omits the strict KEX extension.
        let (mut stream, _, _, _) = exchange_kex_init(stream, false, "ssh-ed25519", false);
        assert_closed(&mut stream);
    });
    assert!(
        matches!(
            error,
            ConnectError::Protocol(sunset::Error::AlgoNoMatch {
                algo: "strict key exchange"
            })
        ),
        "{error:?}"
    );
}

#[test]
fn rejects_ecdsa_key_with_a_different_curve_name_before_userauth() {
    let error = reject_peer("wrong-curve", |stream| {
        // This protocol test double substitutes a curve identifier inside the key blob.
        let algorithm = "ecdsa-sha2-nistp256";
        let (mut stream, _, _, _) = exchange_kex_init(stream, true, algorithm, false);
        assert_eq!(read_packet(&mut stream).first(), Some(&30));
        let mut key = Vec::new();
        ssh_string(&mut key, algorithm.as_bytes());
        ssh_string(&mut key, b"nistp384");
        ssh_string(&mut key, &[4; 65]);
        let mut signature_values = Vec::new();
        ssh_string(&mut signature_values, &[1]);
        ssh_string(&mut signature_values, &[1]);
        let mut signature = Vec::new();
        ssh_string(&mut signature, algorithm.as_bytes());
        ssh_string(&mut signature, &signature_values);
        let mut reply = vec![31];
        ssh_string(&mut reply, &key);
        ssh_string(&mut reply, &[9; 32]);
        ssh_string(&mut reply, &signature);
        write_packet(stream.get_mut(), &reply);
        assert_closed(&mut stream);
    });
    assert!(
        matches!(error, ConnectError::Protocol(sunset::Error::BadKey)),
        "{error:?}"
    );
}

fn reject_peer(name: &str, test_peer: impl FnOnce(TcpStream) + Send + 'static) -> ConnectError {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        test_peer(stream);
    });
    let directory = std::env::temp_dir().join(format!(
        "kortty-ssh-{name}-kex-{}-{}",
        std::process::id(),
        address.port()
    ));
    std::fs::create_dir(&directory).unwrap();
    let trust = TrustStore::new(directory.join("pins"), TrustMode::Strict);
    let result = SshSession::connect(
        Connect {
            address,
            endpoint: Endpoint::new("127.0.0.1", address.port()).unwrap(),
            username: "test-only".into(),
            authentication: Authentication::password("never transmitted".into()),
            term: "xterm-256color".into(),
            size: WindowSize {
                columns: 80,
                rows: 24,
                pixel_width: 0,
                pixel_height: 0,
            },
        },
        trust,
    );
    peer.join().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    match result {
        Ok(_) => panic!("SSH session accepted a forbidden key exchange"),
        Err(error) => error,
    }
}

const IDENT: &[u8] = b"SSH-2.0-kortty-kex-test-peer";

fn exchange_kex_init(
    stream: TcpStream,
    strict: bool,
    host_key_algorithm: &str,
    wrong_guess: bool,
) -> (BufReader<TcpStream>, String, Vec<u8>, Vec<u8>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    let mut stream = BufReader::new(stream);
    let mut client_ident = String::new();
    stream.read_line(&mut client_ident).unwrap();
    stream.get_mut().write_all(IDENT).unwrap();
    stream.get_mut().write_all(b"\r\n").unwrap();
    let client_kex = read_packet(&mut stream);
    assert_eq!(client_kex.first(), Some(&20));

    let mut server_kex = vec![20];
    server_kex.extend_from_slice(&[7; 16]);
    let kex = match (strict, wrong_guess) {
        (true, true) => "unsupported@fixture,curve25519-sha256,kex-strict-s-v00@openssh.com",
        (true, false) => "curve25519-sha256,kex-strict-s-v00@openssh.com",
        (false, _) => "curve25519-sha256",
    };
    for names in [
        kex,
        host_key_algorithm,
        "aes256-ctr",
        "aes256-ctr",
        "hmac-sha2-256",
        "hmac-sha2-256",
        "none",
        "none",
        "",
        "",
    ] {
        ssh_string(&mut server_kex, names.as_bytes());
    }
    server_kex.extend_from_slice(&[u8::from(wrong_guess), 0, 0, 0, 0]);
    write_packet(stream.get_mut(), &server_kex);
    (stream, client_ident, client_kex, server_kex)
}

fn send_zero_point_reply(stream: TcpStream, guess: Option<Vec<u8>>) {
    // This protocol test double deliberately signs a forbidden exchange.
    // The signing key is public test data and never used in production.
    let (mut stream, client_ident, client_kex, server_kex) =
        exchange_kex_init(stream, true, "ssh-ed25519", guess.is_some());
    let client_share = read_packet(&mut stream);
    assert_eq!(&client_share[..5], &[30, 0, 0, 0, 32]);
    assert_eq!(client_share.len(), 37);

    let signing = SigningKey::from_bytes(&[42; 32]);
    if let Some(guess) = guess {
        write_packet(stream.get_mut(), &guess);
    }
    let host_key = ed25519_host_key(signing.verifying_key().as_bytes());
    let server_share = [0; 32];
    let mut exchange = Vec::new();
    for field in [
        client_ident.trim_end_matches(['\r', '\n']).as_bytes(),
        IDENT,
        &client_kex,
        &server_kex,
        &host_key,
        &client_share[5..],
        &server_share,
        // SSH mpint zero is encoded with length zero.
        &[],
    ] {
        ssh_string(&mut exchange, field);
    }
    let signature = signing.sign(&Sha256::digest(&exchange)).to_bytes();
    write_packet(
        stream.get_mut(),
        &ed25519_reply(&host_key, &server_share, &signature),
    );
    assert_closed(&mut stream);
}

fn ed25519_host_key(key: &[u8; 32]) -> Vec<u8> {
    let mut host_key = Vec::new();
    ssh_string(&mut host_key, b"ssh-ed25519");
    ssh_string(&mut host_key, key);
    host_key
}

fn ed25519_reply(key: &[u8], share: &[u8; 32], signature: &[u8; 64]) -> Vec<u8> {
    let mut signature_blob = Vec::new();
    ssh_string(&mut signature_blob, b"ssh-ed25519");
    ssh_string(&mut signature_blob, signature);
    let mut reply = vec![31];
    ssh_string(&mut reply, key);
    ssh_string(&mut reply, share);
    ssh_string(&mut reply, &signature_blob);
    reply
}

fn assert_closed(stream: &mut impl Read) {
    // Hold the socket open until the client aborts, and reject any further
    // transport packet as well as any attempt at SSH user authentication.
    let mut extra = [0; 1];
    assert_eq!(stream.read(&mut extra).unwrap(), 0);
}

fn ssh_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
    out.extend_from_slice(bytes);
}

fn write_packet(stream: &mut TcpStream, payload: &[u8]) {
    let mut padding = 8 - (5 + payload.len()) % 8;
    if padding < 4 {
        padding += 8;
    }
    let mut packet = Vec::new();
    packet.extend_from_slice(
        &u32::try_from(1 + payload.len() + padding)
            .unwrap()
            .to_be_bytes(),
    );
    packet.push(u8::try_from(padding).unwrap());
    packet.extend_from_slice(payload);
    packet.resize(packet.len() + padding, 0);
    stream.write_all(&packet).unwrap();
}

fn read_packet(stream: &mut impl Read) -> Vec<u8> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!((5..=32 * 1024).contains(&length));
    let mut packet = vec![0; length];
    stream.read_exact(&mut packet).unwrap();
    let padding = usize::from(packet[0]);
    assert!(padding < length);
    packet[1..length - padding].to_vec()
}
