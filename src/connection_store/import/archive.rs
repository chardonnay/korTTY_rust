//! Single-member portable connection archives. Nothing is extracted to disk.

use std::io::{self, Cursor, Read as _};
use std::path::Path;

use kortty_vault::Secret;
use pgp::packet::PacketHeader;
use pgp::types::Tag;
use zeroize::Zeroizing;
use zip::{CompressionMethod, ZipArchive};

use super::{Access, Format, MAX_BYTES, Preview, ReadOutcome, invalid};
use crate::snippets::archive::ZipSettings;

const MAX_ZIP_BYTES: usize = 2 * 1024 * 1024;
const MAX_OPENPGP_BYTES: u64 = 3 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) enum Kind {
    Zip,
    OpenPgp,
}

impl Kind {
    pub(super) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?;
        if extension.eq_ignore_ascii_case("zip") {
            Some(Self::Zip)
        } else if ["gpg", "pgp", "asc"]
            .iter()
            .any(|kind| extension.eq_ignore_ascii_case(kind))
        {
            Some(Self::OpenPgp)
        } else {
            None
        }
    }

    pub(super) fn size_limit(self) -> u64 {
        match self {
            Self::Zip => MAX_ZIP_BYTES as u64,
            Self::OpenPgp => MAX_OPENPGP_BYTES,
        }
    }
}

pub(super) fn read(bytes: &[u8], kind: Kind, access: Access<'_>) -> io::Result<ReadOutcome> {
    match kind {
        Kind::Zip => {
            let password = match access {
                Access::ZipPassword(password) => Some(password),
                _ => None,
            };
            read_zip(bytes, password, true)
        }
        Kind::OpenPgp => {
            if !bytes.starts_with(b"-----BEGIN PGP MESSAGE-----")
                && !PacketHeader::try_from_reader(bytes)
                    .is_ok_and(|header| header.tag() == Tag::PublicKeyEncryptedSessionKey)
            {
                return Err(invalid(
                    "expected a recipient-encrypted OpenPGP connection archive",
                ));
            }
            let Access::OpenPgp {
                private_key,
                passphrase,
            } = access
            else {
                return Ok(ReadOutcome::OpenPgpKeyRequired);
            };
            if passphrase.len() > ZipSettings::MAX_PASSWORD_BYTES {
                return Err(invalid(
                    "the OpenPGP private-key passphrase exceeds 1024 UTF-8 bytes",
                ));
            }
            let zip = crate::profile_backup::decrypt_openpgp_document(
                bytes,
                private_key,
                passphrase,
                MAX_ZIP_BYTES,
            )?;
            read_zip(&zip, None, false)
        }
    }
}

fn read_zip(
    bytes: &[u8],
    password: Option<&Secret>,
    allow_password: bool,
) -> io::Result<ReadOutcome> {
    if bytes.len() > MAX_ZIP_BYTES || !bytes.starts_with(b"PK\x03\x04") {
        return Err(invalid("expected a connection ZIP within its size limit"));
    }
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    if zip.len() != 1 || zip.offset() != 0 {
        return Err(invalid(
            "the ZIP must contain exactly one connection file at its root",
        ));
    }
    zip.validate_header_consistency()?;
    let member = zip.by_index_data(0)?;
    let name = std::str::from_utf8(member.name_raw())
        .map_err(|_| invalid("the ZIP member name must use UTF-8"))?;
    if !member.is_file()
        || member
            .unix_mode()
            .is_some_and(|mode| !matches!(mode & 0o170000, 0 | 0o100000))
        || name.is_empty()
        || name.len() > 255
        || name != name.trim()
        || name.contains(['/', '\\', ':'])
        || crate::ssh_dialog::unsafe_text(name)
    {
        return Err(invalid(
            "the ZIP member must be a regular file with a single safe name",
        ));
    }
    let format = Format::from_path(Path::new(name)).ok_or_else(|| {
        invalid("the ZIP must contain a connection XML, CSV or MobaXterm sessions file")
    })?;
    let size = member.size();
    let encrypted = member.encrypted();
    if password.is_some() && !encrypted {
        return Err(invalid(
            "the selected ZIP no longer needs a password. Preview it again",
        ));
    }
    if size == 0 || size > MAX_BYTES || member.compressed_size() > MAX_ZIP_BYTES as u64 {
        return Err(invalid(
            "the connection ZIP member exceeds its size limit or is empty",
        ));
    }
    if !matches!(
        member.compression(),
        CompressionMethod::Stored | CompressionMethod::Deflated
    ) {
        return Err(invalid(
            "the connection ZIP must use stored or Deflate compression",
        ));
    }

    let mut member = if encrypted {
        if !allow_password {
            return Err(invalid(
                "the ZIP inside the OpenPGP message must have no second password",
            ));
        }
        if zip.get_aes_verification_key_and_salt(0)?.is_none() {
            return Err(invalid(
                "only authenticated AES-encrypted connection ZIP files are supported",
            ));
        }
        let Some(password) = password else {
            return Ok(ReadOutcome::ZipPasswordRequired);
        };
        if !(1..=ZipSettings::MAX_PASSWORD_BYTES).contains(&password.len()) {
            return Err(invalid(
                "a connection ZIP password must contain 1 to 1024 UTF-8 bytes",
            ));
        }
        zip.by_index_decrypt(0, password.as_bytes())
            .map_err(|_| invalid("could not unlock or authenticate the connection ZIP"))?
    } else {
        zip.by_index(0)?
    };
    let mut content = Zeroizing::new(Vec::with_capacity(size as usize));
    // Continue to crypto/CRC EOF before parsing any decrypted content.
    (&mut member)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut content)
        .map_err(|_| invalid("could not read or authenticate the connection ZIP member"))?;
    if content.len() as u64 != size {
        return Err(invalid(
            "the connection ZIP member does not match its declared size",
        ));
    }
    Preview::parse_bytes(format, &content).map(ReadOutcome::Preview)
}
