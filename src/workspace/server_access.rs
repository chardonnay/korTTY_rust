use iced::window;
use kortty_control::{ErrorCode, Fault};
use serde_json::json;

use crate::connection_store::Connection;
use crate::ssh_dialog::{LoginTarget, SshDialog};

use super::Workspace;

pub(super) fn blocked_notice(host: &str, port: u16) -> String {
    format!("Your organization's server policy blocks {host}:{port}.")
}

impl Workspace {
    pub(super) fn blocked_ssh_server<'a>(
        &self,
        target: &'a LoginTarget,
        jump: Option<&'a LoginTarget>,
    ) -> Option<(&'a str, u16)> {
        self.clipboard
            .policy
            .server_access()
            .first_blocked(target.servers(jump))
    }

    pub(super) fn blocked_ssh_form(&self, form: &SshDialog) -> Option<String> {
        let (target, jump) = form.server_addresses()?;
        self.clipboard
            .policy
            .server_access()
            .first_blocked(
                jump.iter()
                    .chain(std::iter::once(&target))
                    .map(|(host, port)| (host.as_str(), *port)),
            )
            .map(|(host, port)| blocked_notice(host, port))
    }

    pub(super) fn blocked_saved_server(&self, connection: &Connection) -> Option<String> {
        connection
            .blocked_server(self.clipboard.policy.server_access())
            .map(|(host, port)| blocked_notice(host, port))
    }

    pub(super) fn allow_saved_server(
        &mut self,
        owner: window::Id,
        connection: &Connection,
    ) -> bool {
        if let Some(notice) = self.blocked_saved_server(connection) {
            self.notice(owner, notice);
            return false;
        }
        true
    }

    pub(super) fn control_saved_server(&self, connection: &Connection) -> Result<(), Fault> {
        if let Some((host, port)) = connection.blocked_server(self.clipboard.policy.server_access())
        {
            return Err(Fault::new(
                ErrorCode::Policy,
                "The organization's server policy blocks this connection",
            )
            .with("target", json!(format!("{host}:{port}"))));
        }
        Ok(())
    }
}
