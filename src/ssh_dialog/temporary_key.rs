use std::fmt;
use std::io;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use zeroize::{Zeroize as _, Zeroizing};

use super::Password;

const MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub struct TemporaryKey(Arc<Material>);

struct Material {
    key: Mutex<Zeroizing<Vec<u8>>>,
    deadline: Instant,
    minutes: u16,
    _cancel_expiry: mpsc::Sender<()>,
}

impl TemporaryKey {
    fn new(pasted: Password, minutes: u16) -> io::Result<Self> {
        let text = pasted.as_str().trim();
        if pasted.as_str().len() >= MAX_BYTES
            || !text.is_ascii()
            || text
                .bytes()
                .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n'))
            || !looks_like_private_key(text)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid temporary SSH private key.",
            ));
        }
        let mut key = Zeroizing::new(Vec::with_capacity(text.len() + 1));
        key.extend_from_slice(text.as_bytes());
        key.push(b'\n');
        let deadline = Instant::now() + Duration::from_secs(u64::from(minutes) * 60);
        Self::hold(key, minutes, deadline)
    }

    fn hold(key: Zeroizing<Vec<u8>>, minutes: u16, deadline: Instant) -> io::Result<Self> {
        let (cancel, cancelled) = mpsc::channel();
        let material = Arc::new(Material {
            key: Mutex::new(key),
            deadline,
            minutes,
            _cancel_expiry: cancel,
        });
        let expires = Arc::downgrade(&material);
        std::thread::Builder::new()
            .name("kortty-temporary-ssh-key".into())
            .spawn(move || {
                if cancelled.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    == Err(mpsc::RecvTimeoutError::Timeout)
                    && let Some(material) = expires.upgrade()
                {
                    material
                        .key
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .zeroize();
                }
            })?;
        Ok(Self(material))
    }

    pub(crate) fn copy_for_attempt(&self) -> io::Result<Zeroizing<Vec<u8>>> {
        let key = self.0.key.lock().unwrap_or_else(|error| error.into_inner());
        if !self.is_current() || key.is_empty() {
            return Err(io::Error::other(
                "The temporary SSH key expired. Paste the key again.",
            ));
        }
        Ok(Zeroizing::new(key.to_vec()))
    }

    fn is_current(&self) -> bool {
        Instant::now() < self.0.deadline
    }
}

impl fmt::Debug for TemporaryKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TemporaryKey([redacted])")
    }
}

fn looks_like_private_key(text: &str) -> bool {
    let mut lines = text.lines();
    let end = match lines.next() {
        Some("-----BEGIN OPENSSH PRIVATE KEY-----") => "-----END OPENSSH PRIVATE KEY-----",
        Some("-----BEGIN PRIVATE KEY-----") => "-----END PRIVATE KEY-----",
        Some("-----BEGIN EC PRIVATE KEY-----") => "-----END EC PRIVATE KEY-----",
        _ => return false,
    };
    lines.next_back() == Some(end) && lines.any(|line| !line.is_empty())
}

pub(super) struct Form {
    pub(super) minutes: String,
    key: Option<TemporaryKey>,
    pending_paste: Option<u64>,
    failed_paste: bool,
}

impl Default for Form {
    fn default() -> Self {
        Self {
            minutes: "60".into(),
            key: None,
            pending_paste: None,
            failed_paste: false,
        }
    }
}

impl Form {
    pub(super) fn set_minutes(&mut self, minutes: String) {
        if minutes.len() <= 4
            && minutes.bytes().all(|byte| byte.is_ascii_digit())
            && self.minutes != minutes
        {
            self.minutes = minutes;
            self.clear();
        }
    }

    pub(super) fn begin_paste(&mut self, request: u64) {
        self.clear();
        self.pending_paste = Some(request);
    }

    pub(super) fn expects_paste(&self, request: u64) -> bool {
        self.pending_paste == Some(request)
    }

    pub(super) fn finish_paste(&mut self, request: u64, pasted: Password) {
        if !self.expects_paste(request) {
            return;
        }
        self.pending_paste = None;
        self.key = self
            .minutes
            .parse()
            .ok()
            .filter(|minutes| (1..=1440).contains(minutes))
            .and_then(|minutes| TemporaryKey::new(pasted, minutes).ok());
        self.failed_paste = self.key.is_none();
    }

    pub(super) fn restore(&mut self, key: TemporaryKey) {
        self.clear();
        self.minutes = key.0.minutes.to_string();
        self.key = Some(key);
    }

    pub(super) fn key(&self) -> Option<&TemporaryKey> {
        self.key.as_ref().filter(|key| key.is_current())
    }

    pub(super) fn take(&mut self) -> Option<TemporaryKey> {
        self.pending_paste = None;
        self.key.take()
    }

    pub(super) fn clear(&mut self) {
        self.key = None;
        self.pending_paste = None;
        self.failed_paste = false;
    }

    pub(super) fn status(&self) -> &'static str {
        if self.pending_paste.is_some() {
            "Reading clipboard…"
        } else if self.failed_paste {
            "Paste a complete SSH private key and use a lifetime from 1 to 1440 minutes."
        } else if self.key().is_some() {
            "Private key ready. Changing the lifetime clears the key."
        } else if self.key.is_some() {
            "The temporary SSH key expired. Paste the key again."
        } else {
            "Set the lifetime, then paste an unencrypted private key. The key is kept in memory."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_expired_temporary_key_cannot_be_copied_and_is_wiped_with_live_references() {
        let key = TemporaryKey::hold(Zeroizing::new(vec![12; 64]), 1, Instant::now()).unwrap();
        let retained = key.clone();
        assert!(retained.copy_for_attempt().is_err());
        let wait = Instant::now() + Duration::from_secs(2);
        while !key.0.key.lock().unwrap().is_empty() {
            assert!(Instant::now() < wait, "expired key was not wiped");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(retained.copy_for_attempt().is_err());
    }
}
