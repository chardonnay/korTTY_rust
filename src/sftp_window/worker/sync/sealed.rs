//! Retain verified bytes on a private local handle before handing them off.

use super::*;

pub(super) struct SealedFile {
    stage: StagedDownload,
    stamp: FileStamp,
}

impl SealedFile {
    pub(super) fn from_source(
        source: &LocalSource,
        expected: [u8; 32],
        stopped: &dyn Fn() -> bool,
    ) -> Result<Self> {
        let size = source.size();
        if size > MAX_STANDING_BYTES {
            return Err(io::Error::other("Scheduled copy exceeds 256 MiB").into());
        }
        let stamp = FileStamp::read(source.file())?;
        let sealed = Self::new(size, expected, stopped, |output| {
            let mut input = source.file();
            input.rewind()?;
            Ok(io::copy(&mut input.take(size + 1), output)?)
        })?;
        stamp.require_unchanged(source.file())?;
        source.verify()?;
        Ok(sealed)
    }

    pub(super) fn new(
        size: u64,
        expected: [u8; 32],
        stopped: &dyn Fn() -> bool,
        write: impl FnOnce(&mut dyn Write) -> Result<u64>,
    ) -> Result<Self> {
        require_running(&stopped)?;
        let path = std::env::temp_dir().canonicalize()?.join(files::nonce()?);
        let stage = StagedDownload::resumable(&path)?;
        let file = stage.writer()?;
        let mut digest = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        let mut output = ObserveOutput(CheckedOutput(file, &stopped), |bytes: &[u8]| {
            digest.update(bytes);
        });
        if write(&mut output)? != size || hash_bytes(digest.finalize()) != expected {
            return Err(changed_bytes().into());
        }
        require_running(&stopped)?;
        file.sync_all()?;
        let stamp = FileStamp::read(file)?;
        let sealed = Self { stage, stamp };
        sealed.verify()?;
        Ok(sealed)
    }

    pub(super) fn file(&self) -> io::Result<&File> {
        self.stage.writer()
    }

    pub(super) fn verify(&self) -> io::Result<()> {
        self.stage.verify_stage(&self.stamp)
    }
}
