use std::fmt::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{
    CredentialAddition, CredentialInput, CredentialKind, Entry, Error, MAX_SECRET_BYTES, Secret,
    Vault,
};

const VERSION: &str = "kortty-temporary-ssh-key-v1";
pub const MAX_TEMPORARY_SSH_KEY_BYTES: usize = MAX_SECRET_BYTES - 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemporarySshKeyOptions {
    pub minutes: u16,
    /// Allow the saved key to be selected automatically while it is valid.
    pub automatic: bool,
}

pub struct TemporarySshKey<'a> {
    pub private_key: &'a str,
    pub options: TemporarySshKeyOptions,
    pub remaining: Duration,
}

impl CredentialAddition {
    /// Builds an expiring SSH credential for a prepared vault transaction.
    /// The lifetime starts at preparation and remains fixed in the sealed record.
    pub fn temporary_ssh_key(
        label: String,
        endpoint: (&str, u16, &str),
        options: TemporarySshKeyOptions,
        key: Secret,
    ) -> Result<Self, Error> {
        let created = now()?;
        let value = key.trim();
        if !valid_key(value) || duration(options.minutes).is_none() {
            return Err(Error::InvalidInput("invalid temporary SSH key or lifetime"));
        }
        let mut secret = Secret::new(String::with_capacity(value.len() + 64));
        writeln!(
            &mut *secret,
            "{VERSION} {created} {} {}",
            options.minutes,
            u8::from(options.automatic)
        )
        .map_err(|_| Error::Crypto)?;
        secret.push_str(value);
        secret.push('\n');
        let (host, port, account) = endpoint;
        let server_pattern = if host.contains(':') {
            format!("[{host}]:{port}")
        } else {
            format!("{host}:{port}")
        };
        Ok(Self {
            input: CredentialInput {
                label,
                username: account.to_owned(),
                server_pattern,
                kind: CredentialKind::TemporarySshKey,
                ..CredentialInput::default()
            },
            secret,
            webhook: None,
        })
    }
}

impl Vault {
    /// Creates a credential bound to an exact SSH host, port and account.
    /// The key is never renewed by opening the vault or reading the credential.
    pub fn save_temporary_ssh_key(
        &mut self,
        label: String,
        endpoint: (&str, u16, &str),
        options: TemporarySshKeyOptions,
        key: Secret,
    ) -> Result<String, Error> {
        let addition = CredentialAddition::temporary_ssh_key(label, endpoint, options, key)?;
        self.save_entry(addition.input, addition.secret, Default::default())
    }

    /// Borrows an unexpired private key only for the account and destination
    /// that were reviewed when it was saved.
    pub fn temporary_ssh_key(
        &self,
        id: &str,
        endpoint: (&str, u16, &str),
    ) -> Option<TemporarySshKey<'_>> {
        let entry = self.entries.get(id)?;
        let (host, port, account) = endpoint;
        if entry.metadata.kind != CredentialKind::TemporarySshKey
            || entry.metadata.username != account
            || super::sudo_scope(&entry.metadata.server_pattern) != Some((host, port))
        {
            return None;
        }
        open(&entry.secret, now().ok()?)
    }
}

fn open(secret: &str, current: u64) -> Option<TemporarySshKey<'_>> {
    let (created, options, private_key) = parse(secret)?;
    let elapsed = current.checked_sub(created)?;
    let remaining = duration(options.minutes)?.checked_sub(elapsed)?;
    (remaining != 0).then_some(TemporarySshKey {
        private_key,
        options,
        remaining: Duration::from_millis(remaining),
    })
}

pub(super) fn validate(entry: &Entry) -> Result<(), Error> {
    let metadata = &entry.metadata;
    if metadata.username.is_empty()
        || metadata.username.trim() != metadata.username
        || !metadata.environment.is_empty()
        || super::sudo_scope(&metadata.server_pattern).is_none()
        || parse(&entry.secret).is_none_or(|(created, options, key)| {
            created == 0
                || duration(options.minutes).is_none()
                || !valid_key(key.trim_end_matches('\n'))
        })
    {
        return Err(Error::InvalidInput(
            "temporary SSH keys require a valid private key, lifetime, and exact SSH account and endpoint",
        ));
    }
    Ok(())
}

fn parse(secret: &str) -> Option<(u64, TemporarySshKeyOptions, &str)> {
    let (header, key) = secret.split_once('\n')?;
    let mut words = header.split(' ');
    if words.next()? != VERSION {
        return None;
    }
    let created = words.next()?.parse().ok()?;
    let minutes = words.next()?.parse().ok()?;
    let automatic = match words.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    words
        .next()
        .is_none()
        .then_some((created, TemporarySshKeyOptions { minutes, automatic }, key))
}

fn valid_key(key: &str) -> bool {
    if key.len() > MAX_TEMPORARY_SSH_KEY_BYTES
        || !key.is_ascii()
        || key
            .bytes()
            .any(|byte| byte.is_ascii_control() && !matches!(byte, b'\r' | b'\n'))
    {
        return false;
    }
    let mut lines = key.lines();
    let end = match lines.next() {
        Some("-----BEGIN OPENSSH PRIVATE KEY-----") => "-----END OPENSSH PRIVATE KEY-----",
        Some("-----BEGIN PRIVATE KEY-----") => "-----END PRIVATE KEY-----",
        Some("-----BEGIN RSA PRIVATE KEY-----") => "-----END RSA PRIVATE KEY-----",
        Some("-----BEGIN EC PRIVATE KEY-----") => "-----END EC PRIVATE KEY-----",
        _ => return false,
    };
    lines.next_back() == Some(end) && lines.any(|line| !line.is_empty())
}

fn duration(minutes: u16) -> Option<u64> {
    (1..=1440)
        .contains(&minutes)
        .then(|| u64::from(minutes) * 60_000)
}

fn now() -> Result<u64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .ok_or(Error::InvalidInput(
            "system time is outside the temporary key range",
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_selection_cannot_extend_the_expiry_or_use_a_future_key() {
        let record = format!("{VERSION} 1000 1 1\nfixture");
        assert!(open(&record, 999).is_none());
        let key = open(&record, 60_999).unwrap();
        assert!(key.options.automatic);
        assert_eq!(key.remaining, Duration::from_millis(1));
        assert!(open(&record, 61_000).is_none());
    }
}
