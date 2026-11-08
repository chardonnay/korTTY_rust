use std::collections::HashSet;

use kortty_vault::{CredentialInput, CredentialKind};

use super::super::environment_access::DEFAULT_ENVIRONMENT;
use super::*;

struct Pending {
    id: RowId,
    key: KeyDraft,
    secret: Secret,
}

impl Workspace {
    pub(super) fn save_ssh_keys(&mut self, id: window::Id) -> Task<Desktop> {
        let Some(manager) = self
            .ssh_keys
            .instance_mut(id)
            .filter(|manager| manager.ready() && manager.hosted_close.is_none())
        else {
            return Task::none();
        };
        if !manager.dirty() {
            return self.finish_ssh_keys_manager(id);
        }
        let Some(catalog) = manager.catalog.take() else {
            return Task::none();
        };
        let needs_vault = manager
            .entries
            .iter()
            .any(|key| key.dirty && !key.new_passphrase.is_empty());
        let vault = if needs_vault {
            let Some(vault) = self.vault.as_mut().and_then(VaultWindow::take_for_ssh_keys) else {
                manager.catalog = Some(catalog);
                manager.status =
                    Some("Unlock the vault before saving the new SSH key passphrase.".into());
                return Task::none();
            };
            Some(vault)
        } else {
            None
        };
        let pending = manager
            .entries
            .iter()
            .filter(|key| key.dirty)
            .map(|key| Pending {
                id: key.id,
                key: KeyDraft {
                    name: key.name.clone(),
                    description: key.description.clone(),
                    input: key.path.input(),
                    passphrase: key.passphrase.clone(),
                },
                secret: key.new_passphrase.clone(),
            })
            .collect();
        let removed = manager.removed.clone();
        manager.saving = true;
        manager.status = None;
        Task::future(async move { completed(id, commit(catalog, vault, pending, removed)) })
    }

    pub(super) fn finish_ssh_keys(&mut self, id: window::Id, reply: Reply) -> Task<Desktop> {
        let Some(reply) = reply
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        else {
            return Task::none();
        };
        match reply {
            Completed::Loaded(result) => {
                if let Some(manager) = self
                    .ssh_keys
                    .instance_mut(id)
                    .filter(|manager| manager.loading)
                {
                    manager.loading = false;
                    match result {
                        Ok((catalog, entries)) => {
                            manager.catalog = Some(catalog);
                            manager.entries = entries;
                        }
                        Err(error) => {
                            manager.status = Some(format!("Could not load SSH keys ({error})."))
                        }
                    }
                    if manager.hosted() {
                        return self.resume_hosted_tool_close(id);
                    }
                }
            }
            Completed::Saved {
                catalog,
                vault,
                result,
            } => {
                if let Some(vault) = vault
                    && let Some(window) = &mut self.vault
                {
                    window.restore_after_ssh_keys(vault);
                }
                let Some(manager) = self
                    .ssh_keys
                    .instance_mut(id)
                    .filter(|manager| manager.saving)
                else {
                    return Task::none();
                };
                manager.saving = false;
                match result {
                    Ok(report) => {
                        let native_owner = manager.native_owner();
                        let copied = manager
                            .entries
                            .iter()
                            .any(|key| matches!(key.path, PathEdit::Copy(_)));
                        let closing = manager.hosted_close.is_some();
                        if manager.hosted() {
                            // A pending window or batch close may later be canceled.
                            // Keep the committed catalog and clear all staged secrets.
                            manager.entries = catalog_entries(&catalog);
                            manager.catalog = Some(catalog);
                            manager.removed.clear();
                            manager.reset_selection();
                        }
                        let owner = native_owner.or_else(|| self.hosted_tool_owner(id));
                        self.refresh_ssh_key_choices();
                        if let Some(owner) = owner {
                            self.show_ssh_keys_save_notice(owner, report, copied);
                        }
                        return if closing {
                            self.resume_hosted_tool_close(id)
                        } else {
                            self.finish_ssh_keys_manager(id)
                        };
                    }
                    Err(error) => {
                        manager.catalog = Some(catalog);
                        manager.status = Some(format!("Could not save SSH keys ({error})."));
                        if manager.hosted() {
                            return self.resume_hosted_tool_close(id);
                        }
                    }
                }
            }
        }
        Task::none()
    }

    fn show_ssh_keys_save_notice(&mut self, owner: window::Id, report: CommitReport, copied: bool) {
        if report.durability_warning || report.retained_copies > 0 {
            let mut message = String::from("SSH keys saved.");
            if report.durability_warning {
                message.push_str(" Disk synchronization could not be confirmed.");
            }
            if report.retained_copies > 0 {
                message.push_str(&format!(
                    " {} obsolete managed key file(s) were retained.",
                    report.retained_copies,
                ));
            }
            self.notice(owner, message);
        } else if copied {
            self.notice(owner, "SSH key was copied to user directory.".into());
        } else if !report.added.is_empty() {
            self.notice(owner, format!("{} SSH key(s) added.", report.added.len()));
        }
    }
}

fn commit(
    mut catalog: Catalog,
    mut vault: Option<Vault>,
    pending: Vec<Pending>,
    removed: Vec<u64>,
) -> Completed {
    let original = vault.as_ref().map(|vault| {
        vault
            .entries()
            .map(|entry| entry.id.clone())
            .collect::<HashSet<_>>()
    });
    let result = edits(&mut vault, pending, removed)
        .and_then(|edits| catalog.apply(edits).map_err(|error| error.to_string()));
    let result = match result {
        Err(mut problem) => {
            if let (Some(vault), Some(original)) = (&mut vault, original) {
                // Vault::save can report a directory sync failure after publication.
                // The checked-out vault still identifies precisely this transaction's additions.
                let created = vault
                    .entries()
                    .filter(|entry| !original.contains(&entry.id))
                    .map(|entry| entry.id.clone())
                    .collect::<Vec<_>>();
                let failed = created
                    .iter()
                    .filter(|id| vault.delete(id).is_err())
                    .count();
                if failed != 0 {
                    problem.push_str(&format!(
                        ". Could not confirm removal of {failed} newly created vault passphrase(s)"
                    ));
                }
            }
            Err(problem)
        }
        success => success,
    };
    Completed::Saved {
        catalog,
        vault,
        result,
    }
}

fn edits(
    vault: &mut Option<Vault>,
    pending: Vec<Pending>,
    removed: Vec<u64>,
) -> Result<Vec<Edit>, String> {
    let mut edits = removed.into_iter().map(Edit::Remove).collect::<Vec<_>>();
    for Pending {
        id,
        mut key,
        secret,
    } in pending
    {
        if !secret.is_empty() {
            let vault = vault.as_mut().ok_or("The vault is locked")?;
            let input = CredentialInput {
                label: key.name.clone(),
                environment: DEFAULT_ENVIRONMENT.into(),
                kind: CredentialKind::KeyPassphrase,
                ..CredentialInput::default()
            };
            key.passphrase = Some(
                vault
                    .save(input, secret)
                    .map_err(|error| error.to_string())?,
            );
        }
        edits.push(match id {
            RowId::Saved(id) => Edit::Update(id, key),
            RowId::New(_) => Edit::Add(key),
        });
    }
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::super::tests::Scratch;
    use super::*;

    #[test]
    fn ssh_keys_commit_encrypts_passphrase_and_compensates_only_new_ids_on_stale_catalog() {
        let scratch = Scratch::new();
        let source = scratch.key_file();
        let catalog = Catalog::open(scratch.0.clone()).unwrap();
        let vault_path = scratch.0.join("vault.bin");
        let vault = Vault::create(&vault_path, b"disposable master password").unwrap();
        let first = Pending {
            id: RowId::New(1),
            key: KeyDraft {
                name: "Build key".into(),
                description: "Fixture description".into(),
                input: KeyInput::Copy(source.clone()),
                passphrase: None,
            },
            secret: Secret::new("disposable key passphrase".into()),
        };
        let Completed::Saved {
            catalog,
            vault: Some(vault),
            result,
        } = commit(catalog, Some(vault), vec![first], Vec::new())
        else {
            panic!("expected a committed catalog and returned vault");
        };
        let report = result.unwrap();
        let key = catalog.get(report.added[0]).unwrap();
        let original = key.passphrase.clone().unwrap();
        assert!(matches!(key.source, KeySource::Managed(_)));
        assert_eq!(vault.secret(&original), Some("disposable key passphrase"));
        assert_eq!(
            catalog.read_key(key).unwrap().as_slice(),
            b"fixture identity bytes"
        );

        let mut concurrent = Catalog::open(scratch.0.clone()).unwrap();
        let draft = KeyDraft {
            name: "Another window".into(),
            description: String::new(),
            input: KeyInput::External(source),
            passphrase: None,
        };
        concurrent.apply(vec![Edit::Add(draft.clone())]).unwrap();
        let second = Pending {
            id: RowId::New(2),
            key: draft,
            secret: Secret::new("must be compensated".into()),
        };
        let Completed::Saved {
            vault: Some(vault),
            result,
            ..
        } = commit(catalog, Some(vault), vec![second], Vec::new())
        else {
            panic!("expected the unlocked vault after a failed commit");
        };
        assert!(result.is_err());
        assert_eq!(
            vault
                .entries()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            [original]
        );
        assert_eq!(Catalog::open(scratch.0.clone()).unwrap().records().len(), 2);
    }
}
