use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::private_files::EntryIdentity;

use super::Source;

/// A private profile receipt for exactly the directory the user or recorder selected.
/// Restoring it does not open or authorize a different journal occupying the same path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SummarySource {
    directory: PathBuf,
    id: String,
    location: (EntryIdentity, EntryIdentity),
}

impl SummarySource {
    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn source(&self) -> Source {
        Source {
            directory: self.directory.clone(),
            id: self.id.clone(),
            location: self.location,
        }
    }
}

impl Source {
    pub(crate) fn require_summary_location(&self) -> io::Result<()> {
        self.open_verified().map(drop)
    }

    pub(crate) fn summary_receipt(&self) -> SummarySource {
        SummarySource {
            directory: self.directory.clone(),
            id: self.id.clone(),
            location: self.location,
        }
    }
}
