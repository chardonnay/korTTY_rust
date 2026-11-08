use super::{
    Attributes, Cancellation, Error, Failure, FileKind, FolderRemains, MAX_FILE_BYTES, Read,
    RemotePath, Sftp, Write, before_change, checked_size, copy_remote_file, private_directory,
    published_detail, same_owner, verify_entry, verify_new_destination, verify_unchanged,
};

const MAX_ENTRIES: usize = 4096;
const MAX_PATH_BYTES: usize = 4 << 20;
const MAX_LISTING_BYTES: usize = 2 << 20;
const PLAN_LIMIT: &str = "The scheduled remote copy exceeds its folder entry or path budget";

pub(super) fn run(
    client: &mut Sftp<impl Read, impl Write>,
    from: &RemotePath,
    to: &RemotePath,
    attrs: Attributes,
    cancellation: &Cancellation,
    remains: &mut Option<FolderRemains>,
) -> Result<String, Failure> {
    verify_new_destination(client, to)?;
    let mut plan = Plan {
        reserved: 1,
        ..Plan::default()
    };
    let root = Entry {
        from: from.clone(),
        to: to.clone(),
        attrs,
    };
    plan.inspect(client, root, cancellation)?;

    verify_unchanged(client, from, attrs)?;
    verify_new_destination(client, to)?;
    before_change(cancellation)?;
    match client.mkdir_private(to.as_str()) {
        Ok(()) => *remains = Some(FolderRemains::RemoteCopyPartial),
        Err(error) => {
            if error.connection_lost() {
                *remains = Some(FolderRemains::RemoteCopyPartial);
            }
            return Err(Failure::failed("Could not create the new remote folder"));
        }
    }
    let owner = private_directory(client, to.as_str())?;
    let mut cleaned = true;
    for entry in &plan.entries[1..] {
        before_change(cancellation)?;
        verify_unchanged(client, &entry.from, entry.attrs)?;
        check_owner(client, entry.to.parent(), owner)?;
        if entry.attrs.kind() == FileKind::Directory {
            verify_new_destination(client, &entry.to)?;
            before_change(cancellation)?;
            client
                .mkdir_private(entry.to.as_str())
                .map_err(|_| Failure::failed("Could not create a new remote subfolder"))?;
            check_owner(client, entry.to.as_str(), owner)?;
        } else {
            cleaned &= copy_remote_file(
                client,
                &entry.from,
                &entry.to,
                entry.attrs,
                checked_size(entry.attrs.size)?,
                cancellation,
            )?;
        }
    }
    for entry in &plan.entries {
        before_change(cancellation)?;
        verify_unchanged(client, &entry.from, entry.attrs)?;
        if entry.attrs.kind() == FileKind::Directory {
            check_owner(client, entry.to.as_str(), owner)?;
        }
    }
    *remains = None;
    Ok(published_detail(
        format!(
            "Copied {} folders, {} files and {} bytes on the server via SFTP",
            plan.entries.len() - plan.files,
            plan.files,
            plan.bytes,
        ),
        cleaned,
    ))
}

fn check_owner(
    client: &mut Sftp<impl Read, impl Write>,
    path: &str,
    owner: Attributes,
) -> Result<(), Failure> {
    if !same_owner(private_directory(client, path)?, owner) {
        return Err(Failure::failed("The remote copy destination changed"));
    }
    Ok(())
}

#[derive(Default)]
struct Plan {
    entries: Vec<Entry>,
    reserved: usize,
    path_bytes: usize,
    files: usize,
    bytes: u64,
}

struct Entry {
    from: RemotePath,
    to: RemotePath,
    attrs: Attributes,
}

impl Plan {
    fn inspect(
        &mut self,
        client: &mut Sftp<impl Read, impl Write>,
        entry: Entry,
        cancellation: &Cancellation,
    ) -> Result<(), Failure> {
        before_change(cancellation)?;
        self.path_bytes += entry.from.as_str().len() + entry.to.as_str().len();
        if self.path_bytes > MAX_PATH_BYTES {
            return Err(Failure::blocked(PLAN_LIMIT));
        }
        if entry.attrs.modified.is_none() {
            return Err(Failure::failed(
                "The server did not report a modification time for a remote copy source",
            ));
        }
        if entry.attrs.kind() == FileKind::File {
            let size = checked_size(entry.attrs.size)?;
            if size > MAX_FILE_BYTES - self.bytes {
                return Err(Failure::blocked(
                    "A scheduled remote folder copy supports up to 1 GiB of file data",
                ));
            }
            self.bytes += size;
            self.files += 1;
            self.entries.push(entry);
            return Ok(());
        }

        let available = MAX_ENTRIES - self.reserved;
        // The SFTP response budget includes the optional dot entries.
        let mut children = client
            .read_dir_limited(entry.from.as_str(), available + 2, MAX_LISTING_BYTES)
            .map_err(|error| match error {
                Error::Limit(_) => Failure::blocked(PLAN_LIMIT),
                _ => Failure::failed("Could not list a remote copy source folder"),
            })?;
        if children.len() > available {
            return Err(Failure::blocked(PLAN_LIMIT));
        }
        children.sort_unstable_by(|left, right| left.name.cmp(&right.name));
        if children.windows(2).any(|pair| pair[0].name == pair[1].name) {
            return Err(Failure::failed(
                "The server listed duplicate remote copy names",
            ));
        }
        self.reserved += children.len();
        let index = self.entries.len();
        self.entries.push(entry);
        for child in children {
            before_change(cancellation)?;
            let parent = &self.entries[index];
            let from = parent.from.child(&child.name).map_err(Failure::blocked)?;
            let to = parent.to.child(&child.name).map_err(Failure::blocked)?;
            let attrs = verify_entry(client, from.as_str())?;
            self.inspect(client, Entry { from, to, attrs }, cancellation)?;
        }
        Ok(())
    }
}
