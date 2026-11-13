use std::fs;

use super::*;

pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new() -> Self {
        let mut nonce = [0_u8; 8];
        getrandom::fill(&mut nonce).unwrap();
        let directory = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "kortty-ssh-keys-ui-{:016x}",
            u64::from_ne_bytes(nonce)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt as _;
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&directory)
                .unwrap();
        }
        #[cfg(not(unix))]
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    pub(super) fn key_file(&self) -> PathBuf {
        let path = self.0.join("identity");
        // Test-only file content exercises the catalog copy, without invoking SSH auth.
        fs::write(&path, b"fixture identity bytes").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn ssh_keys_cancel_discards_added_key_and_staged_managed_copy() {
    let scratch = Scratch::new();
    let path = scratch.key_file();
    let mut workspace = Workspace::default();
    let (owner, _) = workspace.open_window(false);
    let manager_id = window::Id::unique();
    let mut manager = Manager::new(manager_id, Placement::Native { owner });
    manager.catalog = Some(Catalog::open(scratch.0.clone()).unwrap());
    manager.loading = false;
    workspace.ssh_keys.native = Some(manager);
    assert_eq!(workspace.ssh_keys_title(manager_id), Some(TITLE));
    assert!(workspace.ssh_keys_view(manager_id).is_some());

    let _ = workspace.update_ssh_keys(manager_id, Message::Add);
    let child = workspace.ssh_keys_for_owner(owner).unwrap();
    assert_ne!(child, manager_id);
    assert_eq!(workspace.ssh_keys_title(child), Some("Add SSH Key"));
    assert!(workspace.ssh_keys_view(child).is_some());
    let _ = workspace.update_ssh_keys(child, Message::Name("Build key".into()));
    let _ = workspace.update_ssh_keys(child, Message::Path(path.to_str().unwrap().into()));
    let _ = workspace.update_ssh_keys(child, Message::Accept);
    let _ = workspace.update_ssh_keys(manager_id, Message::Copy);
    let draft = workspace.ssh_keys.native.as_ref().unwrap();
    assert!(matches!(draft.selected().unwrap().path, PathEdit::Copy(_)));
    assert!(draft.dirty());
    assert!(!scratch.0.join("ssh-keys").exists());

    let _ = workspace.update_ssh_keys(manager_id, Message::Cancel);
    assert!(!workspace.ssh_keys.is_open());
    assert!(
        Catalog::open(scratch.0.clone())
            .unwrap()
            .records()
            .is_empty()
    );
    assert_eq!(fs::read(path).unwrap(), b"fixture identity bytes");
}

#[test]
fn hosted_ssh_keys_keeps_inline_draft_after_transfer_and_discards_only_on_confirmation() {
    let scratch = Scratch::new();
    let path = scratch.key_file();
    let catalog = Catalog::open(scratch.0.clone()).unwrap();
    let mut workspace = Workspace::default();
    workspace.window_settings.open_tool_windows_as_tabs = true;
    workspace.vault = Some(VaultWindow::open(scratch.0.join("vault.bin")).unwrap());
    let (source, _) = workspace.open_window(false);
    let (destination, _) = workspace.open_window(false);
    let _ = workspace.update_ssh_keys(source, Message::Open);
    let id = workspace
        .hosted_tool_in(source, tool_tabs::Kind::SshKeys)
        .unwrap();
    let tab = workspace.windows[&source].tabs[0].id();
    let _ = workspace.update(completed(id, Completed::Loaded(Ok((catalog, Vec::new())))));
    let _ = workspace.update_ssh_keys(id, Message::Add);
    let child = workspace.ssh_keys.hosted[&id].focused_window();
    assert_ne!(id, child);
    assert!(workspace.ssh_keys_title(child).is_none());
    assert!(workspace.hosted_ssh_keys_view(id).is_some());
    let _ = workspace.update_ssh_keys(child, Message::Name("Build key".into()));

    let _ = workspace.transfer(source, Some(destination), tab);
    assert_eq!(workspace.hosted_tool_owner(id), Some(destination));
    let _ = workspace.update_ssh_keys(child, Message::OpenVault);
    let vault = workspace.vault_id.unwrap();
    assert_eq!(
        workspace.hosted_ssh_keys_vault_for_owner(destination),
        Some(vault)
    );
    assert_eq!(workspace.hosted_ssh_keys_vault_for_owner(source), None);
    let _ = workspace.update_ssh_keys(child, Message::Name("Blocked under Vault".into()));
    let _ = workspace.close_profile_tool(vault, false);
    assert!(workspace.vault_id.is_none());
    let _ = workspace.update_ssh_keys(child, Message::Path(path.to_str().unwrap().into()));
    let _ = workspace.update_ssh_keys(child, Message::Accept);
    let _ = workspace.update_ssh_keys(id, Message::Copy);
    let draft = workspace.ssh_keys.hosted[&id].selected().unwrap();
    assert_eq!(draft.name, "Build key");
    assert!(matches!(draft.path, PathEdit::Copy(_)));
    assert!(!scratch.0.join("ssh-keys").exists());

    let _ = workspace.update_ssh_keys(id, Message::Delete);
    let deletion = workspace.ssh_keys.hosted[&id].focused_window();
    assert!(workspace.ssh_keys_title(deletion).is_none());
    let _ = workspace.dismiss_hosted_ssh_keys(id);
    assert!(workspace.ssh_keys.hosted[&id].child.is_none());
    let _ = workspace.request_hosted_tool_close(id);
    let token = workspace.ssh_keys.hosted[&id].hosted_close.unwrap();
    let _ = workspace.update_ssh_keys(id, Message::HostedClose(token.wrapping_add(1), true));
    assert!(workspace.hosted_ssh_keys_dirty(id));
    let _ = workspace.update_ssh_keys(id, Message::HostedClose(token, false));
    assert!(workspace.hosted_ssh_keys_dirty(id));

    let _ = workspace.request_hosted_tool_close(id);
    let token = workspace.ssh_keys.hosted[&id].hosted_close.unwrap();
    let _ = workspace.update_ssh_keys(id, Message::HostedClose(token, true));
    assert!(workspace.hosted_tool_owner(id).is_none());
    assert!(!workspace.ssh_keys.is_open());
    assert!(
        Catalog::open(scratch.0.clone())
            .unwrap()
            .records()
            .is_empty()
    );
    assert_eq!(fs::read(path).unwrap(), b"fixture identity bytes");
}

#[test]
fn ssh_keys_search_matches_name_or_path_and_anchors_globs() {
    let key = Key {
        id: RowId::Saved(3),
        name: "Build [production]".into(),
        description: "Do not search the description".into(),
        path: PathEdit::External(PathBuf::from("/tmp/identities/deploy+test")),
        passphrase: None,
        new_passphrase: Secret::default(),
        dirty: false,
    };
    assert!(Filter::new("  PRODUCTION ".into()).matches(&key));
    assert!(Filter::new("deploy+".into()).matches(&key));
    assert!(Filter::new("build*[production]".into()).matches(&key));
    assert!(Filter::new("*/deploy+*".into()).matches(&key));
    assert!(!Filter::new("deploy*".into()).matches(&key));
    assert!(!Filter::new("description".into()).matches(&key));
}
