use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Condvar, Mutex};

use zeroize::Zeroize;

use super::{ReadWait, closed, lock};

pub(super) struct OutputSender(Arc<OutputQueue>);
pub(super) struct OutputReceiver(Arc<OutputQueue>);

struct OutputQueue {
    state: Mutex<OutputState>,
    ready: Condvar,
}

struct OutputState {
    bytes: VecDeque<u8>,
    sensitive: bool,
    sender_closed: bool,
    receiver_closed: bool,
}

pub(super) fn queue() -> (OutputSender, OutputReceiver) {
    queue_with_wipe(false)
}

pub(super) fn queue_with_wipe(sensitive: bool) -> (OutputSender, OutputReceiver) {
    let queue = Arc::new(OutputQueue {
        state: Mutex::new(OutputState {
            bytes: if sensitive {
                // The receive window bounds the queue and avoids reallocating live bytes.
                VecDeque::with_capacity(sunset::config::DEFAULT_WINDOW)
            } else {
                VecDeque::new()
            },
            sensitive,
            sender_closed: false,
            receiver_closed: false,
        }),
        ready: Condvar::new(),
    });
    (OutputSender(Arc::clone(&queue)), OutputReceiver(queue))
}

impl OutputState {
    fn wipe_prefix(&mut self, count: usize) {
        if self.sensitive {
            let (first, second) = self.bytes.as_mut_slices();
            let split = first.len().min(count);
            first[..split].zeroize();
            second[..count - split].zeroize();
        }
    }
}

impl Drop for OutputState {
    fn drop(&mut self) {
        self.wipe_prefix(self.bytes.len());
    }
}

impl OutputSender {
    pub(super) fn send(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = lock(&self.0.state);
        if state.receiver_closed {
            return Err(closed());
        }
        if bytes.len() > sunset::config::DEFAULT_WINDOW - state.bytes.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "SSH channel exceeded its receive window",
            ));
        }
        let wake = state.bytes.is_empty();
        state.bytes.extend(bytes.iter().copied());
        drop(state);
        if wake {
            self.0.ready.notify_one();
        }
        Ok(())
    }
}

impl Drop for OutputSender {
    fn drop(&mut self) {
        lock(&self.0.state).sender_closed = true;
        self.0.ready.notify_one();
    }
}

impl OutputReceiver {
    pub(super) fn read(&self, bytes: &mut [u8], wait: ReadWait) -> io::Result<usize> {
        let state = lock(&self.0.state);
        let pending = |state: &mut OutputState| state.bytes.is_empty() && !state.sender_closed;
        let mut state = match wait {
            ReadWait::Blocking => self
                .0
                .ready
                .wait_while(state, pending)
                .unwrap_or_else(|error| error.into_inner()),
            ReadWait::Timeout(timeout) => {
                let (state, deadline) = self
                    .0
                    .ready
                    .wait_timeout_while(state, timeout, pending)
                    .unwrap_or_else(|error| error.into_inner());
                if deadline.timed_out() {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                state
            }
            ReadWait::Nonblocking if state.bytes.is_empty() && !state.sender_closed => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            ReadWait::Nonblocking => state,
        };
        let count = state.bytes.len().min(bytes.len());
        let (first, second) = state.bytes.as_slices();
        let split = first.len().min(count);
        bytes[..split].copy_from_slice(&first[..split]);
        bytes[split..count].copy_from_slice(&second[..count - split]);
        state.wipe_prefix(count);
        state.bytes.drain(..count);
        Ok(count)
    }

    pub(super) fn close(&self) -> usize {
        let mut state = lock(&self.0.state);
        state.receiver_closed = true;
        let count = state.bytes.len();
        state.wipe_prefix(count);
        state.bytes.clear();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_budget_counts_bytes_instead_of_packets_and_keeps_final_output() {
        let (sender, receiver) = queue();
        for byte in 0..u8::MAX {
            sender.send(&[byte]).unwrap();
        }
        let mut buffer = [0; u8::MAX as usize];
        assert_eq!(
            receiver.read(&mut buffer, ReadWait::Nonblocking).unwrap(),
            buffer.len()
        );
        assert!(buffer.into_iter().eq(0..u8::MAX));

        let payload = vec![123; sunset::config::DEFAULT_WINDOW];
        sender.send(&payload).unwrap();
        assert_eq!(
            sender.send(&[0]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        drop(sender);
        let mut drained = vec![0; payload.len()];
        assert_eq!(
            receiver.read(&mut drained, ReadWait::Blocking).unwrap(),
            payload.len()
        );
        assert_eq!(drained, payload);
        assert_eq!(receiver.read(&mut buffer, ReadWait::Blocking).unwrap(), 0);
    }
}
