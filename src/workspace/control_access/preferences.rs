use std::io::{self, Read};
use std::path::Path;

use serde::{Deserialize, Serialize};

const FILE: &str = "control-preferences.json";
const MAX_BYTES: u64 = 4096;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct Preferences {
    pub(super) enabled: bool,
    pub(super) mcp_server_enabled: bool,
    pub(super) mcp_write_tools_enabled: bool,
}

impl Preferences {
    pub(super) fn load(root: &Path) -> io::Result<Self> {
        let file = match crate::profile::open_preference(&root.join(FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if !file.metadata()?.is_file() {
            return Err(io::Error::other(
                "Control preferences must be a regular file",
            ));
        }
        let mut data = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MAX_BYTES {
            return Err(io::Error::other(
                "Control preferences exceed the size limit",
            ));
        }
        serde_json::from_slice(&data).map_err(io::Error::other)
    }

    pub(super) fn save(self, root: &Path) -> io::Result<()> {
        crate::profile::save_atomic(&root.join(FILE), &serde_json::to_vec(&self)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_preferences_default_off_and_serialize_separate_opt_ins() {
        assert_eq!(
            serde_json::from_str::<Preferences>("{}").unwrap(),
            Preferences::default()
        );
        let read_only = Preferences {
            enabled: true,
            mcp_server_enabled: true,
            ..Preferences::default()
        };
        let saved = serde_json::to_vec(&read_only).unwrap();
        assert_eq!(
            serde_json::from_slice::<Preferences>(&saved).unwrap(),
            read_only
        );
        assert!(!read_only.mcp_write_tools_enabled);
        assert!(serde_json::from_str::<Preferences>(r#"{"mcp_server_enabled":"true"}"#).is_err());
    }
}
