use std::collections::{BTreeMap, BTreeSet};
use std::io;

use super::super::{ActiveAuth, AuthMethod, Document};
use super::{
    Auth, ImportedProfile, LoginTarget, MAX_BYTES, Preview, Store, invalid, options, same_profile,
    valid_teamwork_id, xml,
};
use crate::clipboard_policy::ServerAccess;

pub(crate) struct JavaPasswordBinding<'a> {
    pub(crate) source_id: &'a str,
    pub(crate) target: &'a LoginTarget,
    pub(crate) jump: Option<&'a LoginTarget>,
    pub(crate) for_jump: bool,
    pub(crate) credential: usize,
}

pub(crate) struct JavaManagedKeyBinding<'a> {
    pub(crate) source_key_id: &'a str,
    pub(crate) key: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JavaImportedRoute {
    pub(crate) id: u64,
    pub(crate) target: LoginTarget,
    pub(crate) jump: Option<LoginTarget>,
}

pub(crate) struct JavaConnectionImport {
    pub(crate) contents: Option<Vec<u8>>,
    pub(crate) routes: BTreeMap<String, JavaImportedRoute>,
}

enum Credential {
    Password(usize),
    ManagedKey(usize),
}

/// A connection import bound to the native catalogue reviewed before sealing.
pub(crate) struct JavaConnectionBindings {
    local: Store,
    imports: Preview,
    slots: BTreeMap<(usize, bool), Credential>,
    omitted: usize,
    omitted_keys: usize,
}

impl JavaConnectionBindings {
    pub(crate) fn review(
        source: Option<&[u8]>,
        current: Option<&[u8]>,
        password_bindings: &[JavaPasswordBinding<'_>],
        managed_keys: &[JavaManagedKeyBinding<'_>],
        policy: &ServerAccess,
    ) -> io::Result<Self> {
        let local = local_store(current)?;
        let imports = source
            .map(|source| binding_source(source, true))
            .transpose()?
            .unwrap_or_default();
        let entries: BTreeMap<_, _> = imports
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.java_id.as_deref().map(|id| (id, (index, entry))))
            .collect();
        let mut slots = BTreeMap::new();
        let mut omitted = 0;
        for binding in password_bindings {
            let slot = entries.get(binding.source_id).filter(|(_, entry)| {
                entry.java_route.as_ref().is_some_and(|(target, jump)| {
                    target == binding.target && jump.as_ref() == binding.jump
                }) && eligible(entry, &local.document, policy)
                    && if binding.for_jump {
                        entry
                            .connection
                            .jump
                            .as_ref()
                            .is_some_and(|login| matches!(login.auth, Auth::Prompt))
                    } else {
                        matches!(entry.connection.login.auth, Auth::Prompt)
                    }
            });
            if let Some((index, _)) = slot {
                if slots
                    .insert(
                        (*index, binding.for_jump),
                        Credential::Password(binding.credential),
                    )
                    .is_some()
                {
                    return Err(invalid("Ambiguous Java connection password binding."));
                }
            } else {
                omitted += 1;
            }
        }
        let mut keys = BTreeMap::new();
        for key in managed_keys {
            if !valid_teamwork_id(key.source_key_id)
                || keys.insert(key.source_key_id, key.key).is_some()
            {
                return Err(invalid("Ambiguous Java managed SSH key binding."));
            }
        }
        let mut bound_keys = 0;
        for (index, entry) in imports.entries.iter().enumerate() {
            let Some(key) = entry.java_key_id.as_deref().and_then(|id| keys.get(id)) else {
                continue;
            };
            if !matches!(
                entry.connection.login.auth,
                Auth::Configured {
                    method: AuthMethod::PublicKey,
                    credential: None,
                    key: None,
                }
            ) || !eligible(entry, &local.document, policy)
            {
                continue;
            }
            if slots
                .insert((index, false), Credential::ManagedKey(*key))
                .is_some()
            {
                return Err(invalid("Ambiguous Java connection authentication binding."));
            }
            bound_keys += 1;
        }
        let omitted_keys = imports.java_key_references - bound_keys;
        Ok(Self {
            local,
            imports,
            slots,
            omitted,
            omitted_keys,
        })
    }

    pub(crate) fn needed(&self) -> BTreeSet<usize> {
        self.slots
            .values()
            .filter_map(|credential| match credential {
                Credential::Password(index) => Some(*index),
                Credential::ManagedKey(_) => None,
            })
            .collect()
    }

    pub(crate) fn omitted(&self) -> usize {
        self.omitted
    }

    pub(crate) fn omitted_keys(&self) -> usize {
        self.omitted_keys
    }

    pub(crate) fn bind(
        self,
        password_ids: &BTreeMap<usize, String>,
        key_ids: &BTreeMap<usize, u64>,
    ) -> io::Result<Option<Vec<u8>>> {
        if self.slots.is_empty() {
            return Ok(None);
        }
        self.bind_with_routes(password_ids, key_ids, &ServerAccess::default())
            .map(|prepared| prepared.contents)
    }

    pub(crate) fn bind_with_routes(
        mut self,
        password_ids: &BTreeMap<usize, String>,
        key_ids: &BTreeMap<usize, u64>,
        policy: &ServerAccess,
    ) -> io::Result<JavaConnectionImport> {
        for ((index, for_jump), credential) in self.slots {
            let connection = &mut self.imports.entries[index].connection;
            let login = if for_jump {
                connection
                    .jump
                    .as_mut()
                    .ok_or_else(|| invalid("Java jump host is unavailable."))?
            } else {
                &mut connection.login
            };
            login.auth = match credential {
                Credential::Password(index) => Auth::VaultPassword(
                    password_ids
                        .get(&index)
                        .ok_or_else(|| invalid("Java password was not sealed."))?
                        .clone(),
                ),
                Credential::ManagedKey(index) => Auth::StoredKey(
                    *key_ids
                        .get(&index)
                        .ok_or_else(|| invalid("Java managed SSH key was not staged."))?,
                ),
            };
        }
        stage_import(&self.local, &self.imports, policy)
    }
}

pub(crate) fn prepare_java_connection_import(
    source: &[u8],
    current: Option<&[u8]>,
    policy: &ServerAccess,
) -> io::Result<JavaConnectionImport> {
    let imports = binding_source(source, false)?;
    let local = local_store(current)?;
    stage_import(&local, &imports, policy)
}

fn binding_source(source: &[u8], strict_ids: bool) -> io::Result<Preview> {
    if source.len() as u64 > MAX_BYTES {
        return Err(invalid("connection import exceeds its size limit"));
    }
    xml::parse_java_binding_source(source, strict_ids)
}

fn local_store(current: Option<&[u8]>) -> io::Result<Store> {
    Ok(Store {
        path: Default::default(),
        document: current
            .map(super::super::decode_document)
            .transpose()?
            .unwrap_or_default(),
    })
}

fn stage_import(
    local: &Store,
    imports: &Preview,
    policy: &ServerAccess,
) -> io::Result<JavaConnectionImport> {
    let mut routes = BTreeMap::new();
    let (candidate, outcome) = options::stage_with_insertions(
        local,
        &imports.entries,
        &imports.local_entries,
        &imports.serial_entries,
        |entry, connection| {
            if let Some(source) = &entry.java_id
                && let Some((target, jump)) = &entry.java_route
                && connection.matches_route(target, jump.as_ref())
                && connection.blocked_server(policy).is_none()
            {
                routes.insert(
                    source.clone(),
                    JavaImportedRoute {
                        id: connection.id,
                        target: target.clone(),
                        jump: jump.clone(),
                    },
                );
            }
        },
    )?;
    let contents = (outcome.imported > 0)
        .then(|| options::encode(&candidate))
        .transpose()?;
    Ok(JavaConnectionImport { contents, routes })
}

fn eligible(entry: &ImportedProfile, local: &Document, policy: &ServerAccess) -> bool {
    let connection = &entry.connection;
    connection.missing_username().is_none()
        && connection.blocked_server(policy).is_none()
        && !local.connections.iter().any(|saved| {
            let group = local
                .library
                .membership
                .get(&saved.id)
                .map_or("", String::as_str);
            same_profile(saved, group, connection, &entry.group)
        })
}

/// Rechecks the exact candidate destinations referenced by migrated Java jobs.
pub(crate) fn check_java_imported_routes(
    candidate: &[u8],
    expected: &[JavaImportedRoute],
    policy: &ServerAccess,
) -> io::Result<()> {
    let candidate = super::super::decode_document(candidate)?;
    let by_id: BTreeMap<_, _> = candidate
        .connections
        .iter()
        .map(|connection| (connection.id, connection))
        .collect();
    for route in expected {
        let connection = by_id
            .get(&route.id)
            .filter(|connection| {
                connection.missing_username().is_none()
                    && connection.matches_route(&route.target, route.jump.as_ref())
            })
            .ok_or_else(|| invalid("A reviewed Java Scheduler connection route changed."))?;
        if connection.blocked_server(policy).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Managed policy blocks a Java Scheduler connection route.",
            ));
        }
    }
    Ok(())
}

/// Rechecks only newly bound routes against the current server policy at startup.
pub(crate) fn check_java_connection_routes(
    candidate: &[u8],
    credential_ids: &[String],
    key_ids: &[u64],
    policy: &ServerAccess,
) -> io::Result<()> {
    let candidate = super::super::decode_document(candidate)?;
    let passwords: BTreeSet<_> = credential_ids.iter().map(String::as_str).collect();
    let keys: BTreeSet<_> = key_ids.iter().copied().collect();
    let bound = |auth: &Auth| match auth.active() {
        ActiveAuth::VaultPassword(id) => passwords.contains(id),
        ActiveAuth::StoredKey(id) => keys.contains(&id),
        _ => false,
    };
    if candidate.connections.iter().any(|connection| {
        (bound(&connection.login.auth)
            || connection
                .jump
                .as_ref()
                .is_some_and(|jump| bound(&jump.auth)))
            && connection.blocked_server(policy).is_some()
    }) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Managed policy blocks a Java credential's connection route.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "d7d49957-2a89-42d0-a96e-857d86066775";
    const PASSWORD: &str = "3cd2651e-2dfd-4d72-b16f-f45ba1fdcd13";

    fn endpoint(host: &str, username: &str) -> LoginTarget {
        LoginTarget {
            host: host.into(),
            port: 22,
            username: username.into(),
        }
    }

    fn managed() -> [JavaManagedKeyBinding<'static>; 1] {
        [JavaManagedKeyBinding {
            source_key_id: KEY,
            key: 5,
        }]
    }

    #[test]
    fn java_bindings_combine_main_key_and_jump_password_with_active_method_precedence() {
        let source = format!(
            "<connections>
                <connection id='key-route'>
                    <name>Key route</name><host>main.example</host><username>alice</username>
                    <authMethod>PUBLIC_KEY</authMethod><sshKeyId>{KEY}</sshKeyId>
                    <credentialId>inactive-password</credentialId>
                    <jumpServer><enabled>true</enabled><host>jump.example</host>
                        <username>jump</username><authMethod>PASSWORD</authMethod></jumpServer>
                </connection>
                <connection id='password-route'>
                    <name>Password route</name><host>password.example</host><username>bob</username>
                    <authMethod>PASSWORD</authMethod><sshKeyId>{KEY}</sshKeyId>
                </connection>
            </connections>"
        );
        let target = endpoint("main.example", "alice");
        let jump = endpoint("jump.example", "jump");
        let password_target = endpoint("password.example", "bob");
        let review = JavaConnectionBindings::review(
            Some(source.as_bytes()),
            None,
            &[
                JavaPasswordBinding {
                    source_id: "key-route",
                    target: &target,
                    jump: Some(&jump),
                    for_jump: true,
                    credential: 1,
                },
                JavaPasswordBinding {
                    source_id: "password-route",
                    target: &password_target,
                    jump: None,
                    for_jump: false,
                    credential: 2,
                },
            ],
            &managed(),
            &ServerAccess::default(),
        )
        .unwrap();
        assert_eq!(review.needed(), BTreeSet::from([1, 2]));
        assert_eq!(review.omitted(), 0);
        assert_eq!(review.omitted_keys(), 0);
        let prepared = review
            .bind_with_routes(
                &BTreeMap::from([(1, PASSWORD.into()), (2, PASSWORD.into())]),
                &BTreeMap::from([(5, 17)]),
                &ServerAccess::default(),
            )
            .unwrap();
        let bytes = prepared.contents.unwrap();
        let document = super::super::super::decode_document(&bytes).unwrap();
        assert_eq!(prepared.routes.len(), 2);
        assert_eq!(
            prepared.routes["key-route"],
            JavaImportedRoute {
                id: document.connections[0].id,
                target,
                jump: Some(jump),
            },
        );
        assert_eq!(
            prepared.routes["password-route"],
            JavaImportedRoute {
                id: document.connections[1].id,
                target: password_target,
                jump: None,
            },
        );
        assert_eq!(document.connections[0].login.auth, Auth::StoredKey(17));
        assert_eq!(
            document.connections[0].jump.as_ref().unwrap().auth,
            Auth::VaultPassword(PASSWORD.into()),
        );
        assert_eq!(
            document.connections[1].login.auth,
            Auth::VaultPassword(PASSWORD.into()),
        );
        let blocked_jump = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['jump.example']",
        )
        .unwrap();
        for (passwords, keys) in [(&[PASSWORD.to_owned()][..], &[][..]), (&[][..], &[17][..])] {
            assert_eq!(
                check_java_connection_routes(&bytes, passwords, keys, blocked_jump.server_access())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied,
            );
        }
        check_java_connection_routes(&bytes, &[], &[], blocked_jump.server_access()).unwrap();
    }

    #[test]
    fn java_bindings_reject_ambiguous_ids_before_route_deduplication() {
        let source = format!(
            "<connections>
                <connection id='duplicate'><name>Route</name><host>main.example</host>
                    <username>alice</username><authMethod>PUBLIC_KEY</authMethod>
                    <sshKeyId>{KEY}</sshKeyId></connection>
                <connection id='duplicate'><name>Route</name><host>main.example</host>
                    <username>alice</username><authMethod>PUBLIC_KEY</authMethod>
                    <sshKeyId>{KEY}</sshKeyId></connection>
            </connections>"
        );
        assert!(
            JavaConnectionBindings::review(
                Some(source.as_bytes()),
                None,
                &[],
                &managed(),
                &ServerAccess::default(),
            )
            .is_err()
        );
        assert_eq!(
            Preview::java_backup(source.as_bytes()).unwrap().duplicates,
            1
        );
    }

    #[test]
    fn java_bindings_omit_unsafe_or_unavailable_managed_references() {
        let route = |id: &str, host: &str, reference: &str| {
            format!(
                "<connection {id}><name>{host}</name><host>{host}</host><username>alice</username>
                    <authMethod>PUBLIC_KEY</authMethod>{reference}</connection>"
            )
        };
        let reference = format!("<sshKeyId>{KEY}</sshKeyId>");
        let routes = [
            route("id='unknown'", "unknown.example", "<sshKeyId>17</sshKeyId>"),
            route(
                "id='foreign'",
                "foreign.example",
                &format!("{reference}<connectionSource>TEAMWORK</connectionSource>"),
            ),
            route(
                "id='temporary-path'",
                "marker.example",
                &format!("{reference}<privateKeyPath>TEMPORARY:key</privateKeyPath>"),
            ),
            route(
                "id='temporary-content'",
                "content.example",
                &format!("{reference}<temporaryKeyContent>redacted-fixture</temporaryKeyContent>"),
            ),
            route(
                "id='malformed'",
                "malformed.example",
                &format!("{reference}{reference}"),
            ),
            route("", "anonymous.example", &reference),
            route("id='collision'", "collision.example", &reference),
        ];
        let source = format!("<connections>{}</connections>", routes.concat());
        let local_source = b"<connections><connection><name>collision.example</name><host>collision.example</host><username>alice</username></connection></connections>";
        let (local, _) = Preview::java_backup(local_source)
            .unwrap()
            .portable_java_backup()
            .unwrap();
        let review = JavaConnectionBindings::review(
            Some(source.as_bytes()),
            Some(&local),
            &[],
            &managed(),
            &ServerAccess::default(),
        )
        .unwrap();
        assert_eq!(review.omitted_keys(), routes.len());
        assert!(
            review
                .bind(&BTreeMap::new(), &BTreeMap::from([(5, 17)]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn plain_java_lineage_tracks_only_new_exact_unambiguous_routes() {
        let local = br#"<connections>
            <connection><name>Collision</name><host>same.example</host><username>alice</username>
                <jumpServer><enabled>true</enabled><host>old-jump.example</host><username>jump</username></jumpServer>
            </connection>
        </connections>"#;
        let source = br#"<connections>
            <connection id="preserve-case"><name>New</name><host>MAIN.Example</host><username>alice</username>
                <jumpServer><enabled>true</enabled><host>JUMP.Example</host><port>2222</port><username>jump</username></jumpServer>
            </connection>
            <connection id="collision"><name>Collision</name><host>same.example</host><username>alice</username>
                <jumpServer><enabled>true</enabled><host>new-jump.example</host><username>jump</username></jumpServer>
            </connection>
            <connection id="dup"><name>One</name><host>one.example</host><username>alice</username></connection>
            <connection id="dup"><name>Two</name><host>two.example</host><username>alice</username></connection>
            <connection id="first"><name>Ambiguous</name><host>dupe.example</host><username>alice</username></connection>
            <connection id="second"><name>Ambiguous</name><host>dupe.example</host><username>alice</username>
                <jumpServer><enabled>true</enabled><host>another.example</host><username>jump</username></jumpServer>
            </connection>
            <connection id="foreign"><name>Foreign</name><host>foreign.example</host><username>alice</username>
                <connectionSource>TEAMWORK</connectionSource>
            </connection>
            <connection id="temp"><name>Temporary</name><host>temp.example</host><username>alice</username>
                <jumpServer><enabled>true</enabled><host>temp-jump.example</host><username>jump</username>
                    <privateKeyPath>TEMPORARY:key</privateKeyPath>
                </jumpServer>
            </connection>
            <connection id="blocked"><name>Blocked</name><host>blocked.example</host><username>alice</username></connection>
        </connections>"#;
        let local = prepare_java_connection_import(local, None, &ServerAccess::default())
            .unwrap()
            .contents
            .unwrap();
        let policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['blocked.example']",
        ).unwrap();
        let prepared =
            prepare_java_connection_import(source, Some(&local), policy.server_access()).unwrap();
        let bytes = prepared.contents.unwrap();
        assert_eq!(prepared.routes.len(), 1);
        let route = &prepared.routes["preserve-case"];
        let document = super::super::super::decode_document(&bytes).unwrap();
        assert_eq!(document.connections.len(), 8);
        assert_ne!(route.id, document.connections[0].id);
        assert_eq!(
            document.connections[0].jump.as_ref().unwrap().endpoint,
            endpoint("old-jump.example", "jump"),
        );
        assert_eq!(route.target, endpoint("main.example", "alice"));
        assert_eq!(
            route.jump,
            Some(LoginTarget {
                port: 2222,
                ..endpoint("jump.example", "jump")
            }),
        );
        check_java_imported_routes(&bytes, std::slice::from_ref(route), policy.server_access())
            .unwrap();
        let mut changed = route.clone();
        changed.id = document.connections[0].id;
        assert_eq!(
            check_java_imported_routes(&bytes, &[changed], policy.server_access())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
        );
        let mut changed = route.clone();
        changed.jump.as_mut().unwrap().port = 22;
        assert!(check_java_imported_routes(&bytes, &[changed], policy.server_access()).is_err());
        let changed_policy = crate::clipboard_policy::parse(
            "[meta]\nschema-version = 1\n[[rule]]\n[rule.servers]\nmode = 'deny'\nhosts = ['jump.example:2222']",
        ).unwrap();
        assert_eq!(
            check_java_imported_routes(
                &bytes,
                std::slice::from_ref(route),
                changed_policy.server_access()
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::PermissionDenied,
        );
    }
}
