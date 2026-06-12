pub mod diagnostics;
pub mod exec_fallback;
pub mod manager;
pub mod paths;

use diagnostics::SftpStartupError;
use serde::Serialize;
use std::collections::HashMap;
use tokio::sync::Mutex;

/// Maximum number of bytes for files opened through the snippet-editor text commands.
pub const MAX_TEXT_EDITOR_BYTES: u64 = 5 * 1024 * 1024;

/// Frontend error code prefix for remote text files that are not valid UTF-8.
pub const BINARY_OR_NON_TEXT_CODE: &str = "BINARY_OR_NON_TEXT";

/// Transfer backend used for the remote file operations of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SftpTransferMode {
    /// Native SFTP subsystem via russh-sftp.
    Sftp,
    /// Exec + base64 command-line fallback (SFTP subsystem unavailable).
    ExecFallback,
}

/// Diagnostic reason why a session fell back to the exec transport.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpModeReason {
    pub code: String,
    pub message: String,
}

impl SftpModeReason {
    pub fn from_startup_error(error: &SftpStartupError) -> Self {
        Self {
            code: error.error_code().to_string(),
            message: error.to_string(),
        }
    }
}

/// Payload of the "kortty-sftp-mode" Tauri event and of the sftp_transfer_mode command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpModeStatus {
    pub session_id: String,
    pub mode: SftpTransferMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<SftpModeReason>,
}

/// Tracks the last reported transfer mode per session so the "kortty-sftp-mode"
/// event is emitted once when the mode is first determined (and again only when
/// it changes, e.g. after a reconnect made the SFTP subsystem available).
///
/// The mode itself is not authoritative here: each operation resolves it through
/// `SSHSession::open_sftp()`, whose outcome is cached inside the session state and
/// reset on reconnect, so a failed subsystem start is never retried per operation.
#[derive(Default)]
pub struct SftpModeStore(pub Mutex<HashMap<String, SftpModeStatus>>);

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics::SftpStartupErrorKind;

    #[test]
    fn transfer_mode_serializes_as_camel_case() {
        assert_eq!(
            serde_json::to_string(&SftpTransferMode::Sftp).unwrap(),
            "\"sftp\""
        );
        assert_eq!(
            serde_json::to_string(&SftpTransferMode::ExecFallback).unwrap(),
            "\"execFallback\""
        );
    }

    #[test]
    fn mode_status_omits_missing_reason_and_uses_camel_case_keys() {
        let status = SftpModeStatus {
            session_id: "abc".into(),
            mode: SftpTransferMode::Sftp,
            reason: None,
        };
        let json = serde_json::to_string(&status).unwrap();

        assert!(json.contains("\"sessionId\":\"abc\""));
        assert!(json.contains("\"mode\":\"sftp\""));
        assert!(!json.contains("reason"));
    }

    #[test]
    fn mode_reason_carries_error_code_and_message() {
        let error =
            SftpStartupError::new(SftpStartupErrorKind::SubsystemRejected, "Channel closing");
        let reason = SftpModeReason::from_startup_error(&error);

        assert_eq!(reason.code, "SFTP_SUBSYSTEM_REJECTED");
        assert!(reason.message.contains("Channel closing"));
    }
}
