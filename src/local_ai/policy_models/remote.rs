//! Staging and process-local provenance for policy-authorized remote GGUF files.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use pgp::crypto::hash::HashAlgorithm;

use crate::clipboard_policy::PolicyModel;
use crate::private_files::Directory;

use super::super::managed_models::{self, MAX_MODELS, OpenedModel, Purpose};
use super::{ModelIdentity, http, unc};

const DIRECTORY: &str = "policy-model-cache";
const WAIT_STEP: Duration = Duration::from_millis(50);
const VERIFY_CHUNK: usize = 1024 * 1024;
const VERIFY_POLICY_INTERVAL: u64 = 16 * VERIFY_CHUNK as u64;

#[derive(Clone, Copy)]
pub(super) enum Source<'a> {
    Http(&'a str),
    Unc(&'a str),
}

pub(super) struct Access<'a> {
    pub(super) cancelled: &'a AtomicBool,
    pub(super) deadline: Instant,
    pub(super) require_current: &'a dyn Fn() -> io::Result<()>,
}

impl Access<'_> {
    fn active(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Policy model acquisition was cancelled",
            ))
        } else if Instant::now() >= self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Policy model acquisition exceeded its time limit",
            ))
        } else {
            Ok(())
        }
    }

    pub(super) fn authorize(&self) -> io::Result<()> {
        self.active()?;
        (self.require_current)()
    }
}

struct Asset {
    definition: PolicyModel,
    content: [u8; 32],
    model: OpenedModel,
}

type Key = (PathBuf, String);
type Slot = Arc<Mutex<Option<Asset>>>;
static ASSETS: LazyLock<Mutex<HashMap<Key, Slot>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(super) fn ready_identity(root: &Path, definition: &PolicyModel) -> io::Result<ModelIdentity> {
    if let Some(digest) = definition.digest {
        return Ok(ModelIdentity(digest));
    }
    let slot = lock(&ASSETS)
        .get(&(root.to_owned(), definition.id.clone()))
        .cloned()
        .ok_or_else(not_ready)?;
    let state = slot.try_lock().map_err(|_| not_ready())?;
    let asset = state
        .as_ref()
        .filter(|asset| &asset.definition == definition)
        .ok_or_else(not_ready)?;
    // The process authenticated this content. A later pathname or metadata
    // change is handled by the leased-file check or a fresh hash at open.
    Ok(ModelIdentity(asset.content))
}

pub(super) fn open(
    root: &Path,
    definition: &PolicyModel,
    source: Source<'_>,
    purpose: Purpose,
    expected: Option<&ModelIdentity>,
    access: &Access<'_>,
) -> io::Result<OpenedModel> {
    access.authorize()?;
    let slot = slot_for(root, definition)?;
    let mut state = wait_for_slot(&slot, access)?;
    let name = cache_name(definition)?;
    let path = root.join(DIRECTORY).join(&name);

    if let Some(asset) = state
        .as_ref()
        .filter(|asset| &asset.definition == definition)
        && asset.model.revalidate().is_ok()
    {
        require_digest(expected, asset.content)?;
        return open_same_file(definition, &path, purpose, asset, access);
    }
    let authenticated_content = state
        .take()
        .filter(|asset| &asset.definition == definition)
        .map(|asset| asset.content);
    let directory = Directory::open(root, false)?.private_child(DIRECTORY, true)?;
    let guard = directory.open_lock(&format!("{name}.lock"))?;
    while let Err(error) = guard.try_lock() {
        match error {
            std::fs::TryLockError::WouldBlock => wait(access)?,
            std::fs::TryLockError::Error(error) => return Err(error),
        }
    }
    access.authorize()?;

    let required = definition
        .digest
        .or_else(|| expected.map(|identity| identity.0));
    let existing = definition
        .digest
        .or(authenticated_content)
        .and_then(|digest| {
            verify(&path, definition, digest, access)
                .ok()
                .map(|model| (model, digest))
        });
    let (model, content) = if let Some(existing) = existing {
        existing
    } else {
        download(
            &directory, &name, &path, definition, source, required, access,
        )?
    };
    require_digest(expected, content)?;
    let asset = Asset {
        definition: definition.clone(),
        content,
        model,
    };
    let opened = open_same_file(definition, &path, purpose, &asset, access)?;
    *state = Some(asset);
    Ok(opened)
}

fn slot_for(root: &Path, definition: &PolicyModel) -> io::Result<Slot> {
    let key = (root.to_owned(), definition.id.clone());
    let mut assets = lock(&ASSETS);
    if let Some(slot) = assets.get(&key) {
        return Ok(slot.clone());
    }
    if assets.len() >= MAX_MODELS {
        let current = crate::clipboard_policy::ClipboardPolicy::load();
        assets.retain(|_, slot| {
            Arc::strong_count(slot) > 1
                || slot.try_lock().is_ok_and(|state| {
                    state.as_ref().is_some_and(|asset| {
                        current.ai_runtime_models().contains(&asset.definition)
                    })
                })
        });
    }
    if assets.len() >= MAX_MODELS {
        return Err(io::Error::other(
            "The prepared policy model limit has been reached",
        ));
    }
    let slot = Arc::new(Mutex::new(None));
    assets.insert(key, slot.clone());
    Ok(slot)
}

fn wait_for_slot<'a>(
    slot: &'a Slot,
    access: &Access<'_>,
) -> io::Result<MutexGuard<'a, Option<Asset>>> {
    loop {
        match slot.try_lock() {
            Ok(state) => return Ok(state),
            Err(TryLockError::Poisoned(error)) => return Ok(error.into_inner()),
            Err(TryLockError::WouldBlock) => wait(access)?,
        }
    }
}

fn wait(access: &Access<'_>) -> io::Result<()> {
    access.active()?;
    std::thread::sleep(WAIT_STEP.min(access.deadline.saturating_duration_since(Instant::now())));
    Ok(())
}

fn cache_name(definition: &PolicyModel) -> io::Result<String> {
    let bytes = HashAlgorithm::Sha256
        .digest(definition.id.as_bytes())
        .map_err(io::Error::other)?;
    let mut name = String::with_capacity(69);
    for byte in bytes {
        write!(name, "{byte:02x}").expect("writing to a String succeeds");
    }
    name.push_str(".gguf");
    Ok(name)
}

fn download(
    directory: &Directory,
    name: &str,
    path: &Path,
    definition: &PolicyModel,
    source: Source<'_>,
    required: Option<[u8; 32]>,
    access: &Access<'_>,
) -> io::Result<(OpenedModel, [u8; 32])> {
    let replace = match directory.open_private_file(name) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    let content = Cell::new(None);
    let retained = RefCell::new(None);
    let write = |file: &mut File| {
        access.authorize()?;
        let mut hash = HashAlgorithm::Sha256
            .new_hasher()
            .map_err(io::Error::other)?;
        let mut sink = HashedFile {
            file,
            update: |bytes: &[u8]| hash.update(bytes),
        };
        let bytes = match source {
            Source::Http(url) => http::receive(
                url,
                definition.digest.is_some(),
                &mut sink,
                access.cancelled,
                access.deadline,
                access.require_current,
            ),
            Source::Unc(path) => unc::receive(
                path,
                &mut sink,
                access.cancelled,
                access.deadline,
                access.require_current,
            ),
        }?;
        let actual: [u8; 32] = hash
            .finalize()
            .as_ref()
            .try_into()
            .expect("SHA-256 has 32 bytes");
        if required.is_some_and(|digest| digest != actual) {
            return Err(digest_mismatch());
        }
        file.seek(SeekFrom::Start(0))?;
        managed_models::validate_policy_cache_file(file, bytes, access.cancelled)?;
        content.set(Some(actual));
        Ok(())
    };
    let publish = if replace {
        directory.replace_with_readable(name, write, || {
            access.authorize()?;
            // A fresh HTTPS download can authenticate the existing bytes. Keep
            // that inode when another process may still be using it.
            if definition.digest.is_none()
                && let Some(digest) = content.get()
                && let Ok(model) = verify(path, definition, digest, access)
            {
                *retained.borrow_mut() = Some((model, digest));
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            access.authorize()
        })
    } else {
        directory.publish_checked_readable(name, write, || access.authorize())
    };
    if let Some(verified) = retained.into_inner() {
        access.authorize()?;
        return Ok(verified);
    }
    publish?;
    directory.sync()?;
    let digest = content
        .get()
        .ok_or_else(|| io::Error::other("The verified model digest is missing"))?;
    Ok((verify(path, definition, digest, access)?, digest))
}

struct HashedFile<'a, F> {
    file: &'a mut File,
    update: F,
}

impl<F: FnMut(&[u8])> Write for HashedFile<'_, F> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = self.file.write(bytes)?;
        (self.update)(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

fn verify(
    path: &Path,
    definition: &PolicyModel,
    content: [u8; 32],
    access: &Access<'_>,
) -> io::Result<OpenedModel> {
    access.authorize()?;
    let model =
        managed_models::open_policy_cache_model(definition, path, Purpose::Chat, access.cancelled)?;
    verify_opened(&model, content, access)?;
    Ok(model)
}

pub(super) fn verify_opened(
    model: &OpenedModel,
    content: [u8; 32],
    access: &Access<'_>,
) -> io::Result<()> {
    access.authorize()?;
    verify_bytes(
        model.policy_cache_file()?,
        model.model.bytes,
        content,
        access,
    )?;
    model.revalidate()?;
    access.authorize()
}

#[cfg(target_os = "linux")]
pub(super) fn protect_for_launch(
    model: OpenedModel,
    expected: &ModelIdentity,
    access: &Access<'_>,
) -> io::Result<OpenedModel> {
    access.authorize()?;
    let bytes = model.model.bytes;
    let protected = super::immutable::protect(
        model.policy_cache_file()?,
        bytes,
        access.cancelled,
        access.deadline,
    )?;
    verify_bytes(protected.file(), bytes, expected.0, access)?;
    access.authorize()?;
    Ok(model.into_immutable_policy(protected))
}

fn verify_bytes(file: &File, bytes: u64, content: [u8; 32], access: &Access<'_>) -> io::Result<()> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hash = HashAlgorithm::Sha256
        .new_hasher()
        .map_err(io::Error::other)?;
    let mut buffer = vec![0; VERIFY_CHUNK];
    let mut remaining = bytes;
    let mut since_check = 0;
    while remaining > 0 {
        access.active()?;
        if since_check >= VERIFY_POLICY_INTERVAL {
            access.authorize()?;
            since_check = 0;
        }
        let limit = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..limit])?;
        if count == 0 {
            return Err(digest_mismatch());
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
        since_check += count as u64;
    }
    if hash.finalize().as_ref() != content || file.read(&mut buffer[..1])? != 0 {
        return Err(digest_mismatch());
    }
    access.authorize()
}

fn open_same_file(
    definition: &PolicyModel,
    path: &Path,
    purpose: Purpose,
    asset: &Asset,
    access: &Access<'_>,
) -> io::Result<OpenedModel> {
    let opened =
        managed_models::open_policy_cache_model(definition, path, purpose, access.cancelled)?;
    if !opened.policy_stamps()?.eq(asset.model.policy_stamps()?) {
        return Err(digest_mismatch());
    }
    access.authorize()?;
    Ok(opened)
}

fn require_digest(expected: Option<&ModelIdentity>, digest: [u8; 32]) -> io::Result<()> {
    if expected.is_some_and(|expected| expected.0 != digest) {
        return Err(digest_mismatch());
    }
    Ok(())
}

fn digest_mismatch() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "The policy model bytes do not match the authorized SHA-256",
    )
}

fn not_ready() -> io::Error {
    io::Error::new(
        io::ErrorKind::WouldBlock,
        "The policy model must be prepared. Refresh the model list and try again.",
    )
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::thread;

    use crate::clipboard_policy::PolicyModelRuntime;
    use crate::local_ai::test_support::{accept, listener, wire_request};

    use super::*;

    struct Fixture {
        _scratch: crate::snippets::tests::Profile,
        root: PathBuf,
        definition: PolicyModel,
        cancelled: AtomicBool,
    }

    impl Fixture {
        fn new(source: String, digest: Option<[u8; 32]>) -> Self {
            let scratch = crate::snippets::tests::Profile::new();
            let root = scratch.0.canonicalize().unwrap();
            Self {
                _scratch: scratch,
                root,
                definition: PolicyModel {
                    id: "policy-test-model".into(),
                    name: "Test Model".into(),
                    runtime: PolicyModelRuntime::Gguf,
                    source,
                    digest,
                    protected_cache: None,
                },
                cancelled: AtomicBool::new(false),
            }
        }

        fn cache_path(&self) -> PathBuf {
            self.root
                .join(DIRECTORY)
                .join(cache_name(&self.definition).unwrap())
        }

        fn forget_process_state(&self) {
            lock(&ASSETS).remove(&(self.root.clone(), self.definition.id.clone()));
        }

        fn open(&self, purpose: Purpose) -> io::Result<OpenedModel> {
            self.open_for(purpose, Duration::from_secs(5))
        }

        fn open_for(&self, purpose: Purpose, timeout: Duration) -> io::Result<OpenedModel> {
            let super::super::Source::Remote(source) = super::super::source(&self.definition)?
            else {
                panic!("expected a remote policy fixture");
            };
            open(
                &self.root,
                &self.definition,
                source,
                purpose,
                self.definition.digest.map(ModelIdentity).as_ref(),
                &Access {
                    cancelled: &self.cancelled,
                    deadline: Instant::now() + timeout,
                    require_current: &|| Ok(()),
                },
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.forget_process_state();
        }
    }

    fn serve(bytes: Vec<u8>) -> (String, thread::JoinHandle<()>) {
        let (listener, _) = listener();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let mut stream = accept(&listener);
            assert!(wire_request(&mut stream, "GET /model.gguf HTTP/1.1").is_empty());
            let mut stream = stream.into_inner();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        });
        (format!("http://{address}/model.gguf"), worker)
    }

    #[test]
    fn policy_remote_model_verifies_download_and_disk_cache_on_reopen() {
        let bytes = managed_models::policy_cache_fixture(2048);
        let digest = HashAlgorithm::Sha256
            .digest(&bytes)
            .unwrap()
            .try_into()
            .unwrap();
        let (source, server) = serve(bytes.clone());
        let fixture = Fixture::new(source, Some(digest));
        let opened = fixture.open(Purpose::Embedding).unwrap();
        server.join().unwrap();
        assert_eq!(opened.model.purpose, Purpose::Embedding);
        assert_eq!(opened.model.context_tokens, 2048);
        drop(opened);
        fixture.forget_process_state();

        assert!(fixture.open(Purpose::Chat).is_ok());
        fixture.forget_process_state();
        std::fs::write(
            fixture.cache_path(),
            managed_models::policy_cache_fixture(4096),
        )
        .unwrap();
        assert!(fixture.open(Purpose::Chat).is_err());
    }

    #[test]
    fn policy_remote_model_refuses_wrong_digest_before_publication() {
        let (source, server) = serve(managed_models::policy_cache_fixture(2048));
        let fixture = Fixture::new(source, Some([3; 32]));
        let error = fixture.open(Purpose::Chat).err().unwrap();
        server.join().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!fixture.cache_path().exists());
    }

    #[test]
    fn policy_remote_model_without_pin_requires_current_process_provenance() {
        let fixture = Fixture::new("https://models.example.invalid/model.gguf".into(), None);
        Directory::open(&fixture.root, false)
            .unwrap()
            .private_child(DIRECTORY, true)
            .unwrap()
            .publish_copy(
                &cache_name(&fixture.definition).unwrap(),
                &managed_models::policy_cache_fixture(2048),
            )
            .unwrap();
        let error = ready_identity(&fixture.root, &fixture.definition).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    }

    fn live_digest() -> [u8; 32] {
        let text = std::env::var("KORTTY_SMOKE_POLICY_SHA256").expect("configured fixture SHA-256");
        assert!(text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit()));
        std::array::from_fn(|index| {
            u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap()
        })
    }

    #[test]
    #[ignore = "requires an explicitly configured HTTPS GGUF fixture with a system-trusted certificate"]
    fn policy_remote_model_unpinned_https_live_fixture() {
        let source = std::env::var("KORTTY_SMOKE_POLICY_HTTPS").expect("configured HTTPS fixture");
        assert!(source.starts_with("https://"));
        let fixture = Fixture::new(source, None);
        let opened = fixture
            .open_for(Purpose::Chat, Duration::from_secs(120))
            .unwrap();
        assert_eq!(
            ready_identity(&fixture.root, &fixture.definition)
                .unwrap()
                .0,
            live_digest()
        );
        fixture.forget_process_state();
        assert!(ready_identity(&fixture.root, &fixture.definition).is_err());
        let reopened = fixture
            .open_for(Purpose::Chat, Duration::from_secs(120))
            .unwrap();
        assert!(
            reopened
                .policy_stamps()
                .unwrap()
                .eq(opened.policy_stamps().unwrap())
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires an authorized Windows SMB share with a complete GGUF file"]
    fn policy_remote_model_windows_smb_live_fixture() {
        let source = std::env::var("KORTTY_SMOKE_POLICY_UNC").expect("configured UNC fixture");
        super::super::unc::validate(&source).unwrap();
        let fixture = Fixture::new(source, Some(live_digest()));
        let opened = fixture
            .open_for(Purpose::Chat, Duration::from_secs(120))
            .unwrap();
        assert_eq!(opened.model.id, fixture.definition.id);
        opened.revalidate().unwrap();
    }
}
