//! Local measurement for the skill blocks retained in a reviewed request.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, SyncSender},
};
use std::thread::{self, JoinHandle};

use pgp::crypto::hash::HashAlgorithm;

use crate::clipboard_policy::ClipboardPolicy;
use crate::local_ai::Tokenizer;

use super::statistics::{self, Current, Observation, Surface, Usage};
use super::{Skill, SkillGuidance, Store};
use crate::ai_manager::Configuration;

#[derive(Clone)]
pub(crate) struct SkillUsageReceipt(Arc<Measurement>);

struct Measurement {
    root: PathBuf,
    surface: Surface,
    usage: Vec<Usage>,
    recorded: AtomicBool,
}

enum Work {
    Record(Arc<Measurement>),
    Barrier(mpsc::Sender<()>),
}

struct Writer {
    send: SyncSender<Work>,
    thread: JoinHandle<()>,
}

static WRITER: Mutex<Option<Writer>> = Mutex::new(None);

fn writer() -> std::io::Result<Writer> {
    let (send, receive) = mpsc::sync_channel(256);
    let thread = thread::Builder::new()
        .name("kortty-skill-statistics".into())
        .spawn(move || {
            while let Ok(work) = receive.recv() {
                match work {
                    Work::Record(data) => data.write(),
                    Work::Barrier(done) => {
                        let _ = done.send(());
                    }
                }
            }
        })?;
    Ok(Writer { send, thread })
}

pub(in crate::ai_manager) fn flush() {
    let sender = WRITER
        .lock()
        .ok()
        .and_then(|writer| writer.as_ref().map(|writer| writer.send.clone()));
    if let Some(sender) = sender {
        let (done, wait) = mpsc::channel();
        if sender.send(Work::Barrier(done)).is_ok() {
            let _ = wait.recv();
        }
    }
}

pub(in crate::ai_manager) fn shutdown() {
    let writer = WRITER.lock().ok().and_then(|mut writer| writer.take());
    if let Some(Writer { send, thread }) = writer {
        drop(send);
        let _ = thread.join();
    }
}

impl Measurement {
    fn write(&self) {
        let _ = statistics::record(&self.root, self.surface, &self.usage, || {
            allowed_now(&self.root)
        });
    }
}

impl fmt::Debug for SkillUsageReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SkillUsageReceipt").finish_non_exhaustive()
    }
}

impl SkillUsageReceipt {
    pub(in crate::ai_manager) fn new(
        root: &Path,
        store: &Store,
        guidance: &SkillGuidance,
        tokenizer: Tokenizer,
        surface: Surface,
    ) -> Option<Self> {
        let mut retained = BTreeMap::new();
        for entry in &guidance.included {
            *retained.entry(entry.id).or_insert(false) |= entry.pinned;
        }
        let usage: Vec<_> = retained
            .into_iter()
            .filter_map(|(id, pinned)| {
                Some(Usage {
                    skill: observe(id, store.skills.get(&id)?, tokenizer)?,
                    pinned,
                    full_size: true,
                })
            })
            .collect();
        (!usage.is_empty()).then(|| {
            Self(Arc::new(Measurement {
                root: root.to_owned(),
                surface,
                usage,
                recorded: AtomicBool::new(false),
            }))
        })
    }

    pub(crate) fn record(self) {
        if self.0.recorded.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut current) = WRITER.lock() {
            if current.is_none() {
                *current = writer().ok();
            }
            if let Some(writer) = current.as_ref() {
                let _ = writer.send.try_send(Work::Record(self.0));
            }
        }
    }
}

pub(super) fn allowed_now(root: &Path) -> bool {
    Configuration::load_from(root)
        .is_ok_and(|config| config.skill_statistics_allowed(&ClipboardPolicy::load()))
}

pub(in crate::ai_manager) fn observe_sizes(root: &Path) {
    let Ok(config) = Configuration::load_from(root) else {
        return;
    };
    if !config.skill_statistics_allowed(&ClipboardPolicy::load()) {
        return;
    }
    let sizes: Vec<_> = config
        .skills
        .current_statistics(config.skill_statistics_tokenizer())
        .into_iter()
        .map(|row| row.skill)
        .collect();
    let _ = statistics::observe(root, &sizes, || allowed_now(root));
}

impl Store {
    pub(super) fn current_statistics(&self, tokenizer: Tokenizer) -> Vec<Current> {
        self.skills
            .iter()
            .filter_map(|(&id, skill)| {
                Some(Current {
                    skill: observe(id, skill, tokenizer)?,
                    enabled: skill.available(),
                    learned_rules: skill.rules.len(),
                })
            })
            .collect()
    }
}

fn fingerprint(skill: &Skill) -> Option<[u8; 32]> {
    let input = serde_json::to_vec(&(
        &skill.name,
        &skill.description,
        &skill.tags,
        &skill.content,
        &skill.rules,
    ))
    .ok()?;
    HashAlgorithm::Sha256.digest(&input).ok()?.try_into().ok()
}

fn observe(id: u64, skill: &Skill, tokenizer: Tokenizer) -> Option<Observation> {
    let fingerprint = fingerprint(skill)?;
    let mut counted = skill.content.clone();
    for rule in &skill.rules {
        if !rule.trim().is_empty() {
            counted.push_str("\n- ");
            counted.push_str(rule.trim());
        }
    }
    Some(Observation {
        id,
        name: skill.name.trim().to_owned(),
        tokens: tokenizer.count_text(&counted),
        tokenizer,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use crate::ai_manager::recorded_skill_uses_for_tests;
    use crate::local_ai::Profile;

    use super::*;
    use crate::ai_manager::skills::{MAX_PROMPT_BYTES, Target};

    #[test]
    fn measurements_keep_only_retained_skills_and_respect_saved_revocation() {
        let fixture = crate::snippets::tests::Profile::new();
        let root = fixture.0.canonicalize().unwrap();
        let baseline = Configuration::load_fixture_from(&root).unwrap();
        let mut config = baseline.clone();
        config.skills.skills.clear();
        config.skills.auto_detect = false;
        let mut add = |name: &str, content: String| {
            config
                .skills
                .insert(Skill {
                    name: name.into(),
                    content,
                    enabled: true,
                    ..Skill::default()
                })
                .unwrap()
        };
        let excluded = add("Large instructions", "x".repeat(MAX_PROMPT_BYTES));
        let pinned = add("Pinned skill", "Inspect changes".into());
        let automatic = add("Automatic skill", "Review commits".into());
        config.save_fixture_at(&root, &baseline).unwrap();
        let saved = Configuration::load_fixture_from(&root).unwrap();
        let guidance = saved
            .skills
            .selection("Review changes", Target::Chat, &[pinned, pinned])
            .reviewed_guidance(&[]);
        assert_eq!(guidance.included.len(), 2);
        assert!(
            guidance
                .included
                .iter()
                .any(|entry| entry.id == pinned && entry.pinned)
        );
        assert!(
            guidance
                .included
                .iter()
                .any(|entry| entry.id == automatic && !entry.pinned)
        );
        assert!(!guidance.included.iter().any(|entry| entry.id == excluded));
        let transport = Profile::new("127.0.0.1:11434", "fixture").unwrap();
        assert!(
            saved
                .skill_usage(&guidance, &transport, Surface::Chat, true)
                .is_none()
        );
        let request = saved
            .skill_usage(&guidance, &transport, Surface::Chat, false)
            .unwrap();
        request.clone().record();
        request.record();
        assert_eq!(recorded_skill_uses_for_tests(&root), (1, 1));

        let revoked = saved
            .skill_usage(&guidance, &transport, Surface::Chat, false)
            .unwrap();
        config.skills.usage_statistics = false;
        config.save_fixture_at(&root, &saved).unwrap();
        revoked.record();
        assert_eq!(recorded_skill_uses_for_tests(&root), (1, 1));
    }
}
