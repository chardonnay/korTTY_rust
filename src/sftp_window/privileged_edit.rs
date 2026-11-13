//! Binds an elevated file operation to its original authenticated route.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kortty_ssh::SensitiveLine;

use crate::clipboard_policy::ClipboardPolicy;
use crate::ssh_connection::ExecOpener;
use crate::vault_window::AgentSudoCredential;
use crate::workspace::SshTarget;

use super::{RemoteEditor, SftpWindow};

pub(crate) mod protocol;

#[derive(Clone)]
pub(super) struct Route {
    target: SshTarget,
    opener: ExecOpener,
    sftp: RemoteEditor,
}

impl Route {
    pub(super) fn label(&self) -> String {
        let key = self.opener.verified_host_key().map(|key| key.fingerprint());
        format!(
            "{} · {}",
            super::files::display(&self.target.label()),
            key.as_deref().unwrap_or_default()
        )
    }

    pub(super) fn is_closed(&self) -> bool {
        self.sftp.is_closed() || self.opener.is_closed()
    }

    pub(super) fn allowed_now(&self) -> bool {
        !self.is_closed() && self.target.sftp_sudo_allowed(&ClipboardPolicy::load())
    }

    pub(super) fn matches(&self, window: &SftpWindow) -> bool {
        self.target == *window.target()
            && window.owns_editor(&self.sftp)
            && !self.is_closed()
            && window
                .worker
                .as_ref()
                .and_then(super::worker::Worker::exec_opener)
                .is_some_and(|current| self.opener.same_connection(&current))
    }

    pub(super) fn read(
        &self,
        path: &str,
        access: Access,
        permit: Arc<AtomicBool>,
    ) -> io::Result<protocol::Document> {
        let allowed = self.guard(permit, access.credential);
        protocol::read(self.opener.clone(), path, access.auth, allowed).map_err(io::Error::other)
    }

    pub(super) fn write(
        &self,
        document: &protocol::Document,
        text: Arc<str>,
        access: Access,
        permit: Arc<AtomicBool>,
    ) -> io::Result<protocol::Document> {
        let allowed = self.guard(permit, access.credential);
        protocol::write(self.opener.clone(), document, text, access.auth, allowed)
            .map_err(io::Error::other)
    }

    fn guard(
        &self,
        permit: Arc<AtomicBool>,
        credential: Option<AgentSudoCredential>,
    ) -> impl Fn() -> bool + Clone + Send + Sync + 'static {
        let route = self.clone();
        move || {
            permit.load(Ordering::Acquire)
                && credential.as_ref().is_none_or(AgentSudoCredential::active)
                && route.allowed_now()
        }
    }
}

pub(super) struct Access {
    auth: protocol::Auth,
    credential: Option<AgentSudoCredential>,
}

impl Access {
    pub(super) fn passwordless() -> Self {
        Self {
            auth: protocol::Auth::Passwordless,
            credential: None,
        }
    }

    pub(super) fn password(line: SensitiveLine, credential: Option<AgentSudoCredential>) -> Self {
        Self {
            auth: protocol::Auth::Password(line),
            credential,
        }
    }
}

impl SftpWindow {
    pub(super) fn privileged_editor_route(&self) -> Option<Route> {
        if self.incognito() {
            return None;
        }
        let worker = self.worker.as_ref()?;
        let sftp = worker.editor().filter(RemoteEditor::writable)?;
        let opener = worker.exec_opener()?;
        Some(Route {
            target: self.target.clone(),
            opener,
            sftp,
        })
    }
}
