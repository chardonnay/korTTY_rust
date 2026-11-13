//! Durable plans, validated generation boundaries and prepared publications.

use super::*;
use crate::snippet_analysis::model::modularization::Job;
use crate::snippets::Library;

pub(super) const MAX_ACTIVE_JOBS: usize = 4;

pub(crate) struct ReviewedSplitRevision {
    pub(crate) pre: SnippetId,
    pub(crate) source_sha256: String,
    pub(crate) job: Option<Box<Job>>,
}

impl ReviewedSplitRevision {
    fn require(&self, record: &Record) -> io::Result<()> {
        if record.source.sha256 != self.source_sha256 || record.modularization != self.job {
            return Err(invalid(
                "The source analysis or split changed. Review the request again.",
            ));
        }
        Ok(())
    }
}

impl Store {
    #[cfg(test)]
    pub(crate) fn require_modularization(
        &self,
        subject: Subject,
        pre: SnippetId,
        expected: Option<(SnippetId, u32)>,
    ) -> io::Result<()> {
        let history = self.load(subject)?;
        let record = history
            .records
            .iter()
            .find(|record| record.id == pre && record.purpose != Purpose::Verify)
            .ok_or_else(|| invalid("The source analysis no longer exists"))?;
        require_job_version(record.modularization.as_deref(), expected)
    }

    pub(crate) fn require_reviewed_split(
        &self,
        subject: Subject,
        review: &ReviewedSplitRevision,
    ) -> io::Result<()> {
        let history = self.load(subject)?;
        let record = history
            .records
            .iter()
            .find(|record| record.id == review.pre && record.purpose != Purpose::Verify)
            .ok_or_else(|| invalid("The source analysis no longer exists"))?;
        review.require(record)
    }

    pub(crate) fn commit_modularization(
        &self,
        subject: Subject,
        pre: SnippetId,
        expected: (SnippetId, u32),
        library: &mut Library,
    ) -> io::Result<History> {
        self.update(subject, |history| {
            let record = history
                .records
                .iter_mut()
                .find(|record| record.id == pre)
                .ok_or_else(|| invalid("The source analysis no longer exists"))?;
            let job = record
                .modularization
                .as_deref_mut()
                .filter(|job| job.version() == expected && !job.published)
                .ok_or_else(|| invalid("The reviewed publication changed before saving"))?;
            let revision = job
                .revision
                .checked_add(1)
                .ok_or_else(|| invalid("The modularization revision is exhausted"))?;
            let publication = job
                .publication
                .as_ref()
                .ok_or_else(|| invalid("The reviewed file destinations are missing"))?;
            library.publish_modularization(publication)?;
            job.revision = revision;
            job.published = true;
            record.updated_at = now_millis().max(record.updated_at);
            Ok(true)
        })
    }

    pub(crate) fn save_modularization(
        &self,
        subject: Subject,
        pre: SnippetId,
        expected: Option<(SnippetId, u32)>,
        next: Option<Job>,
    ) -> io::Result<History> {
        self.save_modularization_while(subject, pre, expected, next, || true)
    }

    pub(crate) fn save_modularization_while(
        &self,
        subject: Subject,
        pre: SnippetId,
        expected: Option<(SnippetId, u32)>,
        next: Option<Job>,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.change_modularization(
            subject,
            pre,
            |record| require_job_version(record.modularization.as_deref(), expected),
            next,
            still_current,
        )
    }

    pub(crate) fn save_reviewed_split(
        &self,
        subject: Subject,
        review: &ReviewedSplitRevision,
        next: Job,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.change_modularization(
            subject,
            review.pre,
            |record| review.require(record),
            Some(next),
            still_current,
        )
    }

    fn change_modularization(
        &self,
        subject: Subject,
        pre: SnippetId,
        require: impl FnOnce(&Record) -> io::Result<()>,
        mut next: Option<Job>,
        still_current: impl Fn() -> bool,
    ) -> io::Result<History> {
        self.update_while(subject, still_current, |history| {
            let active = history
                .records
                .iter()
                .filter(|record| record.id != pre && retained(record))
                .count();
            let record = history
                .records
                .iter_mut()
                .find(|record| record.id == pre && record.purpose != Purpose::Verify)
                .ok_or_else(|| invalid("The source analysis no longer exists"))?;
            require(record)?;
            let previous = record.modularization.as_deref();
            if let Some(next) = &mut next {
                if previous.is_some_and(|old| old.id == next.id && old.outbound != next.outbound) {
                    return Err(invalid(
                        "The destination changed. Review a new modularization plan.",
                    ));
                }
                let previous_version = previous
                    .filter(|old| old.id == next.id)
                    .map_or(0, |old| old.revision);
                next.revision = previous_version
                    .checked_add(1)
                    .ok_or_else(|| invalid("The modularization revision is exhausted"))?;
                validate_job(next, record, subject)?;
                if next.generation.is_some() && !next.published && active >= MAX_ACTIVE_JOBS {
                    return Err(invalid(
                        "Finish or discard an existing modularization before starting another",
                    ));
                }
            }
            if previous.is_some_and(|job| job.publication.is_some() && !job.published)
                && !next.as_ref().is_some_and(|job| {
                    previous.is_some_and(|previous| {
                        previous.id == job.id && previous.publication == job.publication
                    })
                })
            {
                return Err(invalid(
                    "Resolve the prepared publication before changing this plan",
                ));
            }
            if next.is_none() && previous.is_none() {
                return Ok(false);
            }
            record.modularization = next.map(Box::new);
            record.updated_at = now_millis().max(record.updated_at);
            Ok(true)
        })
    }
}

fn require_job_version(
    previous: Option<&Job>,
    expected: Option<(SnippetId, u32)>,
) -> io::Result<()> {
    if previous.map(Job::version) != expected {
        return Err(invalid(
            "The modularization changed. Reload its history and review the request again.",
        ));
    }
    Ok(())
}

pub(super) fn validate_job(job: &Job, record: &Record, subject: Subject) -> io::Result<()> {
    if record.purpose == Purpose::Verify
        || (job.outbound.is_none()
            && job
                .generation
                .as_ref()
                .is_some_and(|generation| generation.profile_id != record.provenance.profile_id))
        || matches!(subject, Subject::Snippet(id) if !matches!(job.sources.as_slice(), [(_, original)] if *original == id))
    {
        return Err(invalid(
            "The stored modularization has an invalid analysis, source identity or profile",
        ));
    }
    job.validate(&record.source.sha256)
        .map_err(io::Error::other)
}

pub(super) fn retained(record: &Record) -> bool {
    record
        .modularization
        .as_deref()
        .is_some_and(|job| job.generation.is_some() && !job.published)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_manager::{CodeTextLanguage, ProfileRef};
    use crate::snippet_analysis::model::{
        Analysis, Provenance, Source,
        modularization::{Generation, Module, OutboundBinding, OutboundKind, Plan},
    };
    use crate::snippets::{Input, tests::Profile};

    #[test]
    fn prepared_split_recovers_after_library_commit_without_duplicate_files() {
        prepared_split_recovers(ProfileRef::Numeric(1), None);
    }

    #[test]
    fn named_split_recovery_preserves_the_exact_generation_profile() {
        prepared_split_recovers(ProfileRef::Policy("policy-SnippetSplit".into()), None);
    }

    #[test]
    fn outbound_split_keeps_its_destination_and_recovers_without_provider_access() {
        prepared_split_recovers(
            ProfileRef::Numeric(1),
            Some(OutboundBinding {
                kind: OutboundKind::Cli,
                profile_id: ProfileRef::Numeric(7),
                profile_revision: [3; 32],
            }),
        );
    }

    fn prepared_split_recovers(reference: ProfileRef, outbound: Option<OutboundBinding>) {
        let profile = Profile::new();
        let mut library = Library::open(profile.path()).unwrap();
        let original = "print('before')\n";
        let source = library
            .save(
                None,
                Input {
                    name: "Original".into(),
                    language: "python".into(),
                    category: String::new(),
                    content: original.into(),
                },
            )
            .unwrap();
        let store = Store::new(profile.0.clone());
        let subject = Subject::Snippet(source);
        let record = Record::new(
            Purpose::Analysis,
            None,
            Source::new(original, "python", "en", "en", "Original").unwrap(),
            Provenance {
                profile_id: reference.clone(),
                ..Provenance::default()
            },
            Analysis::default(),
        )
        .unwrap();
        let pre = record.id;
        let source_sha256 = record.source.sha256.clone();
        store.append(subject, record).unwrap();
        store.require_modularization(subject, pre, None).unwrap();
        let plan = Plan {
            recommended: true,
            rationale: "Split helper from the entry point.".into(),
            files: vec![
                Module {
                    path: "entry.py".into(),
                    purpose: "Entry point".into(),
                    symbols: vec![],
                    executable: true,
                    entry_point: true,
                },
                Module {
                    path: "helper.py".into(),
                    purpose: "Helper".into(),
                    symbols: vec![],
                    executable: false,
                    entry_point: false,
                },
                Module {
                    path: "notes.txt".into(),
                    purpose: "Optional migration notes".into(),
                    symbols: vec![],
                    executable: false,
                    entry_point: false,
                },
            ],
        };
        let helper = "print('after')\n";
        let entry = "import helper\n";
        let progress = serde_json::from_value(serde_json::json!({
            "outputs": [
                {"path":"helper.py", "content": helper},
                {"path":"notes.txt", "content": "Draft notes\n"},
                {"path":"entry.py", "content": entry}
            ],
            "repair": {"state":"complete"}
        }))
        .unwrap();
        let publication = library
            .prepare_modularization(
                "split",
                "python",
                "",
                [
                    (&plan.files[0], entry, Some(source)),
                    (&plan.files[1], helper, None),
                ],
            )
            .unwrap();
        let added = publication.ids().find(|id| *id != source).unwrap();
        let generation_profile = outbound
            .as_ref()
            .map_or_else(|| reference.clone(), |binding| binding.profile_id.clone());
        let mut job = Job {
            id: SnippetId::new().unwrap(),
            revision: 1,
            source_sha256,
            rendered_sha256: content_hash("approved rendered source").unwrap(),
            sources: vec![("original.py".into(), source)],
            plan,
            outbound: outbound.clone(),
            generation: Some(Generation {
                profile_id: generation_profile.clone(),
                approved_preview_sha256: content_hash("approved preview").unwrap(),
                code_text_language: CodeTextLanguage::English,
                instructions: String::new(),
                skills: String::new(),
                progress,
            }),
            publication: Some(publication.clone()),
            published: false,
        };
        let mut changed = job.clone();
        changed.generation.as_mut().unwrap().profile_id =
            ProfileRef::Policy("policy-snippetsplit".into());
        assert!(
            store
                .save_modularization(subject, pre, None, Some(changed))
                .is_err()
        );
        let mut expected = None;
        if outbound.is_some() {
            let mut invalid = job.clone();
            invalid.outbound.as_mut().unwrap().profile_revision = [0; 32];
            assert!(
                store
                    .save_modularization(subject, pre, None, Some(invalid))
                    .is_err()
            );

            let mut planning = job.clone();
            planning.generation = None;
            planning.publication = None;
            let history = store
                .save_modularization(subject, pre, None, Some(planning.clone()))
                .unwrap();
            expected = history
                .current()
                .unwrap()
                .modularization
                .as_deref()
                .map(Job::version);
            store
                .require_modularization(subject, pre, expected)
                .unwrap();
            assert!(store.require_modularization(subject, pre, None).is_err());
            for rebound in [
                None,
                Some(OutboundBinding {
                    kind: OutboundKind::Https,
                    profile_id: generation_profile.clone(),
                    profile_revision: [4; 32],
                }),
            ] {
                planning.outbound = rebound;
                assert!(
                    store
                        .save_modularization(subject, pre, expected, Some(planning.clone()))
                        .is_err()
                );
            }
        } else {
            let legacy = serde_json::to_value(&job).unwrap();
            assert!(legacy.get("outbound").is_none());
            job = serde_json::from_value(legacy).unwrap();
        }
        let history = store
            .save_modularization(subject, pre, expected, Some(job))
            .unwrap();
        let version = history
            .current()
            .unwrap()
            .modularization
            .as_deref()
            .unwrap()
            .version();
        store
            .require_modularization(subject, pre, Some(version))
            .unwrap();
        assert!(
            store
                .require_modularization(subject, pre, expected)
                .is_err()
        );

        // The library commit reached disk before the process could finish recording it in history.
        library.publish_modularization(&publication).unwrap();
        let mut reopened = Library::open(profile.path()).unwrap();
        let updated = store
            .commit_modularization(subject, pre, version, &mut reopened)
            .unwrap();
        let updated_job = updated.current().unwrap().modularization.as_ref().unwrap();
        assert!(updated_job.published);
        assert!(updated_job.outbound == outbound);
        assert_eq!(
            updated_job.generation.as_ref().unwrap().profile_id,
            generation_profile
        );
        assert_eq!(reopened.entries().len(), 2);
        assert_eq!(reopened.get(source).unwrap().content(), entry);
        assert_eq!(reopened.get(added).unwrap().content(), helper);
    }
}
