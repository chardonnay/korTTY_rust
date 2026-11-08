use crate::session_journal::viewer::Screenshot;

use super::{Analysis, Arc, Directory, Writer, invalid, io, screenshot_analysis_store};

const MAX_TIMELINE_ANALYSIS_BYTES: usize = 256 * 1024;

impl Writer {
    pub(super) fn store_screenshot_analysis(
        &mut self,
        asset: &Screenshot,
        analysis: Arc<Analysis>,
    ) -> io::Result<screenshot_analysis_store::Outcome> {
        let index = self.document.entry_index(asset.id())?;
        let entry = &self.document.entries[index];
        if entry.kind != "SCREENSHOT"
            || entry.screenshot.as_deref() != Some(asset.reference())
            || asset.created_at() != Some(entry.time.to_string().as_str())
        {
            return Err(invalid("The selected journal screenshot changed"));
        }

        let source = asset.source();
        let journal = Directory::open(source.directory(), false)?;
        let verify = || asset.require_image_current_in(&journal);
        self.directory.require_metadata()?;
        if entry.analysis.as_deref() == Some(analysis.as_ref()) {
            verify()?;
            return Ok(screenshot_analysis_store::Outcome::default());
        }

        let stored_bytes: usize = self
            .document
            .entries
            .iter()
            .enumerate()
            .filter(|(position, _)| *position != index)
            .filter_map(|(_, entry)| entry.analysis.as_deref())
            .map(screenshot_analysis_store::text_bytes)
            .sum();
        if stored_bytes + screenshot_analysis_store::text_bytes(&analysis)
            > MAX_TIMELINE_ANALYSIS_BYTES
        {
            return Err(invalid(
                "Session journal screenshot analysis exceeds its storage limit",
            ));
        }

        self.log.sync()?;
        let previous = self.document.entries[index].analysis.replace(analysis);
        let durability_warning = match self.document.save_checked(
            &mut self.directory,
            self.log.number,
            self.sequence,
            None,
            verify,
        ) {
            Ok(()) => false,
            Err(error) if error.published => true,
            Err(error) => {
                self.document.entries[index].analysis = previous;
                return Err(error.error);
            }
        };
        self.advance_timeline();
        Ok(screenshot_analysis_store::Outcome {
            changed: true,
            durability_warning,
        })
    }
}
