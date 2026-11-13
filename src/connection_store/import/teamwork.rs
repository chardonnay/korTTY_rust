//! Reads a bounded, read-only snapshot of shared connection definitions.

use std::collections::HashSet;
use std::io;
use std::path::Path;

use crate::ssh_dialog::unsafe_text;

use super::{Connection, MAX_BYTES, MAX_PROFILES, invalid, kortty, read_stable, xml};

const MAX_ID_BYTES: usize = 128;
pub(crate) const MAX_TEAMWORK_BYTES: u64 = MAX_BYTES;

#[derive(Clone, Debug)]
pub(crate) struct TeamworkEntry {
    pub(crate) id: String,
    pub(crate) connection: Connection,
    pub(crate) group: String,
    pub(crate) badge: Option<String>,
    pub(crate) default_auth_eligible: bool,
    pub(crate) last_used_at: Option<u64>,
}

pub(crate) fn read_teamwork(path: &Path) -> io::Result<Vec<TeamworkEntry>> {
    if !path.is_absolute() {
        return Err(invalid(
            "choose an absolute path to a shared connection file",
        ));
    }
    let (content, _) = read_stable(path, MAX_TEAMWORK_BYTES, None)?;
    read_teamwork_bytes(&content)
}

pub(crate) fn read_teamwork_bytes(content: &[u8]) -> io::Result<Vec<TeamworkEntry>> {
    if content.len() as u64 > MAX_TEAMWORK_BYTES {
        return Err(invalid("shared connection XML exceeds its size limit"));
    }
    xml::decode(content, parse)
}

pub(crate) fn valid_teamwork_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && !id.chars().any(char::is_whitespace)
        && !unsafe_text(id)
}

fn parse(content: &str) -> io::Result<Vec<TeamworkEntry>> {
    let document = xml::parse_document(content)
        .map_err(|_| invalid("invalid shared connection XML or XML node limit exceeded"))?;
    let root = document.root_element();
    if !xml::has_unqualified_tag(root, "connections") {
        return Err(invalid("expected a shared kortty connections document"));
    }
    let own_settings = kortty::own_settings_supported(root)?;
    let mut ids = HashSet::new();
    let mut entries = Vec::new();
    for node in root.children() {
        if node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty()) {
            return Err(invalid("unexpected text in shared connection XML"));
        }
        if !node.is_element() {
            continue;
        }
        if !xml::has_unqualified_tag(node, "connection") {
            return Err(invalid("unexpected entry in shared connection XML"));
        }
        let id = node
            .attribute("id")
            .filter(|id| valid_teamwork_id(id))
            .ok_or_else(|| invalid("missing or invalid shared connection ID"))?;
        if !ids.insert(id) {
            return Err(invalid("duplicate shared connection ID"));
        }
        if ids.len() > MAX_PROFILES {
            return Err(invalid(
                "shared connection XML contains more than 512 entries",
            ));
        }
        let placeholder = xml::raw_scalar(node, "host")?.as_deref() == Some("placeholder")
            && xml::raw_scalar(node, "name")?.is_some_and(|name| name.starts_with("(Ordner:"));
        if placeholder || !kortty::is_ssh(node)? {
            continue;
        }
        // The interchange parser keeps the authentication method, but never
        // imports inline material or binds foreign IDs to local credentials.
        let (profile, skipped_tunnels) = kortty::connection(node, own_settings)?;
        if skipped_tunnels > 0 {
            return Err(invalid("invalid SSH forwarding in shared connection XML"));
        }
        // The presence of either target reference excludes the global default.
        // Never retain the referenced IDs from the shared source.
        let default_auth_eligible = !node.children().any(|child| {
            xml::has_unqualified_tag(child, "credentialId")
                || xml::has_unqualified_tag(child, "sshKeyId")
        });
        // Java writes a signed long in milliseconds since the Unix epoch.
        let last_used_at = xml::raw_scalar(node, "lastUsed")?
            .and_then(|value| value.trim().parse::<i64>().ok())
            .filter(|&value| value > 0)
            .map(|value| value as u64);
        entries.push(TeamworkEntry {
            id: id.to_owned(),
            connection: profile.connection,
            group: profile.group,
            badge: profile.badge,
            default_auth_eligible,
            last_used_at,
        });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::path::PathBuf;

    use crate::connection_store::{Auth, AuthMethod};

    use super::*;

    const ROUTE: &str = "<host>ssh.example.org</host><port>22</port><username>ops</username>";

    fn snapshot(entries: &str) -> String {
        format!("<connections schemaVersion=\"2\">{entries}</connections>")
    }

    #[test]
    fn teamwork_last_used_is_a_positive_java_long_in_epoch_milliseconds() {
        for (field, expected) in [
            ("", None),
            ("<lastUsed>0</lastUsed>", None),
            ("<lastUsed>-1</lastUsed>", None),
            ("<lastUsed>yesterday</lastUsed>", None),
            ("<lastUsed>9223372036854775808</lastUsed>", None),
            (
                "<lastUsed> +1700000000000 </lastUsed>",
                Some(1_700_000_000_000),
            ),
            (
                "<lastUsed>9223372036854775807</lastUsed>",
                Some(i64::MAX as u64),
            ),
        ] {
            let xml = snapshot(&format!(
                r#"<connection id="timestamp">{ROUTE}{field}</connection>"#
            ));
            assert_eq!(parse(&xml).unwrap()[0].last_used_at, expected, "{field}");
        }
    }

    #[test]
    fn teamwork_keeps_source_ids_and_never_binds_inline_secrets_or_foreign_ids() {
        let content = snapshot(&format!(
            r#"<connection id="prod-main">{ROUTE}
                <name>Production</name><group>Ops/Production</group><osBadge>debian</osBadge>
                <authMethod>PUBLIC_KEY</authMethod><sshKeyId>42</sshKeyId><credentialId>local-secret</credentialId>
                <encryptedPassword>ignored</encryptedPassword><privateKeyPath>C:/private/key</privateKeyPath>
                <privateKeyPassphrase>ignored</privateKeyPassphrase><temporaryKeyContent>ignored</temporaryKeyContent>
                <jumpServer><enabled>true</enabled><host>jump.example.org</host><username>ops</username>
                    <credentialId>another-secret</credentialId><encryptedPassword>ignored</encryptedPassword>
                    <privateKeyPath>C:/private/jump-key</privateKeyPath>
                </jumpServer>
            </connection>
            <connection id="same-route">{ROUTE}<name>Production</name><group>Ops/Production</group>
                <jumpServer><enabled>true</enabled><host>jump.example.org</host><username>ops</username>
                    <credentialId>jump-reference</credentialId>
                </jumpServer>
            </connection>
            <connection id="empty-reference">{ROUTE}<sshKeyId/></connection>
            <connection id="empty-folder"><name>(Ordner: Empty)</name><host>placeholder</host></connection>
            <connection id="excluded"><protocol>MOSH</protocol>{ROUTE}</connection>"#
        ));
        let entries = parse(&content).unwrap();
        assert_eq!(entries.len(), 3);
        let primary = &entries[0];
        assert_eq!(primary.id, "prod-main");
        assert_eq!(primary.connection.id, 0);
        assert_eq!(primary.group, "Ops/Production");
        assert_eq!(primary.badge.as_deref(), Some("debian"));
        assert_eq!(entries[1].id, "same-route");
        assert!(!primary.default_auth_eligible);
        assert!(entries[1].default_auth_eligible);
        assert!(!entries[2].default_auth_eligible);
        assert_eq!(
            primary.connection.login.endpoint,
            entries[1].connection.login.endpoint
        );
        assert_eq!(
            primary.connection.login.auth,
            Auth::Configured {
                method: AuthMethod::PublicKey,
                credential: None,
                key: None,
            }
        );
        assert_eq!(primary.connection.jump.as_ref().unwrap().auth, Auth::Prompt);
    }

    #[test]
    fn teamwork_rejects_ambiguous_snapshots_and_dtds_as_a_whole() {
        let valid = format!(r#"<connection id="first">{ROUTE}</connection>"#);
        for malformed in [
            format!("<connection>{ROUTE}</connection>"),
            format!(r#"<connection id="">{ROUTE}</connection>"#),
            format!(r#"<connection id="has space">{ROUTE}</connection>"#),
            format!(r#"<connection id="unsafe&#x202e;">{ROUTE}</connection>"#),
            format!(
                r#"<connection id="{}">{ROUTE}</connection>"#,
                "x".repeat(MAX_ID_BYTES + 1)
            ),
            format!(r#"<connection id="first">{ROUTE}</connection>"#),
            r#"<connection id="invalid-host"><host>invalid host</host></connection>"#.into(),
            "<Server />".into(),
            "stray text".into(),
        ] {
            assert!(parse(&snapshot(&format!("{valid}{malformed}"))).is_err());
        }
        let dtd = r#"<!DOCTYPE connections [<!ENTITY example SYSTEM "file:///never-opened">]><connections/>"#;
        assert!(parse(dtd).is_err());
        assert!(parse("<Servers/>").is_err());
        assert!(parse("<connections/>").unwrap().is_empty());
    }

    #[test]
    fn teamwork_reads_bom_variants_from_a_bounded_regular_file() {
        struct Scratch(PathBuf);

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_file(&self.0);
            }
        }

        let path = Scratch(std::env::temp_dir().join(format!(
            "kortty-teamwork-{}.xml",
            crate::private_files::nonce().unwrap()
        )));
        let content =
            format!(r#"<connections><connection id="gerät">{ROUTE}</connection></connections>"#);
        let utf8 = [b"\xef\xbb\xbf".as_slice(), content.as_bytes()].concat();
        let utf16_le = [0xff, 0xfe]
            .into_iter()
            .chain(content.encode_utf16().flat_map(u16::to_le_bytes))
            .collect::<Vec<_>>();
        let utf16_be = [0xfe, 0xff]
            .into_iter()
            .chain(content.encode_utf16().flat_map(u16::to_be_bytes))
            .collect::<Vec<_>>();
        for bytes in [utf8, utf16_le, utf16_be] {
            fs::write(&path.0, bytes).unwrap();
            assert_eq!(read_teamwork(&path.0).unwrap()[0].id, "gerät");
        }
        File::create(&path.0)
            .unwrap()
            .set_len(MAX_BYTES + 1)
            .unwrap();
        assert!(read_teamwork(&path.0).is_err());
        assert!(read_teamwork(Path::new("relative.xml")).is_err());
    }
}
