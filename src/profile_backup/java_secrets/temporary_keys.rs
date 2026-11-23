//! Reads Java's encrypted temporary credentials into the reviewed restore transaction.

use std::collections::BTreeSet;
use std::io;

use kortty_vault::{CredentialAddition, TemporarySshKeyOptions};
use roxmltree::Node;

use super::{JavaSecrets, children, optional_child, parse_xml, raw_scalar, scalar};
use crate::ssh_dialog::LoginTarget;

const MAX_CONNECTION_XML_BYTES: usize = 1024 * 1024;
const MAX_CANDIDATES: usize = 512;
const DEFAULT_MINUTES: u16 = 15;

pub(in crate::profile_backup) struct Imported {
    pub(in crate::profile_backup) source_id: String,
    pub(in crate::profile_backup) login: LoginTarget,
    pub(in crate::profile_backup) jump: Option<LoginTarget>,
    pub(in crate::profile_backup) options: TemporarySshKeyOptions,
    pub(in crate::profile_backup) addition: CredentialAddition,
}

#[derive(Default)]
pub(in crate::profile_backup) struct Review {
    pub(in crate::profile_backup) keys: Vec<Imported>,
    pub(in crate::profile_backup) omitted: usize,
}

pub(in crate::profile_backup) fn inspect(
    secrets: &JavaSecrets<'_>,
    source: &[u8],
) -> io::Result<Review> {
    let document = parse_xml(source, MAX_CONNECTION_XML_BYTES, "connections")?;
    let root = document.root_element();
    if root
        .attribute("schemaVersion")
        .is_some_and(|version| !matches!(version, "1" | "2"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unsupported Java connections.xml schema version.",
        ));
    }
    let mut review = Review::default();
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for (index, connection) in children(root, "connection").enumerate() {
        if index >= MAX_CANDIDATES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Java connections.xml exceeds its entry limit.",
            ));
        }
        if let Some(id) = connection.attribute("id")
            && !seen.insert(id)
        {
            duplicates.insert(id);
        }
    }
    for connection in children(root, "connection") {
        if children(connection, "temporaryKeyContent").next().is_none() {
            continue;
        }
        let unique_id = connection
            .attribute("id")
            .filter(|id| super::passwords::valid_source_id(id) && !duplicates.contains(id));
        match unique_id.and_then(|id| read_key(secrets, connection, id).ok().flatten()) {
            Some(key) => review.keys.push(key),
            None if scalar(connection, "temporaryKeyContent")
                .is_ok_and(|value| value.is_none_or(str::is_empty)) => {}
            None => review.omitted += 1,
        }
    }
    Ok(review)
}

fn read_key(
    secrets: &JavaSecrets<'_>,
    connection: Node<'_, '_>,
    source_id: &str,
) -> Result<Option<Imported>, ()> {
    let Some(encrypted) = scalar(connection, "temporaryKeyContent")?.filter(|key| !key.is_empty())
    else {
        return Ok(None);
    };
    if !matches!(
        scalar(connection, "connectionSource")?,
        None | Some("LOCAL")
    ) || !matches!(scalar(connection, "protocol")?, None | Some("SSH_TCP"))
        || scalar(connection, "authMethod")? != Some("PUBLIC_KEY")
        || scalar(connection, "privateKeyPath")?
            .is_some_and(|path| path != "TEMPORARY:__ENCRYPTED__")
        || scalar(connection, "sshKeyId")?.is_some_and(|id| !id.is_empty())
        || scalar(connection, "teamworkSourceId")?.is_some_and(|id| !id.is_empty())
    {
        return Err(());
    }
    let (_, login) = super::passwords::endpoint(connection)?;
    let jump = match optional_child(connection, "jumpServer")? {
        Some(jump) => match scalar(jump, "enabled")? {
            Some("true" | "1") => {
                if scalar(jump, "temporaryKeyContent")?.is_some_and(|key| !key.is_empty())
                    || scalar(jump, "privateKeyPath")?
                        .is_some_and(|path| path.starts_with("TEMPORARY:"))
                {
                    return Err(());
                }
                Some(super::passwords::endpoint(jump)?.1)
            }
            None | Some("false" | "0" | "") => None,
            _ => return Err(()),
        },
        None => None,
    };
    let minutes = match scalar(connection, "temporaryKeyExpirationMinutes")? {
        None | Some("") => DEFAULT_MINUTES,
        Some(value) if value.bytes().all(|byte| byte.is_ascii_digit()) => value
            .parse::<u16>()
            .ok()
            .filter(|minutes| (1..=1440).contains(minutes))
            .ok_or(())?,
        _ => return Err(()),
    };
    let automatic = match scalar(connection, "temporaryKeyPermanent")? {
        Some("true" | "1") => true,
        None | Some("false" | "0" | "") => false,
        _ => return Err(()),
    };
    let key = secrets.decrypt_temporary_key(encrypted).map_err(|_| ())?;
    kortty_ssh::validate_private_key(key.trim().as_bytes(), None).map_err(|_| ())?;
    let label = raw_scalar(connection, "name")?
        .filter(|name| {
            !name.trim().is_empty() && name.len() <= 160 && !crate::ssh_dialog::unsafe_text(name)
        })
        .unwrap_or("Imported temporary SSH key")
        .to_owned();
    let options = TemporarySshKeyOptions { minutes, automatic };
    let addition = CredentialAddition::temporary_ssh_key(
        label,
        (&login.host, login.port, &login.username),
        options,
        key,
    )
    .map_err(|_| ())?;
    Ok(Some(Imported {
        source_id: source_id.to_owned(),
        login,
        jump,
        options,
        addition,
    }))
}
