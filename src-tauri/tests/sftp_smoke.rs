//! Live SSH/SFTP smoke test against a real server.
//!
//! Ignored by default — requires a reachable SSH server. Run with:
//! ```sh
//! KORTTY_SMOKE_HOST=10.0.0.1 KORTTY_SMOKE_USER=user KORTTY_SMOKE_PASSWORD=secret \
//!   cargo test --test sftp_smoke -- --ignored --nocapture
//! ```

use kortty_lib::model::connection::{AuthMethod, ConnectionSettings};
use kortty_lib::sftp::manager;
use kortty_lib::ssh::session::SSHSession;
use tokio::sync::mpsc;

fn smoke_settings() -> Option<ConnectionSettings> {
    let host = std::env::var("KORTTY_SMOKE_HOST").ok()?;
    let username = std::env::var("KORTTY_SMOKE_USER").ok()?;
    let password = std::env::var("KORTTY_SMOKE_PASSWORD").ok()?;
    Some(ConnectionSettings {
        id: "smoke-test".to_string(),
        name: "smoke-test".to_string(),
        host,
        username,
        auth_method: AuthMethod::Password,
        password: Some(password),
        ..ConnectionSettings::default()
    })
}

#[tokio::test]
#[ignore = "requires a live SSH server (KORTTY_SMOKE_HOST/USER/PASSWORD)"]
async fn ssh_and_sftp_roundtrip_against_live_server() {
    let settings = smoke_settings()
        .expect("KORTTY_SMOKE_HOST, KORTTY_SMOKE_USER and KORTTY_SMOKE_PASSWORD must be set");

    let mut session = SSHSession::new(settings);
    let (output_tx, mut output_rx) = mpsc::unbounded_channel();
    session
        .connect(output_tx)
        .await
        .expect("SSH connect failed");
    println!("[1/9] SSH connection + password auth: OK");

    // Exec channel must keep working alongside SFTP (terminal command path).
    let whoami = session
        .exec_command("whoami")
        .await
        .expect("exec_command failed");
    println!("[2/9] exec_command(whoami): {:?}", whoami.trim());

    let sftp = session.open_sftp().await.expect("open_sftp failed");
    println!("[3/9] SFTP subsystem negotiated: OK");

    // Home listing.
    let home_entries = manager::list_dir(&sftp, ".")
        .await
        .expect("list_dir failed");
    println!(
        "[4/9] list_dir(home): {} entries, first: {:?}",
        home_entries.len(),
        home_entries
            .first()
            .map(|e| (&e.name, &e.permissions, &e.owner))
    );

    let base = format!("kortty-smoke-{}", std::process::id());
    let nested = format!("{base}/nested/dir");
    manager::mkdir(&sftp, &nested)
        .await
        .expect("mkdir -p failed");
    println!("[5/9] mkdir -p {nested}: OK");

    // Text file roundtrip (UTF-8 incl. umlauts).
    let text_path = format!("{base}/grüße.txt");
    let content = "Hällo SFTP — Zeile 1\nZeile 2\n";
    manager::write_text_file(&sftp, &text_path, content)
        .await
        .expect("write_text_file failed");
    let read_back = manager::read_text_file(&sftp, &text_path)
        .await
        .expect("read_text_file failed");
    assert_eq!(read_back, content, "text roundtrip mismatch");
    println!("[6/9] write/read text file (UTF-8): OK");

    // Binary upload/download roundtrip.
    let local_dir = std::env::temp_dir().join(&base);
    std::fs::create_dir_all(&local_dir).expect("local temp dir");
    let local_src = local_dir.join("payload.bin");
    let payload: Vec<u8> = (0..=255u8).cycle().take(256 * 1024 + 13).collect();
    std::fs::write(&local_src, &payload).expect("write local payload");
    let remote_bin = format!("{base}/payload.bin");
    manager::upload(&sftp, local_src.to_str().unwrap(), &remote_bin)
        .await
        .expect("upload failed");
    let local_dst = local_dir.join("payload-down.bin");
    manager::download(&sftp, &remote_bin, local_dst.to_str().unwrap())
        .await
        .expect("download failed");
    let downloaded = std::fs::read(&local_dst).expect("read downloaded payload");
    assert_eq!(downloaded, payload, "binary roundtrip mismatch");
    println!("[7/9] upload/download binary roundtrip (256 KiB): OK");

    // Rename + chmod, verify via listing.
    let renamed = format!("{base}/payload-renamed.bin");
    manager::rename(&sftp, &remote_bin, &renamed)
        .await
        .expect("rename failed");
    manager::chmod(&sftp, &renamed, 0o600)
        .await
        .expect("chmod failed");
    let entries = manager::list_dir(&sftp, &base)
        .await
        .expect("list base failed");
    let entry = entries
        .iter()
        .find(|e| e.name == "payload-renamed.bin")
        .expect("renamed file not in listing");
    assert!(
        entry
            .permissions
            .as_deref()
            .unwrap_or("")
            .contains("rw-------"),
        "chmod 600 not reflected in listing: {:?}",
        entry.permissions
    );
    println!(
        "[8/9] rename + chmod 600: OK (permissions: {:?}, owner: {:?})",
        entry.permissions, entry.owner
    );

    // Recursive delete of the whole tree.
    manager::delete(&sftp, &base)
        .await
        .expect("recursive delete failed");
    let after = manager::list_dir(&sftp, ".")
        .await
        .expect("list after delete");
    assert!(
        !after.iter().any(|e| e.name == base),
        "smoke dir still present after recursive delete"
    );
    println!("[9/9] recursive delete: OK");

    std::fs::remove_dir_all(&local_dir).ok();
    session.disconnect().await.ok();

    // Regression guard for the sink registration: SFTP channel data must never
    // reach the terminal output channel. If it leaked, the uploaded file content
    // would show up in the terminal stream.
    let mut terminal_output = Vec::new();
    while let Ok(chunk) = output_rx.try_recv() {
        terminal_output.extend_from_slice(&chunk);
    }
    let terminal_text = String::from_utf8_lossy(&terminal_output);
    assert!(
        !terminal_text.contains("Hällo SFTP"),
        "SFTP file content leaked into terminal output"
    );
    println!(
        "[10/10] no SFTP data leaked into terminal output ({} bytes of legit shell output): OK",
        terminal_output.len()
    );
    println!("SMOKE TEST PASSED");
}
