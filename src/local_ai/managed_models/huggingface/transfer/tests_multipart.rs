use std::io::{BufRead as _, Write as _};

use pgp::crypto::hash::HashAlgorithm;
use url::Url;

use crate::local_ai::managed_models::{Purpose, split};
use crate::local_ai::test_support::{accept, listener};

use super::super::Repository;
use super::*;

#[test]
fn multipart_resume_rehashes_prior_shards_before_registering_the_complete_model() {
    let profile = crate::snippets::tests::Profile::new();
    let root = profile.0.canonicalize().unwrap();
    let first = split::tests::fixture(0, 2, 2, false);
    let second = split::tests::fixture(1, 2, 2, false);
    let artifact = |index, bytes: &[u8]| Artifact {
        path: format!("fixture-{:05}-of-00002.gguf", index),
        size: bytes.len() as u64,
        digest: hex(&HashAlgorithm::Sha256.digest(bytes).unwrap()),
    };
    let plan = Plan {
        repository: Repository::parse("fixture/split").unwrap(),
        revision: "a".repeat(40),
        file: artifact(1, &first),
        additional: vec![artifact(2, &second)],
        purpose: Purpose::Chat,
    };
    plan.validate().unwrap();
    let (server, _) = listener();
    let mut hub = Hub::new(None, Duration::from_secs(5)).unwrap();
    hub.origin = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
    let first_bytes = first.clone();
    let second_bytes = second.clone();
    let revision = plan.revision.clone();
    let split_offset = 21_usize;
    let first_server = std::thread::spawn(move || {
        for (ordinal, data) in [(1, first_bytes), (2, second_bytes)] {
            let mut socket = accept(&server);
            let headers = read_request(&mut socket, &revision, ordinal);
            assert!(!headers.iter().any(|line| line.starts_with("range:")));
            write!(socket.get_mut(), "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"shard-{ordinal}\"\r\nConnection: close\r\n\r\n", data.len()).unwrap();
            let limit = if ordinal == 1 {
                data.len()
            } else {
                split_offset
            };
            socket.get_mut().write_all(&data[..limit]).unwrap();
        }
    });
    let flag = AtomicBool::new(false);
    assert!(matches!(
        run(&root, &plan, &hub, &flag, &flag, |_| {}).unwrap(),
        Downloaded::Paused {
            interrupted: true,
            ..
        }
    ));
    first_server.join().unwrap();
    assert_eq!(read_paused(&root).unwrap(), Some(plan.clone()));
    assert!(Registry::new(root.clone()).list().unwrap().is_empty());

    let first_stage = root
        .join(FOLDER)
        .join(TRANSFER)
        .join(split::private_name(0, 2));
    let mut corrupt = first.clone();
    corrupt[0] ^= 1;
    std::fs::write(&first_stage, corrupt).unwrap();
    let error = run(&root, &plan, &hub, &flag, &flag, |_| {}).unwrap_err();
    assert!(error.to_string().contains("reviewed SHA-256"));
    assert!(Registry::new(root.clone()).list().unwrap().is_empty());
    std::fs::write(first_stage, &first).unwrap();

    let (server, _) = listener();
    let mut resumed = Hub::new(None, Duration::from_secs(5)).unwrap();
    resumed.origin = Url::parse(&format!("http://{}/", server.local_addr().unwrap())).unwrap();
    let second_bytes = second.clone();
    let revision = plan.revision.clone();
    let resume_server = std::thread::spawn(move || {
        let mut socket = accept(&server);
        let headers = read_request(&mut socket, &revision, 2);
        assert!(
            headers
                .iter()
                .any(|line| line.trim_end() == format!("range: bytes={split_offset}-"))
        );
        assert!(
            headers
                .iter()
                .any(|line| line.trim_end() == "if-range: \"shard-2\"")
        );
        let size = second_bytes.len();
        write!(socket.get_mut(), "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {split_offset}-{}/{size}\r\nETag: \"shard-2\"\r\nConnection: close\r\n\r\n", size - split_offset, size - 1).unwrap();
        socket
            .get_mut()
            .write_all(&second_bytes[split_offset..])
            .unwrap();
    });
    let Downloaded::Installed(model, _) =
        run(&root, &plan, &resumed, &flag, &flag, |_| {}).unwrap()
    else {
        panic!("expected one published split model");
    };
    resume_server.join().unwrap();
    assert_eq!(model.shard_bytes, [first.len() as u64, second.len() as u64]);
    let opened = Registry::new(root.clone()).open(&model.id).unwrap();
    assert_eq!(std::fs::read(&opened.path).unwrap(), first);
    assert_eq!(
        std::fs::read(opened.path.with_file_name(split::private_name(1, 2))).unwrap(),
        second
    );
    assert!(read_paused(&root).unwrap().is_none());
}

fn read_request(
    socket: &mut std::io::BufReader<std::net::TcpStream>,
    revision: &str,
    ordinal: usize,
) -> Vec<String> {
    let mut line = String::new();
    socket.read_line(&mut line).unwrap();
    assert_eq!(
        line.trim_end(),
        format!(
            "GET /fixture/split/resolve/{revision}/fixture-{ordinal:05}-of-00002.gguf HTTP/1.1"
        )
    );
    let mut headers = Vec::new();
    loop {
        line.clear();
        assert_ne!(socket.read_line(&mut line).unwrap(), 0);
        if line == "\r\n" {
            return headers;
        }
        headers.push(line.to_ascii_lowercase());
    }
}
