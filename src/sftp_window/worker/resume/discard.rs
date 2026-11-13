//! Keeps a live offer intact when its durable receipt cannot be removed.

use super::*;

impl Offer {
    pub(in crate::sftp_window) fn try_discard(&self) -> io::Result<()> {
        let Some(state) = self.download_state() else {
            return Err(io::Error::other("Reconnect to discard the partial upload"));
        };
        let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(download) = state.as_mut() {
            download.discard_receipt()?;
            download.staged.discard_checkpoint();
        }
        drop(state.take());
        Ok(())
    }
}
