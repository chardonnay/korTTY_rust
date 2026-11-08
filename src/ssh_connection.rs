use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use kortty_ssh::{Authentication, SshAbort, SshSession, TrustMode, TrustStore, WindowSize};

pub(crate) use kortty::ssh_runtime::{connect_transport, connect_transport_with_options, dial};

use crate::{profile, ssh_worker};

mod channels;
pub(crate) use channels::{Channel, DirectOpener, RemoteForward, RemoteOpener, SftpOpener};

const MAX_PRIVATE_KEY: u64 = 1024 * 1024;

pub(crate) enum Connected {
    Direct(Box<SshSession>),
    Worker(Box<ssh_worker::Session>),
}

impl Connected {
    pub(crate) fn sandbox_backend(&self) -> Option<&'static str> {
        match self {
            Self::Direct(_) => None,
            Self::Worker(worker) => worker.sandbox_backend(),
        }
    }

    pub(crate) fn sftp_opener(&self) -> Option<SftpOpener> {
        match self {
            Self::Direct(ssh) => ssh.sftp_opener().map(SftpOpener::Direct),
            Self::Worker(ssh) => ssh.opener().map(SftpOpener::Worker),
        }
    }

    pub(crate) fn tunnel_openers(&self) -> Option<(DirectOpener, RemoteOpener)> {
        match self {
            Self::Direct(ssh) => Some((
                DirectOpener::Direct(ssh.direct_opener()),
                RemoteOpener::Direct(ssh.remote_opener()),
            )),
            Self::Worker(ssh) => ssh.opener().map(|opener| {
                (
                    DirectOpener::Worker(opener.clone()),
                    RemoteOpener::Worker(opener),
                )
            }),
        }
    }

    pub(crate) fn take_reader(&mut self) -> Option<Box<dyn Read + Send>> {
        match self {
            Self::Direct(ssh) => ssh.take_reader().map(|reader| Box::new(reader) as _),
            Self::Worker(ssh) => ssh.take_reader().map(|reader| Box::new(reader) as _),
        }
    }

    pub(crate) fn take_writer(&mut self) -> Option<Box<dyn io::Write + Send>> {
        match self {
            Self::Direct(ssh) => ssh.take_writer().map(|writer| Box::new(writer) as _),
            Self::Worker(ssh) => ssh.take_writer().map(|writer| Box::new(writer) as _),
        }
    }

    pub(crate) fn abort_handle(&self) -> Abort {
        match self {
            Self::Direct(ssh) => Abort::Direct(ssh.abort_handle()),
            Self::Worker(ssh) => Abort::Worker(ssh.abort_handle()),
        }
    }

    pub(crate) fn resize(&self, size: WindowSize) -> io::Result<()> {
        match self {
            Self::Direct(ssh) => ssh.resize(size),
            Self::Worker(ssh) => ssh.resize(size),
        }
    }

    pub(crate) fn exit_code(&self) -> Option<u32> {
        match self {
            Self::Direct(ssh) => ssh.exit_code(),
            Self::Worker(ssh) => ssh.exit_code(),
        }
    }

    pub(crate) fn close(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(ssh) => ssh.close(),
            Self::Worker(ssh) => ssh.close(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum Abort {
    Direct(SshAbort),
    Worker(ssh_worker::Abort),
    Stream(ssh_worker::StreamAbort),
}

impl Abort {
    pub(crate) fn abort(&self) {
        match self {
            Self::Direct(ssh) => ssh.abort(),
            Self::Worker(ssh) => ssh.abort(),
            Self::Stream(stream) => stream.abort(),
        }
    }

    pub(crate) fn is_closed(&self) -> bool {
        match self {
            Self::Direct(ssh) => ssh.is_closed(),
            Self::Worker(ssh) => ssh.is_closed(),
            Self::Stream(stream) => stream.is_closed(),
        }
    }
}

pub fn trust_store() -> io::Result<TrustStore> {
    Ok(TrustStore::new(
        profile::ssh_known_hosts()?,
        TrustMode::Strict,
    ))
}

pub fn private_key(path: &Path) -> io::Result<Authentication> {
    Ok(Authentication::private_key(read_key(path)?))
}

pub fn private_key_with_passphrase(
    path: &Path,
    passphrase: crate::ssh_dialog::Password,
) -> io::Result<Authentication> {
    let key = read_key(path)?;
    Ok(Authentication::private_key_with_passphrase(
        key,
        passphrase.into_string(),
    ))
}

pub(crate) fn read_key(path: &Path) -> io::Result<Vec<u8>> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH key must be a regular file",
        ));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH key must be a regular file",
        ));
    }
    let mut key = Vec::new();
    file.take(MAX_PRIVATE_KEY + 1).read_to_end(&mut key)?;
    if key.is_empty() || key.len() as u64 > MAX_PRIVATE_KEY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SSH key file is empty or too large",
        ));
    }
    Ok(key)
}

pub(crate) fn read_registered_key(
    record: &crate::ssh_key_catalog::KeyRecord,
) -> io::Result<zeroize::Zeroizing<Vec<u8>>> {
    crate::ssh_key_catalog::Catalog::open_profile()?.read_key(record)
}
