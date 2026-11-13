//! Relays a sudo terminal only after the fixed validation script has finished.

use std::io::{Read as _, Write as _};

use zeroize::Zeroizing;

use super::{ConnectError, Flow, Frame, Server, closed, exec, lock};
use crate::ssh_runtime::{password::Answer, sudo_pty, wire::CHUNK};

impl Server {
    pub(super) fn open_sudo_pty(&self, flow: &Flow, nonce: [u8; 16]) -> Result<(), ConnectError> {
        let opener = self
            .exec
            .as_ref()
            .filter(|opener| opener.verified_host_key().is_some())
            .ok_or_else(closed)?;
        let gate = flow.clone();
        let mut lease = opener
            .open_pty_cancellable(&sudo_pty::command(nonce), move || !gate.password_open())?;
        {
            let mut active = lock(&self.active);
            if !flow.password_open() {
                return Ok(());
            }
            active.insert(flow.id(), lease.abort_handle());
        }
        let result = (|| {
            self.mux.control(Frame::Opened(flow.id(), None))?;
            let gate = flow.clone();
            let authenticated = sudo_pty::authenticate(
                &mut lease,
                nonce,
                move || gate.password_open(),
                || match flow.take_password() {
                    Ok(Some(line)) => Answer::Line(line),
                    Ok(None) => Answer::Pending,
                    Err(_) => Answer::Closed,
                },
                || flow.password_challenge().is_ok(),
            );
            match authenticated {
                Ok(mut terminal) => {
                    flow.ready_password_file()?;
                    while !flow.wait_password_file_ready(super::ACCEPT_POLL)? {
                        if !flow.password_open() {
                            return Ok(());
                        }
                    }
                    exec::relay(flow, &lease, terminal.writer, None, |output| {
                        let mut bytes = Zeroizing::new([0; CHUNK]);
                        loop {
                            let count = terminal.reader.read(&mut bytes[..])?;
                            if count == 0 {
                                return Ok(());
                            }
                            output.write_all(&bytes[..count])?;
                        }
                    })
                }
                Err(status) if flow.password_open() => flow.finish_password(status),
                Err(_) => Ok(()),
            }
        })();
        lock(&self.active).remove(&flow.id());
        result.map_err(Into::into)
    }
}
