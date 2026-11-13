//! Revalidates private approvals and their local files for a reviewed Terminal AI request.

use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use zeroize::Zeroizing;

use crate::private_files::{Directory, EntryIdentity, FileStamp, require_single_link};

use super::{MAX_FILE_BYTES, Reference, Registry, format, registry};

const MAX_CHECKS: usize = 32;
const MAX_READ_BYTES: usize = 4 * MAX_FILE_BYTES + MAX_CHECKS;
const MAX_EXCERPT_BYTES: usize = 16 * 1024;
const MAX_PATH_BYTES: usize = 4096;
const MAX_PATH_COMPONENTS: usize = 64;

pub(in crate::ai_manager::memory) struct ApprovedReference {
    pub(in crate::ai_manager::memory) text: Zeroizing<String>,
    pub(in crate::ai_manager::memory) binding: ReferenceBinding,
}

#[derive(Clone)]
pub(in crate::ai_manager::memory) struct ReferenceBinding {
    reference: Reference,
    identity: OpenIdentity,
}

impl ReferenceBinding {
    pub(in crate::ai_manager::memory) fn is_connection(&self) -> bool {
        self.reference.connection_id.is_some()
    }
}

#[derive(Clone, Eq, PartialEq)]
struct OpenIdentity {
    home: EntryIdentity,
    root: EntryIdentity,
    parent: EntryIdentity,
    file: FileStamp,
}

pub(in crate::ai_manager::memory) fn load(
    directory: &Path,
    connection: Option<u64>,
    allows_path: impl Fn(&Path) -> bool,
) -> io::Result<Vec<ApprovedReference>> {
    select(
        directory,
        connection,
        std::env::home_dir().as_deref(),
        &allows_path,
    )
}

pub(in crate::ai_manager::memory) fn verify(
    directory: &Path,
    bindings: &[ReferenceBinding],
    allows_path: impl Fn(&Path) -> bool,
) -> io::Result<()> {
    verify_in_home(
        directory,
        bindings,
        std::env::home_dir().as_deref(),
        &allows_path,
    )
}

#[cfg(test)]
pub(in crate::ai_manager::memory) fn load_with_home(
    directory: &Path,
    connection: Option<u64>,
    home: &Path,
) -> io::Result<Vec<ApprovedReference>> {
    select(directory, connection, Some(home), &|_| true)
}

fn select(
    directory: &Path,
    connection: Option<u64>,
    home: Option<&Path>,
    allows_path: &impl Fn(&Path) -> bool,
) -> io::Result<Vec<ApprovedReference>> {
    if connection == Some(0) {
        return Err(io::Error::other(
            "AI reference memory requires a saved connection ID",
        ));
    }
    let connection = connection.map(|id| id.to_string());
    let registry = registry(directory)
        .map_err(|_| io::Error::other("Could not read approved AI memory references"))?;
    let candidates = registry.entries.iter().filter(|entry| {
        eligible(entry)
            && (entry.connection_id.is_none() || entry.connection_id == connection)
            && unique(&registry, &entry.id).is_some()
    });
    let mut candidates = candidates.peekable();
    if candidates.peek().is_none() {
        return Ok(Vec::new());
    }
    let Some(home) = home.and_then(|path| Home::open(path).ok()) else {
        return Ok(Vec::new());
    };
    let mut budget = MAX_READ_BYTES;
    let mut approved = Vec::new();
    for entry in candidates
        .filter(|entry| reference_location(&home, entry).is_ok())
        .take(MAX_CHECKS)
    {
        if budget == 0 {
            break;
        }
        let Ok((identity, bytes)) = read_approved(&home, entry, &mut budget, allows_path) else {
            continue;
        };
        let Ok(text) = excerpt(&bytes, Path::new(&entry.path)) else {
            continue;
        };
        if !text.trim().is_empty() {
            approved.push(ApprovedReference {
                text,
                binding: ReferenceBinding {
                    reference: entry.clone(),
                    identity,
                },
            });
        }
    }
    if !approved.is_empty() && home.require_current().is_err() {
        approved.clear();
    }
    Ok(approved)
}

fn verify_in_home(
    directory: &Path,
    bindings: &[ReferenceBinding],
    home: Option<&Path>,
    allows_path: &impl Fn(&Path) -> bool,
) -> io::Result<()> {
    if bindings.is_empty() {
        return Ok(());
    }
    if bindings.len() > MAX_CHECKS {
        return Err(changed());
    }
    let current = registry(directory).map_err(|_| changed())?;
    let home = Home::open(home.ok_or_else(changed)?).map_err(|_| changed())?;
    let mut budget = MAX_READ_BYTES;
    for binding in bindings {
        let entry = unique(&current, &binding.reference.id)
            .filter(|entry| **entry == binding.reference && eligible(entry))
            .ok_or_else(changed)?;
        let (identity, _) =
            read_approved(&home, entry, &mut budget, allows_path).map_err(|_| changed())?;
        if identity != binding.identity {
            return Err(changed());
        }
    }
    home.require_current().map_err(|_| changed())
}

fn eligible(entry: &Reference) -> bool {
    entry.active
        && entry.status == "ok"
        && !entry.id.is_empty()
        && entry.id.len() <= 128
        && entry
            .id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && entry.approved_sha256.len() == 64
        && entry
            .approved_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn unique<'a>(registry: &'a Registry, id: &str) -> Option<&'a Reference> {
    let mut matching = registry.entries.iter().filter(|entry| entry.id == id);
    let entry = matching.next()?;
    matching.next().is_none().then_some(entry)
}

struct Home {
    configured: PathBuf,
    canonical: PathBuf,
    directory: Directory,
    identity: EntryIdentity,
}

impl Home {
    fn open(path: &Path) -> io::Result<Self> {
        if !ordinary_absolute(path) {
            return Err(ineligible());
        }
        let canonical = path.canonicalize()?;
        let directory = Directory::open(&canonical, false)?;
        let identity = directory.identity()?;
        Ok(Self {
            configured: path.to_owned(),
            canonical,
            directory,
            identity,
        })
    }

    fn relative<'a>(&self, path: &'a Path) -> io::Result<&'a Path> {
        path.strip_prefix(&self.configured)
            .or_else(|_| path.strip_prefix(&self.canonical))
            .map_err(|_| ineligible())
    }

    fn require_current(&self) -> io::Result<()> {
        if self.configured.canonicalize()? != self.canonical
            || Directory::open(&self.canonical, false)?.identity()? != self.identity
        {
            return Err(changed());
        }
        Ok(())
    }
}

fn ordinary_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some_and(|value| {
            value.len() <= MAX_PATH_BYTES && !crate::ssh_dialog::unsafe_text(value)
        })
        && path.components().count() <= MAX_PATH_COMPONENTS
        && !path
            .components()
            .any(|component| component == Component::ParentDir)
}

fn descend(root: &Directory, relative: &Path) -> io::Result<Option<Directory>> {
    let mut current = None;
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(ineligible());
        };
        let name = name.to_str().ok_or_else(ineligible)?;
        current = Some(current.as_ref().unwrap_or(root).subdirectory(name, false)?);
    }
    Ok(current)
}

fn reference_location<'a>(
    home: &Home,
    entry: &'a Reference,
) -> io::Result<(&'a Path, &'a Path, &'a str)> {
    let path = Path::new(&entry.path);
    let root = Path::new(&entry.root);
    if !ordinary_absolute(path) || !ordinary_absolute(root) || !markdown(path) {
        return Err(ineligible());
    }
    let relative = path.strip_prefix(root).map_err(|_| ineligible())?;
    let name = relative
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(ineligible)?;
    let parent = relative.parent().ok_or_else(ineligible)?;
    Ok((home.relative(root)?, parent, name))
}

fn open_reference(
    home: &Home,
    entry: &Reference,
    allows_path: &impl Fn(&Path) -> bool,
) -> io::Result<(File, OpenIdentity)> {
    let (relative_root, parent, name) = reference_location(home, entry)?;
    let root = descend(&home.directory, relative_root)?;
    let root = root.as_ref().unwrap_or(&home.directory);
    let directory = descend(root, parent)?;
    let directory = directory.as_ref().unwrap_or(root);
    let file = directory.open_file(name)?;
    if !is_local(&file)? {
        return Err(ineligible());
    }
    require_single_link(&file)?;
    let identity = OpenIdentity {
        home: home.identity,
        root: root.identity()?,
        parent: directory.identity()?,
        file: FileStamp::read(&file)?,
    };
    // Resolve the spelling used by policy, then prove it names the same opened local file.
    // A renamed path or a substituted link cannot authorize a different file handle.
    let canonical = home
        .canonical
        .join(relative_root)
        .join(parent)
        .join(name)
        .canonicalize()?;
    if !canonical.starts_with(&home.canonical) || !markdown(&canonical) {
        return Err(ineligible());
    }
    let canonical_parent = Directory::open(canonical.parent().ok_or_else(ineligible)?, false)?;
    if !directory.same_as(&canonical_parent)? {
        return Err(changed());
    }
    let canonical_name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(ineligible)?;
    identity
        .file
        .require_unchanged(&canonical_parent.open_file(canonical_name)?)?;
    if !allows_path(&canonical) {
        return Err(ineligible());
    }
    Ok((file, identity))
}

fn read_approved(
    home: &Home,
    entry: &Reference,
    remaining: &mut usize,
    allows_path: &impl Fn(&Path) -> bool,
) -> io::Result<(OpenIdentity, Zeroizing<Vec<u8>>)> {
    let (file, identity) = open_reference(home, entry, allows_path)?;
    let limit = (*remaining).min(MAX_FILE_BYTES + 1);
    if file.metadata()?.len() >= limit as u64 {
        return Err(ineligible());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    let read = (&file).take(limit as u64).read_to_end(&mut bytes);
    *remaining -= bytes.len();
    read?;
    if bytes.len() == limit || bytes.contains(&0) {
        return Err(ineligible());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| ineligible())?;
    if format::hash(text).as_deref() != Some(entry.approved_sha256.as_str()) {
        return Err(changed());
    }
    identity.file.require_unchanged(&file)?;
    require_single_link(&file)?;
    let (_, current) = open_reference(home, entry, allows_path)?;
    if current != identity {
        return Err(changed());
    }
    Ok((identity, bytes))
}

fn markdown(path: &Path) -> bool {
    extension(path, "md") || extension(path, "mdc")
}

fn extension(path: &Path, suffix: &str) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(suffix))
}

fn excerpt(bytes: &[u8], path: &Path) -> io::Result<Zeroizing<String>> {
    let source = std::str::from_utf8(bytes).map_err(|_| ineligible())?;
    let clean = format::checked_reference_text(source).map_err(|_| ineligible())?;
    let text = if extension(path, "mdc") {
        strip_front_matter(&clean)?
    } else {
        &clean
    };
    let text = if text.len() > MAX_EXCERPT_BYTES {
        let prefix = &text[..text.floor_char_boundary(MAX_EXCERPT_BYTES)];
        prefix
            .rfind("\n\n")
            .filter(|&index| index > 0)
            .or_else(|| prefix.rfind('\n').filter(|&index| index > 0))
            .map_or(prefix, |index| &prefix[..index])
            .trim_end()
    } else {
        text
    };
    Ok(Zeroizing::new(text.to_owned()))
}

fn strip_front_matter(text: &str) -> io::Result<&str> {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next().filter(|line| line.trim() == "---") else {
        return Ok(text);
    };
    let mut offset = first.len();
    for line in lines {
        offset += line.len();
        if line.trim() == "---" {
            return Ok(&text[offset..]);
        }
    }
    Err(ineligible())
}

#[cfg(target_os = "linux")]
fn is_local(file: &File) -> io::Result<bool> {
    let magic = rustix::fs::fstatfs(file)?.f_type as u32;
    // ext2/3/4, XFS, Btrfs, tmpfs, F2FS and ZFS. Stacked and unknown providers are excluded.
    Ok(matches!(
        magic,
        0xef53 | 0x58465342 | 0x9123683e | 0x01021994 | 0xf2f52010 | 0x2fc12fc1
    ))
}

#[cfg(target_os = "macos")]
fn is_local(file: &File) -> io::Result<bool> {
    let filesystem = rustix::fs::fstatfs(file)?;
    const MNT_LOCAL: u32 = 0x0000_1000;
    let name: Vec<_> = filesystem
        .f_fstypename
        .into_iter()
        .take_while(|&byte| byte != 0)
        .map(|byte| byte as u8)
        .collect();
    Ok(filesystem.f_flags & MNT_LOCAL != 0
        && matches!(name.as_slice(), b"apfs" | b"hfs" | b"msdos" | b"exfat"))
}

#[cfg(windows)]
fn is_local(file: &File) -> io::Result<bool> {
    kortty_files::is_local_reference_file(file)
}

fn ineligible() -> io::Error {
    io::Error::other("The AI memory reference is not an eligible local Markdown file")
}

fn changed() -> io::Error {
    io::Error::other("An approved AI memory reference changed. Review the request again.")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::ai_manager::memory::Scope;
    use crate::clipboard_policy::AiMemoryPolicy;
    use crate::snippets::tests::Profile;

    use super::super::{approve, change, review};
    use super::*;

    struct Fixture {
        _profile: Profile,
        home: PathBuf,
        memory: PathBuf,
        outside: PathBuf,
    }

    impl Fixture {
        fn new() -> io::Result<Self> {
            let profile = Profile::new();
            let root = profile.0.canonicalize()?;
            let home = root.join("home");
            let memory = root.join("memory");
            let outside = root.join("outside");
            for path in [&home, &memory, &outside] {
                Directory::open(path, true)?;
            }
            Ok(Self {
                _profile: profile,
                home,
                memory,
                outside,
            })
        }

        fn approve(&self, scope: &Scope, path: &Path, text: &str) -> io::Result<()> {
            fs::write(path, text)?;
            let policy = AiMemoryPolicy::default();
            approve(
                &self.memory,
                scope,
                registry(&self.memory)?,
                review(path.to_owned(), &policy)?,
                &policy,
            )?;
            Ok(())
        }

        fn load(&self, connection: Option<u64>) -> io::Result<Vec<ApprovedReference>> {
            load_with_home(&self.memory, connection, &self.home)
        }

        fn verify(&self, bindings: &[ReferenceBinding]) -> io::Result<()> {
            verify_in_home(&self.memory, bindings, Some(&self.home), &|_| true)
        }
    }

    fn local_fixture(path: &Path) -> io::Result<bool> {
        let local = is_local(&File::open(path)?)?;
        #[cfg(any(windows, target_os = "macos"))]
        assert!(
            local,
            "the native test directory must support local-file proof"
        );
        Ok(local)
    }

    #[test]
    fn approved_home_markdown_is_limited_to_global_and_the_exact_saved_scope() -> io::Result<()> {
        let fixture = Fixture::new()?;
        let global = Scope::global();
        let path = fixture.home.join("Global.md");
        fixture.approve(&global, &path, "Global preference.\n")?;
        fixture.approve(
            &global,
            &fixture.outside.join("external.md"),
            "Outside the home.\n",
        )?;
        fixture.approve(
            &global,
            &fixture.home.join("instructions.txt"),
            "Not Markdown.\n",
        )?;
        fixture.approve(
            &Scope::connection(42, "Selected".into()),
            &fixture.home.join("rules.MDC"),
            "---\ndescription: Configuration\n---\nUse the current branch.\n",
        )?;
        fixture.approve(
            &Scope::connection(7, "Other".into()),
            &fixture.home.join("other.md"),
            "Other connection.\n",
        )?;

        let selected = fixture.load(Some(42))?;
        if !local_fixture(&path)? {
            // Containers with a stacked filesystem still exercise production refusal.
            assert!(selected.is_empty());
            return Ok(());
        }
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].text.as_str(), "Global preference.\n");
        assert!(!selected[0].binding.is_connection());
        assert_eq!(selected[1].text.as_str(), "Use the current branch.\n");
        assert!(selected[1].binding.is_connection());
        assert_eq!(fixture.load(None)?.len(), 1);
        assert_eq!(fixture.load(Some(5))?.len(), 1);
        assert!(fixture.load(Some(0)).is_err());
        Ok(())
    }

    #[test]
    fn ineligible_legacy_paths_do_not_use_the_native_check_limit() -> io::Result<()> {
        let fixture = Fixture::new()?;
        let path = fixture.home.join("notes.md");
        fixture.approve(&Scope::global(), &path, "Use approved notes.\n")?;
        let mut approvals = registry(&fixture.memory)?;
        let valid = approvals.entries.pop().unwrap();
        for index in 0..MAX_CHECKS {
            let mut row = valid.clone();
            let (root, suffix) = if index % 2 == 0 {
                (&fixture.home, "txt")
            } else {
                (&fixture.outside, "md")
            };
            row.id = format!("legacy-{index}");
            row.root = root.to_str().unwrap().to_owned();
            row.path = root
                .join(format!("legacy-{index}.{suffix}"))
                .to_str()
                .unwrap()
                .to_owned();
            approvals.entries.push(row);
        }
        approvals.entries.push(valid);
        super::super::save(&fixture.memory, &approvals)?;

        let selected = fixture.load(None)?;
        if local_fixture(&path)? {
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].text.as_str(), "Use approved notes.\n");
        } else {
            assert!(selected.is_empty());
        }
        Ok(())
    }

    #[test]
    fn selected_reference_is_bound_to_raw_bytes_identity_and_active_registry_row() -> io::Result<()>
    {
        let fixture = Fixture::new()?;
        let scope = Scope::global();
        let path = fixture.home.join("notes.md");
        let text = "Keep local notes.\n";
        fixture.approve(&scope, &path, text)?;
        let approved = fixture.load(None)?;
        if !local_fixture(&path)? {
            assert!(approved.is_empty());
            return Ok(());
        }
        let binding = approved.into_iter().next().unwrap().binding;
        fixture.verify(std::slice::from_ref(&binding))?;
        fs::write(&path, "Keep other notes.\n")?;
        assert!(fixture.load(None)?.is_empty());
        assert!(fixture.verify(std::slice::from_ref(&binding)).is_err());

        fs::write(&path, text)?;
        let binding = fixture.load(None)?.pop().unwrap().binding;
        fs::rename(&path, fixture.home.join("old-notes.md"))?;
        fs::write(&path, text)?;
        assert!(fixture.verify(std::slice::from_ref(&binding)).is_err());

        let binding = fixture.load(None)?.pop().unwrap().binding;
        let id = binding.reference.id.clone();
        let current = registry(&fixture.memory)?;
        let policy = AiMemoryPolicy::default();
        let disabled = change(&fixture.memory, &scope, current, &id, Some(false), &policy)?;
        assert!(fixture.load(None)?.is_empty());
        assert!(fixture.verify(std::slice::from_ref(&binding)).is_err());
        let enabled = change(&fixture.memory, &scope, disabled, &id, Some(true), &policy)?;
        fixture.verify(std::slice::from_ref(&binding))?;
        change(&fixture.memory, &scope, enabled, &id, None, &policy)?;
        assert!(fixture.verify(&[binding]).is_err());
        Ok(())
    }

    #[test]
    fn managed_roots_filter_each_reference_before_read_and_again_at_send() -> io::Result<()> {
        let fixture = Fixture::new()?;
        let approved_root = fixture.home.join("managed");
        Directory::open(&approved_root, true)?;
        let denied = fixture.home.join("outside-managed-root.md");
        let allowed = approved_root.join("included.md");
        fixture.approve(&Scope::global(), &denied, "Keep other notes.\n")?;
        fixture.approve(&Scope::global(), &allowed, "Use approved notes.\n")?;
        let approved_root = approved_root.canonicalize()?;
        let allows = |candidate: &Path| candidate.starts_with(&approved_root);

        let selected = select(&fixture.memory, None, Some(&fixture.home), &allows)?;
        if !local_fixture(&allowed)? {
            assert!(selected.is_empty());
            return Ok(());
        }
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].text.as_str(), "Use approved notes.\n");
        let binding = &selected[0].binding;
        verify_in_home(
            &fixture.memory,
            std::slice::from_ref(binding),
            Some(&fixture.home),
            &allows,
        )?;
        let tightened = |candidate: &Path| candidate.starts_with(approved_root.join("new-scope"));
        assert!(
            verify_in_home(
                &fixture.memory,
                std::slice::from_ref(binding),
                Some(&fixture.home),
                &tightened
            )
            .is_err()
        );

        let home = Home::open(&fixture.home)?;
        let approvals = registry(&fixture.memory)?;
        let mut remaining = MAX_READ_BYTES;
        assert!(read_approved(&home, &approvals.entries[0], &mut remaining, &allows).is_err());
        assert_eq!(remaining, MAX_READ_BYTES);
        Ok(())
    }

    #[test]
    fn read_budget_limits_combined_approved_bytes() -> io::Result<()> {
        let fixture = Fixture::new()?;
        let path = fixture.home.join("notes.md");
        let text = "Use bounded context.\n";
        fixture.approve(&Scope::global(), &path, text)?;
        let home = Home::open(&fixture.home)?;
        let registry = registry(&fixture.memory)?;
        let entry = &registry.entries[0];
        let mut remaining = text.len() + 1;
        if !local_fixture(&path)? {
            assert!(read_approved(&home, entry, &mut remaining, &|_| true).is_err());
            return Ok(());
        }
        let (_, bytes) = read_approved(&home, entry, &mut remaining, &|_| true)?;
        assert_eq!(&*bytes, text.as_bytes());
        assert_eq!(remaining, 1);
        assert!(read_approved(&home, entry, &mut remaining, &|_| true).is_err());
        assert_eq!(remaining, 1);
        Ok(())
    }

    #[test]
    fn sanitized_whole_file_is_checked_before_mdc_metadata_or_excerpts_are_removed() {
        let path = Path::new("notes.mdc");
        let concealed = "---\npass\u{061c}word: Winter2026!\n---\nUse the current branch.\n";
        assert!(excerpt(concealed.as_bytes(), path).is_err());
        assert!(excerpt(b"---\nrule: no closing marker\n", path).is_err());
        let long = "é".repeat(MAX_EXCERPT_BYTES);
        assert_eq!(
            excerpt(long.as_bytes(), path).unwrap().len(),
            MAX_EXCERPT_BYTES
        );
        let with_tail = format!("{long}\npassword: Winter2026!\n");
        assert!(excerpt(with_tail.as_bytes(), path).is_err());
    }
}
