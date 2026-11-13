use std::io::{self, Read as _, Write as _};
use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};

use zeroize::Zeroizing;

use super::{Error, MAX_DIAGNOSTICS, POLL, ProcessInput, ProcessOutput, active, input::Launch};

pub(super) fn run(
    program: &Launch,
    _image: &std::fs::File,
    directory: &Path,
    input: ProcessInput<'_>,
    authorize: impl FnOnce() -> Result<(), Error>,
) -> Result<ProcessOutput, Error> {
    let ProcessInput {
        stdin: prompt,
        deadline,
        canceled,
        capture,
    } = input;
    let prepare = if capture.diagnostics() {
        kortty_pty::WindowsCli::prepare_with_diagnostics
    } else {
        kortty_pty::WindowsCli::prepare
    };
    let mut process = prepare(&program.path, &program.arguments, directory)
        .map_err(|_| Error::Review("Could not start the approved CLI executable.".into()))?;
    authorize()?;
    process
        .resume()
        .map_err(|_| Error::Review("Could not start the approved CLI executable.".into()))?;
    let mut input = process.input.take().ok_or(Error::Service)?;
    let output = process.output.take().ok_or(Error::Service)?;
    let diagnostics = process.diagnostics.take();
    std::thread::scope(|scope| {
        let result = (|| {
            let (sent, write) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("kortty-cli-input".into())
                .spawn_scoped(scope, move || {
                    let result = match input.write_all(prompt.as_bytes()) {
                        Ok(()) => Ok(true),
                        Err(error)
                            if capture.diagnostics()
                                && error.kind() == io::ErrorKind::BrokenPipe =>
                        {
                            Ok(false)
                        }
                        Err(error) => Err(error),
                    };
                    drop(input);
                    let _ = sent.send(result);
                })
                .map_err(|_| Error::Service)?;
            let limit = capture.output_limit();
            let read = reader(scope, output, limit + 1, capture.discard_stdout_tail())?;
            let diagnostics = diagnostics
                .map(|pipe| reader(scope, pipe, MAX_DIAGNOSTICS, true))
                .transpose()?;
            let mut written = None;
            let mut answer = None;
            let mut diagnostic_bytes = diagnostics.is_none().then(Zeroizing::default);
            loop {
                active(deadline, canceled)?;
                if written.is_none() {
                    written = take(&write)?;
                }
                if answer.is_none() {
                    answer = take(&read)?;
                    if !capture.discard_stdout_tail()
                        && answer.as_ref().is_some_and(|bytes| bytes.len() > limit)
                    {
                        return Err(capture.overflow());
                    }
                }
                if diagnostic_bytes.is_none()
                    && let Some(diagnostics) = &diagnostics
                {
                    diagnostic_bytes = take(diagnostics)?;
                }
                if let Some(success) = process.try_success().map_err(|_| Error::Service)? {
                    if !success && !capture.diagnostics() {
                        return Err(Error::Review("The CLI exited unsuccessfully.".into()));
                    }
                    if let (Some(written), Some(_), Some(_)) = (written, &answer, &diagnostic_bytes)
                    {
                        if success && !written {
                            return Err(Error::Service);
                        }
                        return Ok(ProcessOutput {
                            stdout: answer.take().unwrap_or_default(),
                            diagnostics: diagnostic_bytes.take().unwrap_or_default(),
                            success,
                            credential: None,
                        });
                    }
                }
                std::thread::sleep(POLL);
            }
        })();
        // The Job stops pipe-owning descendants before the scoped readers join.
        drop(process);
        result
    })
}

fn reader<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    mut pipe: io::PipeReader,
    limit: usize,
    drain: bool,
) -> Result<mpsc::Receiver<io::Result<Zeroizing<Vec<u8>>>>, Error> {
    let (received, read) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("kortty-cli-output".into())
        .spawn_scoped(scope, move || {
            let result = (|| {
                let mut bytes = Zeroizing::default();
                pipe.by_ref().take(limit as u64).read_to_end(&mut bytes)?;
                if drain {
                    io::copy(&mut pipe, &mut io::sink())?;
                }
                Ok(bytes)
            })();
            let _ = received.send(result);
        })
        .map_err(|_| Error::Service)?;
    Ok(read)
}

fn take<T>(channel: &mpsc::Receiver<io::Result<T>>) -> Result<Option<T>, Error> {
    match channel.try_recv() {
        Ok(result) => result.map(Some).map_err(|_| Error::Service),
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => Err(Error::Service),
    }
}
