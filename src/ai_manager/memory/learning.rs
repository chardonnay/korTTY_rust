//! Confirmed learned rules in the private AI Memory store.

use std::{
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use zeroize::Zeroizing;

use super::{Scope, format, store};
use crate::ai_manager::RuleSource;

pub(crate) const MAX_RULE_CHARACTERS: usize = 300;
const UNDO_WINDOW: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub(crate) struct RuleProposal {
    directory: PathBuf,
    scope: Scope,
    base: store::Snapshot,
    proposed: String,
    rule: String,
    entry: format::Entry,
}

impl fmt::Debug for RuleProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuleProposal")
            .finish_non_exhaustive()
    }
}

impl RuleProposal {
    pub(crate) fn current_text(&self) -> &str {
        &self.base.text
    }

    pub(crate) fn proposed_text(&self) -> &str {
        &self.proposed
    }

    pub(crate) fn rule(&self) -> &str {
        &self.rule
    }

    pub(crate) fn save(&self, budget: u16) -> io::Result<SavedRule> {
        require_budget(&self.proposed, budget)?;
        store::save(&self.directory, &self.scope, &self.base, &self.proposed)?;
        Ok(SavedRule {
            directory: self.directory.clone(),
            scope: self.scope.clone(),
            entry: self.entry.clone(),
            original_len: self.base.text.len(),
            written: Arc::from(self.proposed.as_str()),
            saved: Instant::now(),
            undo_claimed: Arc::new(AtomicBool::new(false)),
        })
    }
}

#[derive(Clone)]
pub(crate) struct SavedRule {
    directory: PathBuf,
    scope: Scope,
    entry: format::Entry,
    original_len: usize,
    written: Arc<str>,
    saved: Instant,
    undo_claimed: Arc<AtomicBool>,
}

impl fmt::Debug for SavedRule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("SavedRule").finish_non_exhaustive()
    }
}

impl SavedRule {
    pub(crate) fn remaining(&self) -> Duration {
        if self.undo_claimed.load(Ordering::Acquire) {
            return Duration::ZERO;
        }
        UNDO_WINDOW.saturating_sub(self.saved.elapsed())
    }

    pub(crate) fn undo(&self) -> io::Result<()> {
        require_undo_time(self.saved)?;
        if self
            .undo_claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::other(
                "Undo was already requested for this rule.",
            ));
        }
        let result = self.archive_entry();
        if result.is_err() {
            self.undo_claimed.store(false, Ordering::Release);
        }
        result
    }

    fn archive_entry(&self) -> io::Result<()> {
        let base = store::load(&self.directory, &self.scope)?;
        let mut matching = format::entries(&base.text)
            .into_iter()
            .filter(|entry| entry.id == self.entry.id);
        let Some(entry) = matching.next() else {
            return Err(io::Error::other("The saved rule was edited or removed."));
        };
        if matching.next().is_some()
            || entry.origin != self.entry.origin
            || entry.created != self.entry.created
            || entry.source != self.entry.source
            || entry.body != self.entry.body
        {
            return Err(io::Error::other("The saved rule no longer matches."));
        }
        require_undo_time(self.saved)?;
        if base.text == self.written.as_ref() {
            // The original document is an exact prefix of the reviewed append.
            store::move_to_archive_with_text(
                &self.directory,
                &self.scope,
                &base,
                &[entry],
                &base.text[..self.original_len],
            )?;
        } else {
            store::move_to_archive(&self.directory, &self.scope, &base, &[entry])?;
        }
        Ok(())
    }
}

pub(crate) fn preview(
    profile_root: &Path,
    connection: Option<u64>,
    input: &str,
    budget: u16,
    source: RuleSource,
) -> io::Result<RuleProposal> {
    let rule = checked_rule(input)?;
    let directory = profile_root.join("memory");
    let scope = connection.map_or_else(Scope::global, |id| Scope::connection(id, id.to_string()));
    let base = store::load(&directory, &scope)?;
    let body = format!("- {rule}");
    let entries = format::entries(&base.text);
    if entries.iter().any(|entry| entry.body == body) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "This rule is already saved.",
        ));
    }
    let id = crate::private_files::nonce()?;
    if entries.iter().any(|entry| entry.id == id) {
        return Err(io::Error::other(
            "Could not allocate a unique memory rule ID. Try again.",
        ));
    }
    let created = jiff::Zoned::now().date().to_string();
    let block = format::new_entry(&id, &created, source.name(), &body)
        .ok_or_else(|| io::Error::other("Could not format the memory rule."))?;
    let entry = format::entries(&block)
        .pop()
        .ok_or_else(|| io::Error::other("Could not format the memory rule."))?;
    let proposed = store::append(&base.text, &block);
    store::validate(&proposed, &base.text, store::MAX_BYTES)?;
    require_budget(&proposed, budget)?;
    Ok(RuleProposal {
        directory,
        scope,
        base,
        proposed,
        rule,
        entry,
    })
}

pub(crate) fn normalized_length(input: &str) -> usize {
    let mut length = 0;
    normalized_characters(input, |_| length += 1);
    length
}

fn normalized_characters(input: &str, mut emit: impl FnMut(char)) {
    let mut has_text = false;
    let mut space = false;
    for character in input
        .chars()
        .filter(|&character| !format::invisible(character))
    {
        if character.is_whitespace() {
            space = has_text;
            continue;
        }
        if space {
            emit(' ');
            space = false;
        }
        emit(character);
        has_text = true;
    }
}

pub(in crate::ai_manager) fn checked_rule(input: &str) -> io::Result<String> {
    if input.len() > store::MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The rule input is too large.",
        ));
    }
    let clean = format::checked_reference_text(input).map_err(possible_credential)?;
    let mut rule = Zeroizing::new(String::new());
    normalized_characters(&clean, |character| rule.push(character));
    if rule.is_empty()
        || rule.chars().count() > MAX_RULE_CHARACTERS
        || rule.contains("kortty:entry")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A memory rule needs 1–300 characters of text without a managed-entry marker.",
        ));
    }
    if let Some(line) = format::secret_line(&rule, "") {
        return Err(possible_credential(line));
    }
    Ok(std::mem::take(&mut *rule))
}

fn possible_credential(line: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("Possible credential on line {line}. Remove it before saving."),
    )
}

fn require_budget(text: &str, budget: u16) -> io::Result<()> {
    if format::estimate(text) > usize::from(budget) * 2 {
        return Err(io::Error::other(
            "The memory file would exceed twice its token budget.",
        ));
    }
    Ok(())
}

fn require_undo_time(saved: Instant) -> io::Result<()> {
    if saved.elapsed() >= UNDO_WINDOW {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "The undo period has expired.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Profile(PathBuf);

    impl Profile {
        fn new() -> io::Result<Self> {
            Ok(Self(std::env::temp_dir().canonicalize()?.join(format!(
                "kortty-memory-rule-{}",
                crate::private_files::nonce()?
            ))))
        }

        fn memory(&self) -> PathBuf {
            self.0.join("memory")
        }
    }

    impl Drop for Profile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn manual_rule_preview_normalizes_and_does_not_create_files() -> io::Result<()> {
        let profile = Profile::new()?;
        let input = " \t Keep   the\n blue\u{200b} route. \r\n";
        let proposal = preview(&profile.0, None, input, 1500, RuleSource::Manual)?;
        assert!(!profile.0.exists());
        assert_eq!(proposal.rule(), "Keep the blue route.");
        assert_eq!(normalized_length(input), proposal.rule().chars().count());
        assert_eq!(proposal.current_text(), "");
        assert!(proposal.proposed_text().contains("source=manual"));
        assert!(!format!("{proposal:?}").contains("blue route"));

        let accepted = "🗺".repeat(MAX_RULE_CHARACTERS);
        assert_eq!(normalized_length(&accepted), MAX_RULE_CHARACTERS);
        assert!(checked_rule(&accepted).is_ok());
        assert!(checked_rule(&format!("{accepted}🗺")).is_err());
        for rejected in [
            " \u{200b} ",
            "Never add <!-- kortty:entry id=foreign -->.",
            "pass\u{200b}word: Winter2026!",
            "password:\nWinter2026!",
        ] {
            assert!(checked_rule(rejected).is_err());
        }
        assert!(checked_rule(&" ".repeat(store::MAX_BYTES + 1)).is_err());
        Ok(())
    }

    #[test]
    fn reviewed_chat_rule_persists_its_source() -> io::Result<()> {
        let profile = Profile::new()?;
        let scope = Scope::connection(42, String::new());
        let proposal = preview(
            &profile.0,
            Some(42),
            "Use the maintenance host.",
            1500,
            RuleSource::Chat,
        )?;
        assert!(!profile.0.exists());
        assert!(proposal.proposed_text().contains("source=chat"));
        proposal.save(1500)?;
        let current = store::load(&profile.memory(), &scope)?;
        let entries = format::entries(&current.text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "chat");
        Ok(())
    }

    #[test]
    fn manual_rule_save_preserves_snapshot_and_refuses_duplicates_and_lower_budget()
    -> io::Result<()> {
        let profile = Profile::new()?;
        let directory = profile.memory();
        let scope = Scope::global();
        let original = "é".repeat(2500);
        let base = store::save(&directory, &scope, &store::Snapshot::default(), &original)?;
        let proposal = preview(
            &profile.0,
            None,
            "Retain audit logs.",
            1500,
            RuleSource::Manual,
        )?;
        assert_eq!(proposal.current_text(), original);
        assert!(proposal.save(300).is_err());
        assert_eq!(store::load(&directory, &scope)?.text, original);
        assert!(store::history(&directory, &scope)?.is_empty());

        store::save(&directory, &scope, &base, "A newer note.\n")?;
        assert!(proposal.save(1500).is_err());
        assert_eq!(store::load(&directory, &scope)?.text, "A newer note.\n");

        let first = preview(
            &profile.0,
            None,
            "Retain audit logs.",
            1500,
            RuleSource::Manual,
        )?;
        let concurrent = preview(
            &profile.0,
            None,
            "Retain audit logs.",
            1500,
            RuleSource::Manual,
        )?;
        let saved = first.save(1500)?;
        assert!(saved.remaining() > Duration::ZERO);
        assert!(concurrent.save(1500).is_err());
        assert_eq!(store::load(&directory, &scope)?.text, first.proposed_text());
        let duplicate = preview(
            &profile.0,
            None,
            "Retain   audit logs.",
            1500,
            RuleSource::Manual,
        )
        .unwrap_err();
        assert_eq!(duplicate.kind(), io::ErrorKind::AlreadyExists);
        let versions = store::history(&directory, &scope)?;
        assert_eq!(
            store::history_text(&directory, &scope, &versions[0])?,
            "A newer note.\n"
        );
        Ok(())
    }

    #[test]
    fn manual_rule_append_respects_the_file_byte_limit_even_under_a_large_budget() -> io::Result<()>
    {
        let profile = Profile::new()?;
        let directory = profile.memory();
        let original = "é".repeat(store::MAX_BYTES / 2);
        let scope = Scope::global();
        store::save(&directory, &scope, &store::Snapshot::default(), &original)?;
        let error = preview(
            &profile.0,
            None,
            "Keep the rule short.",
            u16::MAX,
            RuleSource::Manual,
        )
        .unwrap_err();
        assert!(error.to_string().contains("size limit"));
        assert_eq!(store::load(&directory, &scope)?.text, original);
        Ok(())
    }

    #[test]
    fn manual_rule_undo_archives_only_its_intact_rule_in_the_same_scope() -> io::Result<()> {
        let profile = Profile::new()?;
        let directory = profile.memory();
        let global_scope = Scope::global();
        let original = "Keep this global note.\n";
        store::save(
            &directory,
            &global_scope,
            &store::Snapshot::default(),
            original,
        )?;
        let global = preview(&profile.0, None, "Use staging.", 1500, RuleSource::Manual)?;
        let saved_global = global.save(1500)?;
        let connection = preview(
            &profile.0,
            Some(42),
            "Use staging.",
            1500,
            RuleSource::Manual,
        )?;
        let saved = connection.save(1500)?;
        assert!(!format!("{saved:?}").contains("staging"));

        let scope = Scope::connection(42, String::new());
        let base = store::load(&directory, &scope)?;
        let later = "Keep this later edit.\n";
        store::save(&directory, &scope, &base, &format!("{}{later}", base.text))?;

        saved.undo()?;
        assert!(saved.remaining().is_zero());
        assert!(saved.clone().undo().is_err());
        assert_eq!(store::load(&directory, &scope)?.text, later);
        assert_eq!(
            store::load(&directory, &Scope::global())?.text,
            global.proposed_text()
        );
        assert!(store::archive(&directory, &Scope::global())?.1.is_empty());
        let (_, archived) = store::archive(&directory, &scope)?;
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].entry.id, saved.entry.id);
        assert_eq!(archived[0].entry.body, "- Use staging.");

        let saved_block = global.proposed_text()[original.len()..]
            .strip_prefix('\n')
            .unwrap();
        saved_global.undo()?;
        assert_eq!(store::load(&directory, &global_scope)?.text, original);
        let (archive, items) = store::archive(&directory, &global_scope)?;
        assert_eq!(items.len(), 1);
        assert_eq!(&archive.text[items[0].entry.range.clone()], saved_block);
        Ok(())
    }

    #[test]
    fn manual_rule_undo_preserves_edited_rules_and_expires() -> io::Result<()> {
        let profile = Profile::new()?;
        let directory = profile.memory();
        let scope = Scope::global();
        let proposal = preview(&profile.0, None, "Use staging.", 1500, RuleSource::Manual)?;
        let mut saved = proposal.save(1500)?;
        let base = store::load(&directory, &scope)?;
        let rewritten = saved.entry.rewrite("Use the maintenance host.").unwrap();
        let new = store::save(&directory, &scope, &base, &rewritten)?;
        assert!(saved.undo().is_err());
        assert_eq!(store::load(&directory, &scope)?.text, rewritten);
        assert!(store::archive(&directory, &scope)?.1.is_empty());

        store::save(&directory, &scope, &new, proposal.proposed_text())?;
        saved.saved = Instant::now() - UNDO_WINDOW;
        assert!(saved.remaining().is_zero());
        assert_eq!(saved.undo().unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(
            store::load(&directory, &scope)?.text,
            proposal.proposed_text()
        );
        Ok(())
    }
}
