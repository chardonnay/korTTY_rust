use serde::{Deserialize, Serialize};

/// Frontend error code for an SFTP subsystem that was rejected or closed by the server.
pub const SFTP_SUBSYSTEM_REJECTED_CODE: &str = "SFTP_SUBSYSTEM_REJECTED";
/// Frontend error code for an SFTP subsystem that could not be started for other reasons.
pub const SFTP_SUBSYSTEM_START_FAILED_CODE: &str = "SFTP_SUBSYSTEM_START_FAILED";

/// Message fragments that indicate the server rejected or closed the SFTP subsystem
/// before an SFTP version was negotiated (mirrors Java SFTPSession.isSftpSubsystemNegotiationFailure).
const SUBSYSTEM_REJECTION_MARKERS: [&str; 5] = [
    "channel closing",
    "closed before version negotiated",
    "eof",
    "subsystem request failed",
    "channel open failure",
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SftpStartupErrorKind {
    SubsystemRejected,
    #[default]
    StartFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SftpStartupError {
    #[serde(default)]
    pub kind: SftpStartupErrorKind,
    #[serde(default)]
    pub technical_cause: String,
}

impl SftpStartupError {
    pub fn new(kind: SftpStartupErrorKind, technical_cause: impl Into<String>) -> Self {
        Self {
            kind,
            technical_cause: normalize_technical_cause(technical_cause.into()),
        }
    }

    /// Builds an error whose kind is derived from the technical cause message.
    pub fn classified(technical_cause: impl Into<String>) -> Self {
        let technical_cause = technical_cause.into();
        Self::new(
            classify_sftp_startup_error(&technical_cause),
            technical_cause,
        )
    }

    /// Builds a generic startup failure that bypasses rejection classification.
    pub fn start_failed(technical_cause: impl Into<String>) -> Self {
        Self::new(SftpStartupErrorKind::StartFailed, technical_cause)
    }

    pub fn error_code(&self) -> &'static str {
        match self.kind {
            SftpStartupErrorKind::SubsystemRejected => SFTP_SUBSYSTEM_REJECTED_CODE,
            SftpStartupErrorKind::StartFailed => SFTP_SUBSYSTEM_START_FAILED_CODE,
        }
    }
}

impl std::fmt::Display for SftpStartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            SftpStartupErrorKind::SubsystemRejected => write!(
                f,
                "SFTP subsystem was rejected or closed by the server after successful SSH \
authentication; no SFTP version was negotiated. Check whether SFTP is enabled for this \
target or the SSH proxy. Technical cause: {}",
                self.technical_cause
            ),
            SftpStartupErrorKind::StartFailed => write!(
                f,
                "SFTP subsystem could not be started after successful SSH authentication: {}",
                self.technical_cause
            ),
        }
    }
}

impl std::error::Error for SftpStartupError {}

/// Classifies an SFTP startup failure message: rejection markers indicate the server
/// (or an SSH proxy) refused or closed the subsystem after successful authentication.
pub fn classify_sftp_startup_error(message: &str) -> SftpStartupErrorKind {
    let normalized = message.to_lowercase();
    if SUBSYSTEM_REJECTION_MARKERS
        .iter()
        .any(|marker| normalized.contains(marker))
    {
        SftpStartupErrorKind::SubsystemRejected
    } else {
        SftpStartupErrorKind::StartFailed
    }
}

fn normalize_technical_cause(cause: String) -> String {
    let trimmed = cause.trim();
    if trimmed.is_empty() {
        "unknown error".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_channel_closing_as_subsystem_rejection() {
        let message = "IoWriteFutureImpl[SftpChannelSubsystem][SSH_MSG_CHANNEL_DATA]: \
Failed (EOFException) to execute: Channel closing";

        assert_eq!(
            classify_sftp_startup_error(message),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn detects_subsystem_request_failure_as_rejection() {
        assert_eq!(
            classify_sftp_startup_error("subsystem request failed on channel 0"),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn detects_version_negotiation_abort_as_rejection() {
        assert_eq!(
            classify_sftp_startup_error("Channel closed before version negotiated"),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn detects_eof_as_rejection() {
        assert_eq!(
            classify_sftp_startup_error("unexpected EOF while reading packet"),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn detects_channel_open_failure_as_rejection() {
        assert_eq!(
            classify_sftp_startup_error("channel open failure: AdministrativelyProhibited"),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(
            classify_sftp_startup_error("CHANNEL CLOSING"),
            SftpStartupErrorKind::SubsystemRejected
        );
    }

    #[test]
    fn generic_failure_keeps_start_failed_kind() {
        assert_eq!(
            classify_sftp_startup_error("permission denied"),
            SftpStartupErrorKind::StartFailed
        );
    }

    #[test]
    fn classified_error_exposes_rejection_code_and_message() {
        let error = SftpStartupError::classified("Channel closing");

        assert_eq!(error.kind, SftpStartupErrorKind::SubsystemRejected);
        assert_eq!(error.error_code(), SFTP_SUBSYSTEM_REJECTED_CODE);
        assert!(error.to_string().contains("no SFTP version was negotiated"));
        assert!(error.to_string().contains("Channel closing"));
    }

    #[test]
    fn generic_start_failure_keeps_cause_message() {
        let error = SftpStartupError::classified("permission denied");

        assert_eq!(error.kind, SftpStartupErrorKind::StartFailed);
        assert_eq!(error.error_code(), SFTP_SUBSYSTEM_START_FAILED_CODE);
        assert_eq!(
            error.to_string(),
            "SFTP subsystem could not be started after successful SSH authentication: \
permission denied"
        );
    }

    #[test]
    fn start_failed_bypasses_rejection_classification() {
        let error = SftpStartupError::start_failed("eof while connecting");

        assert_eq!(error.kind, SftpStartupErrorKind::StartFailed);
    }

    #[test]
    fn blank_technical_cause_falls_back_to_unknown_error() {
        let error = SftpStartupError::classified("   ");

        assert_eq!(error.technical_cause, "unknown error");
    }

    #[test]
    fn serializes_kind_as_camel_case() {
        let error = SftpStartupError::classified("Channel closing");
        let json = serde_json::to_string(&error).expect("error should serialize");

        assert!(json.contains("\"kind\":\"subsystemRejected\""));
        assert!(json.contains("\"technicalCause\":\"Channel closing\""));
    }
}
