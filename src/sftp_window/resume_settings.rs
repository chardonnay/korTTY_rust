//! Personal preferences for retaining interrupted SFTP transfers.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResumePreferences {
    pub(crate) enabled: bool,
    pub(crate) keep_on_cancel: bool,
}

impl Default for ResumePreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_on_cancel: false,
        }
    }
}

impl ResumePreferences {
    pub(super) fn retention(self, cancelled: bool) -> bool {
        self.enabled && (!cancelled || self.keep_on_cancel)
    }
}
