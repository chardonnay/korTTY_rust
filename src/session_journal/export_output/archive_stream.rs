//! Bounded handoff from ZIP production to OpenPGP encryption without a plaintext file.

use std::io::{self, Read, Write};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;

use zeroize::Zeroizing;

const CHUNK_BYTES: usize = 64 * 1024;
type Chunk = Zeroizing<Vec<u8>>;

pub(super) fn transfer(
    produce: impl FnOnce(Writer) -> io::Result<()>,
    consume: impl FnOnce(Reader) -> io::Result<()> + Send,
) -> io::Result<()> {
    thread::scope(|scope| {
        let (send, receive) = sync_channel(2);
        let consumer = thread::Builder::new()
            .name("kortty-journal-openpgp".into())
            .spawn_scoped(scope, move || consume(Reader::new(receive)))?;
        let produced = produce(Writer(send));
        let consumed = consumer
            .join()
            .map_err(|_| io::Error::other("Journal encryption worker stopped"))?;
        match produced {
            Ok(()) => consumed,
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => consumed.and(Err(error)),
            Err(error) => Err(error),
        }
    })
}

pub(super) struct Writer(SyncSender<Chunk>);

impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let size = bytes.len().min(CHUNK_BYTES);
        if size != 0 {
            self.0
                .send(Zeroizing::new(bytes[..size].to_vec()))
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        }
        Ok(size)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) struct Reader {
    receive: Receiver<Chunk>,
    chunk: Chunk,
    offset: usize,
}

impl Reader {
    fn new(receive: Receiver<Chunk>) -> Self {
        Self {
            receive,
            chunk: Zeroizing::new(Vec::new()),
            offset: 0,
        }
    }
}

impl Read for Reader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.offset == self.chunk.len() {
            let Ok(chunk) = self.receive.recv() else {
                return Ok(0);
            };
            self.chunk = chunk;
            self.offset = 0;
        }
        let mut remaining = &self.chunk[self.offset..];
        let size = remaining.read(output)?;
        self.offset += size;
        Ok(size)
    }
}
