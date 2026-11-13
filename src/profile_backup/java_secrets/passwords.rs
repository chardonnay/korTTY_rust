//! Maps Java stored and inline SSH passwords to native vault additions.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

#[cfg(test)]
use kortty_vault::Secret;
use kortty_vault::{CredentialAddition, CredentialInput, CredentialKind};
use roxmltree::Node;

use super::{JavaSecrets, children, optional_child, parse_xml, raw_scalar, scalar};
use crate::ssh_dialog::{LoginTarget, normalize_host, unsafe_text, valid_username};
use crate::workspace::environment_access::DEFAULT_ENVIRONMENT;

const MAX_CREDENTIAL_XML_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONNECTION_XML_BYTES: usize = 1024 * 1024;
const MAX_CREDENTIALS: usize = 1024;
const MAX_CONNECTIONS: usize = 512;
const MAX_DERIVATIONS: usize = 256;
const MAX_SSH_PASSWORD_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::profile_backup) enum Hop {
    Main,
    Jump,
}

pub(in crate::profile_backup) struct Binding {
    pub(in crate::profile_backup) source_connection_id: String,
    pub(in crate::profile_backup) hop: Hop,
    pub(in crate::profile_backup) route_target: LoginTarget,
    pub(in crate::profile_backup) route_jump: Option<LoginTarget>,
    pub(in crate::profile_backup) endpoint: LoginTarget,
    pub(in crate::profile_backup) addition: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(in crate::profile_backup) enum Omission {
    InvalidStoredCredential,
    ExternalPasswordCommand,
    UnsupportedPasswordType,
    InvalidConnection,
    UnsupportedAuthentication,
    UnsupportedConnection,
    InvalidCredentialReference,
    CredentialScopeMismatch,
    InvalidPasswordEnvelope,
    UnauthenticatedPassword,
    DerivationLimit,
}

#[derive(Default)]
pub(in crate::profile_backup) struct PasswordImport {
    pub(in crate::profile_backup) additions: Vec<CredentialAddition>,
    /// Additions before this index are standalone stored credentials.
    pub(in crate::profile_backup) standalone: usize,
    pub(in crate::profile_backup) bindings: Vec<Binding>,
    pub(in crate::profile_backup) omissions: BTreeMap<Omission, usize>,
}

impl PasswordImport {
    pub(in crate::profile_backup) fn omitted_count(&self) -> usize {
        self.omissions.values().sum()
    }
}

/// The result owns only the plaintext passwords to be sealed by the destination vault.
pub(in crate::profile_backup) fn inspect(
    secrets: &JavaSecrets<'_>,
    credentials_xml: Option<&[u8]>,
    connections_xml: Option<&[u8]>,
) -> io::Result<PasswordImport> {
    let credentials = credentials_xml
        .map(|bytes| parse_xml(bytes, MAX_CREDENTIAL_XML_BYTES, "credentials"))
        .transpose()?;
    let connections = connections_xml
        .map(|bytes| parse_xml(bytes, MAX_CONNECTION_XML_BYTES, "connections"))
        .transpose()?;
    let mut reader = Reader {
        secrets,
        remaining: MAX_DERIVATIONS,
        stored: BTreeMap::new(),
        import: PasswordImport::default(),
    };
    if let Some(document) = credentials {
        reader.credentials(document.root_element())?;
    }
    reader.import.standalone = reader.import.additions.len();
    if let Some(document) = connections {
        reader.connections(document.root_element())?;
    }
    // Backups without a verifier can still authenticate each field independently.
    if secrets.master_key.is_none()
        && reader.import.additions.is_empty()
        && reader
            .import
            .omissions
            .contains_key(&Omission::UnauthenticatedPassword)
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "The Java KorTTY master password could not authenticate any SSH passwords.",
        ));
    }
    Ok(reader.import)
}

struct Reader<'a, 'password> {
    secrets: &'a JavaSecrets<'password>,
    remaining: usize,
    stored: BTreeMap<String, usize>,
    import: PasswordImport,
}

impl Reader<'_, '_> {
    fn credentials(&mut self, root: Node<'_, '_>) -> io::Result<()> {
        let mut ids = BTreeSet::new();
        for (index, node) in children(root, "credential").enumerate() {
            if index >= MAX_CREDENTIALS {
                return Err(invalid("Java credentials.xml exceeds its entry limit."));
            }
            let id = match raw_scalar(node, "id") {
                Ok(Some(id)) if valid_source_id(id) => id,
                _ => {
                    self.omit(Omission::InvalidStoredCredential);
                    continue;
                }
            };
            if !ids.insert(id) {
                return Err(invalid(
                    "Java credentials.xml contains duplicate credential IDs.",
                ));
            }
            match stored_password(node) {
                Ok((input, encoded)) => {
                    if let Some(addition) = self.add(input, encoded) {
                        self.stored.insert(id.to_owned(), addition);
                    }
                }
                Err(omission) => self.omit(omission),
            }
        }
        Ok(())
    }

    fn connections(&mut self, root: Node<'_, '_>) -> io::Result<()> {
        if root
            .attribute("schemaVersion")
            .is_some_and(|version| !matches!(version, "1" | "2"))
        {
            return Err(invalid("Unsupported Java connections.xml schema version."));
        }
        let mut ids = BTreeSet::new();
        for (index, node) in children(root, "connection").enumerate() {
            if index >= MAX_CONNECTIONS {
                return Err(invalid("Java connections.xml exceeds its entry limit."));
            }
            let id = node.attribute("id").filter(|id| valid_source_id(id));
            if id.is_some_and(|id| !ids.insert(id)) {
                return Err(invalid(
                    "Java connections.xml contains duplicate connection IDs.",
                ));
            }
            let Some(id) = id else {
                self.omit(Omission::InvalidConnection);
                continue;
            };
            if let Err(omission) = self.connection(node, id) {
                self.omit(omission);
            }
        }
        Ok(())
    }

    fn connection(&mut self, node: Node<'_, '_>, id: &str) -> Result<(), Omission> {
        let invalid = |_| Omission::InvalidConnection;
        let jump = optional_child(node, "jumpServer").map_err(invalid)?;
        if !has_password_fields(node) && !jump.is_some_and(has_password_fields) {
            return Ok(());
        }
        if !matches!(
            scalar(node, "connectionSource").map_err(invalid)?,
            None | Some("" | "LOCAL")
        ) || !matches!(
            scalar(node, "protocol").map_err(invalid)?,
            None | Some("" | "SSH_TCP")
        ) {
            return Err(Omission::UnsupportedConnection);
        }
        let (host, target) = endpoint(node).map_err(invalid)?;
        let jump = match jump {
            Some(jump) => match scalar(jump, "enabled").map_err(invalid)? {
                Some("true" | "1") => {
                    let (host, endpoint) = endpoint(jump).map_err(invalid)?;
                    Some((jump, host, endpoint))
                }
                None | Some("" | "false" | "0") => None,
                _ => return Err(Omission::InvalidConnection),
            },
            None => None,
        };
        let route_jump = jump.as_ref().map(|(_, _, endpoint)| endpoint);
        let main = self.password(node, Hop::Main, host, &target);
        self.bind(main, id, Hop::Main, &target, route_jump);
        if let Some((jump, host, endpoint)) = &jump {
            let password = self.password(*jump, Hop::Jump, host, endpoint);
            self.bind(password, id, Hop::Jump, &target, route_jump);
        }
        Ok(())
    }

    fn bind(
        &mut self,
        password: Result<Option<usize>, Omission>,
        id: &str,
        hop: Hop,
        route_target: &LoginTarget,
        route_jump: Option<&LoginTarget>,
    ) {
        match password {
            Ok(Some(addition)) => {
                let endpoint = match hop {
                    Hop::Main => route_target,
                    Hop::Jump => route_jump.expect("the enabled jump endpoint was validated"),
                };
                self.import.bindings.push(Binding {
                    source_connection_id: id.to_owned(),
                    hop,
                    route_target: route_target.clone(),
                    route_jump: route_jump.cloned(),
                    endpoint: endpoint.clone(),
                    addition,
                });
            }
            Ok(None) => {}
            Err(omission) => self.omit(omission),
        }
    }

    fn password(
        &mut self,
        node: Node<'_, '_>,
        hop: Hop,
        source_host: &str,
        endpoint: &LoginTarget,
    ) -> Result<Option<usize>, Omission> {
        let invalid = |_| Omission::InvalidConnection;
        let reference = raw_scalar(node, "credentialId").map_err(invalid)?;
        if reference.is_none() && !has_password_fields(node) {
            return Ok(None);
        }
        if !matches!(
            scalar(node, "authMethod").map_err(invalid)?,
            None | Some("" | "PASSWORD")
        ) || scalar(node, "temporaryKeyContent")
            .map_err(invalid)?
            .is_some_and(|content| !content.is_empty())
            || scalar(node, "privateKeyPath")
                .map_err(invalid)?
                .is_some_and(|path| path.starts_with("TEMPORARY:"))
        {
            return Err(Omission::UnsupportedAuthentication);
        }
        let addition = if let Some(reference) = reference {
            if hop != Hop::Main {
                return Err(Omission::InvalidCredentialReference);
            }
            let addition = *self
                .stored
                .get(reference)
                .ok_or(Omission::InvalidCredentialReference)?;
            let input = &self.import.additions[addition].input;
            if input.username != endpoint.username
                || !source_pattern_matches(&input.server_pattern, source_host, &endpoint.host)
            {
                return Err(Omission::CredentialScopeMismatch);
            }
            addition
        } else {
            let Some(encoded) = scalar(node, "encryptedPassword").map_err(invalid)? else {
                return Ok(None);
            };
            if encoded.is_empty() {
                return Ok(None);
            }
            let label = match hop {
                Hop::Main => "Imported SSH password",
                Hop::Jump => "Imported SSH jump password",
            };
            let label_with_host = format!("{label} for {}", endpoint.host);
            let input = CredentialInput {
                label: if label_with_host.len() <= 256 {
                    label_with_host
                } else {
                    endpoint.host.clone()
                },
                username: endpoint.username.clone(),
                server_pattern: endpoint.host.clone(),
                kind: CredentialKind::Password,
                ..CredentialInput::default()
            };
            let Some(addition) = self.add(input, encoded) else {
                return Ok(None);
            };
            addition
        };
        Ok(Some(addition))
    }

    fn add(&mut self, input: CredentialInput, encoded: &str) -> Option<usize> {
        if self.remaining == 0 {
            self.omit(Omission::DerivationLimit);
            return None;
        }
        self.remaining -= 1;
        let secret = match self
            .secrets
            .decrypt_field_with_limit(encoded, MAX_SSH_PASSWORD_BYTES)
        {
            Ok(secret) => secret,
            Err(error) => {
                self.omit(if error.kind() == io::ErrorKind::PermissionDenied {
                    Omission::UnauthenticatedPassword
                } else {
                    Omission::InvalidPasswordEnvelope
                });
                return None;
            }
        };
        let index = self.import.additions.len();
        self.import.additions.push(CredentialAddition {
            input,
            secret,
            webhook: None,
        });
        Some(index)
    }

    fn omit(&mut self, omission: Omission) {
        *self.import.omissions.entry(omission).or_default() += 1;
    }
}

fn stored_password<'a>(node: Node<'a, '_>) -> Result<(CredentialInput, &'a str), Omission> {
    let invalid = |_| Omission::InvalidStoredCredential;
    match scalar(node, "passwordType").map_err(invalid)? {
        None | Some("STORED") => {}
        Some("EXTERNAL_COMMAND") => return Err(Omission::ExternalPasswordCommand),
        Some(_) => return Err(Omission::UnsupportedPasswordType),
    }
    let label = raw_scalar(node, "name")
        .map_err(invalid)?
        .ok_or(Omission::InvalidStoredCredential)?;
    let username = raw_scalar(node, "username")
        .map_err(invalid)?
        .ok_or(Omission::InvalidStoredCredential)?;
    let server_pattern = raw_scalar(node, "serverPattern")
        .map_err(invalid)?
        .unwrap_or_default();
    let environment = match scalar(node, "environmentId").map_err(invalid)? {
        Some(value) if !value.is_empty() => value,
        _ => scalar(node, "environment")
            .map_err(invalid)?
            .filter(|value| !value.is_empty())
            .unwrap_or(DEFAULT_ENVIRONMENT),
    };
    let description = raw_scalar(node, "description")
        .map_err(invalid)?
        .unwrap_or_default();
    if label.trim().is_empty()
        || label.len() > 256
        || unsafe_text(label)
        || !valid_username(username)
        || username.trim() != username
        || !valid_pattern(server_pattern)
        || environment.len() > 128
        || unsafe_text(environment)
        || description.len() > 2048
        || description
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\r' | '\n' | '\t'))
    {
        return Err(Omission::InvalidStoredCredential);
    }
    let encoded = required(node, "encryptedPassword").map_err(invalid)?;
    Ok((
        CredentialInput {
            label: label.into(),
            username: username.into(),
            server_pattern: server_pattern.into(),
            environment: environment.into(),
            description: description.into(),
            kind: CredentialKind::Password,
            ..CredentialInput::default()
        },
        encoded,
    ))
}

fn endpoint<'a>(node: Node<'a, '_>) -> Result<(&'a str, LoginTarget), ()> {
    let source_host = required(node, "host")?;
    let host = normalize_host(source_host).map_err(|_| ())?;
    let username = raw_scalar(node, "username")?.ok_or(())?;
    let port = match scalar(node, "port")? {
        None => 22,
        Some(port) if port.bytes().all(|byte| byte.is_ascii_digit()) => {
            port.parse().map_err(|_| ())?
        }
        Some(_) => return Err(()),
    };
    let endpoint = LoginTarget {
        host,
        port,
        username: username.into(),
    };
    endpoint.validate().map_err(|_| ())?;
    Ok((source_host, endpoint))
}

fn has_password_fields(node: Node<'_, '_>) -> bool {
    children(node, "credentialId").next().is_some()
        || children(node, "encryptedPassword").next().is_some()
}

fn valid_source_id(id: &str) -> bool {
    (1..=128).contains(&id.len()) && !id.chars().any(char::is_whitespace) && !unsafe_text(id)
}

fn valid_pattern(pattern: &str) -> bool {
    pattern.len() <= 253
        && pattern
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'_' | b'*'))
}

fn source_pattern_matches(pattern: &str, source_host: &str, native_host: &str) -> bool {
    if pattern.is_empty() {
        return true;
    }
    // Java tests the original spelling case-sensitively. The native route may
    // canonicalize an IP literal, and the vault ignores a trailing DNS root dot.
    let java = pattern.replace('.', "\\.").replace('*', ".*");
    let native = pattern
        .strip_suffix('.')
        .unwrap_or(pattern)
        .replace('.', "\\.")
        .replace('*', ".*");
    regex::Regex::new(&format!("\\A{java}\\z")).is_ok_and(|java| java.is_match(source_host))
        && regex::RegexBuilder::new(&format!("\\A{native}\\z"))
            .case_insensitive(true)
            .build()
            .is_ok_and(|native| {
                native.is_match(native_host.strip_suffix('.').unwrap_or(native_host))
            })
}

fn required<'a>(node: Node<'a, '_>, field: &'static str) -> Result<&'a str, ()> {
    scalar(node, field)?
        .filter(|value| !value.is_empty())
        .ok_or(())
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MASTER_PROPERTIES: &[u8] = include_bytes!("fixtures/master.properties");
    const ENVELOPES: &str = include_str!("fixtures/envelopes.txt");

    #[test]
    fn imports_stored_and_inline_passwords_with_exact_route_bindings() {
        let mut envelopes = ENVELOPES.lines();
        let stored = envelopes.next().unwrap();
        let jump = envelopes.next().unwrap();
        let credentials = format!(
            r#"<credentials>
              <credential>
                <id>credential-1</id><name>Stored café login</name><username>fixture</username>
                <environmentId>DEV-CUSTOM</environmentId><serverPattern>*.fixture.test</serverPattern>
                <encryptedPassword>{stored}</encryptedPassword>
              </credential>
              <credential>
                <id>external</id><passwordType>EXTERNAL_COMMAND</passwordType>
                <encryptedExternalCommand>opaque command</encryptedExternalCommand>
              </credential>
            </credentials>"#
        );
        let connections = format!(
            r#"<connections schemaVersion="2">
              <connection id="route-a">
                <host>first.fixture.test</host><port>2222</port><username>fixture</username>
                <credentialId>credential-1</credentialId><encryptedPassword>ignored</encryptedPassword>
                <jumpServer>
                  <enabled>true</enabled><host>jumphost.fixture.test</host><username>jump-user</username>
                  <encryptedPassword>{jump}</encryptedPassword>
                </jumpServer>
              </connection>
              <connection id="route-b">
                <host>second.fixture.test</host><username>fixture</username>
                <credentialId>credential-1</credentialId>
              </connection>
              <connection id="route-c">
                <host>outside.example</host><username>fixture</username>
                <credentialId>credential-1</credentialId><encryptedPassword>ignored</encryptedPassword>
              </connection>
              <connection id="route-d">
                <host>first.fixture.test</host><username>other-user</username>
                <credentialId>credential-1</credentialId><encryptedPassword>ignored</encryptedPassword>
              </connection>
              <connection id="route-e">
                <host>first.fixture.test</host><username>fixture</username>
                <credentialId>external</credentialId><encryptedPassword>ignored</encryptedPassword>
              </connection>
            </connections>"#
        );
        let password = Secret::new("päss🔑例".into());
        let secrets = JavaSecrets::open(Some(MASTER_PROPERTIES), &password).unwrap();
        let import = inspect(
            &secrets,
            Some(credentials.as_bytes()),
            Some(connections.as_bytes()),
        )
        .unwrap();
        assert_eq!(import.standalone, 1);
        assert_eq!(import.additions.len(), 2);
        assert_eq!(import.additions[0].input.kind, CredentialKind::Password);
        assert_eq!(import.additions[0].input.environment, "DEV-CUSTOM");
        assert_eq!(import.additions[0].secret.as_str(), "café – 新🔒");
        assert_eq!(import.additions[1].input.username, "jump-user");
        assert!(import.additions[1].secret.is_empty());
        assert_eq!(import.bindings.len(), 3);
        let first = &import.bindings[0];
        assert_eq!(first.source_connection_id, "route-a");
        assert_eq!(first.hop, Hop::Main);
        assert_eq!(first.endpoint.port, 2222);
        assert_eq!(first.route_target, first.endpoint);
        let jump = &import.bindings[1];
        assert_eq!(jump.hop, Hop::Jump);
        assert_eq!(jump.route_target, first.route_target);
        assert_eq!(jump.route_jump.as_ref(), Some(&jump.endpoint));
        assert_eq!(first.route_jump, jump.route_jump);
        assert_eq!(jump.endpoint.host, "jumphost.fixture.test");
        assert_eq!(jump.addition, 1);
        assert_eq!(import.bindings[2].source_connection_id, "route-b");
        assert_eq!(import.bindings[2].addition, 0);
        assert_eq!(
            import.omissions.get(&Omission::ExternalPasswordCommand),
            Some(&1)
        );
        assert_eq!(
            import.omissions.get(&Omission::CredentialScopeMismatch),
            Some(&2)
        );
        assert_eq!(
            import.omissions.get(&Omission::InvalidCredentialReference),
            Some(&1)
        );
        assert_eq!(import.omitted_count(), 4);
    }

    #[test]
    fn wrong_source_password_without_master_key_can_be_retried() {
        let encoded = ENVELOPES.lines().next().unwrap();
        let connections = format!(
            r#"<connections><connection id="inline">
              <host>fixture.test</host><username>fixture</username>
              <encryptedPassword>{encoded}</encryptedPassword>
            </connection></connections>"#
        );
        let password = Secret::new("different password".into());
        let secrets = JavaSecrets::open(None, &password).unwrap();
        let error = inspect(&secrets, None, Some(connections.as_bytes()))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn rejects_ambiguous_source_ids_and_untrusted_xml() {
        let password = Secret::new("päss🔑例".into());
        let secrets = JavaSecrets::open(None, &password).unwrap();
        for input in [
            br#"<connections><connection id="dupe"/><connection id="dupe"/></connections>"#
                .as_slice(),
            br#"<!DOCTYPE connections [<!ENTITY secret SYSTEM "file:///secret">]><connections/>"#,
            br#"<connections xmlns="urn:foreign"/>"#,
        ] {
            let error = inspect(&secrets, None, Some(input)).err().unwrap();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }
}
