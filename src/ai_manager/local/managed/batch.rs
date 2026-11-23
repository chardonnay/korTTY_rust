//! Reviewed selections and lifecycle operations across the two model registries.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::local_ai::{managed_models, mlx_models, runtime_package};

use super::check_cancelled;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReviewedModel {
    Gguf(managed_models::Model),
    Mlx(mlx_models::Model),
}

impl ReviewedModel {
    pub(in crate::ai_manager) fn name(&self) -> &str {
        match self {
            Self::Gguf(model) => &model.name,
            Self::Mlx(model) => &model.name,
        }
    }

    pub(super) fn source_kind(&self) -> &'static str {
        match self {
            Self::Gguf(model) if model.external.is_some() => "External GGUF registration",
            Self::Gguf(_) => "Private GGUF copy",
            Self::Mlx(_) => "Private MLX copy",
        }
    }

    pub(super) fn remove(
        &self,
        profile: &Path,
        gguf: &managed_models::Registry,
        cancelled: &AtomicBool,
    ) -> Result<String, String> {
        match self {
            Self::Gguf(model) => runtime_package::with_stopped_model(profile, &model.id, || {
                check_cancelled(cancelled)?;
                gguf.remove_reviewed(model)
            }),
            Self::Mlx(model) => runtime_package::with_stopped_mlx_model(profile, &model.id, || {
                check_cancelled(cancelled)?;
                mlx_models::Registry::new(profile).remove_reviewed(model)
            }),
        }
        .map_err(|error| error.to_string())?;
        Ok(match self {
            Self::Gguf(model) if model.external.is_some() => {
                format!("Removed the external registration for {}.", model.name)
            }
            Self::Gguf(model) => format!("Removed the managed copy of {}.", model.name),
            Self::Mlx(model) => format!("Removed MLX model {}.", model.name),
        })
    }
}

pub(super) fn start_models(
    profile: &Path,
    models: &[ReviewedModel],
    cancelled: &AtomicBool,
) -> String {
    let gguf = models
        .iter()
        .any(|model| matches!(model, ReviewedModel::Gguf(_)))
        .then(|| managed_models::Registry::new(profile.to_owned()).list())
        .transpose();
    let mlx = models
        .iter()
        .any(|model| matches!(model, ReviewedModel::Mlx(_)))
        .then(|| mlx_models::Registry::new(profile).list())
        .transpose();
    let run = |model: &ReviewedModel| {
        check_cancelled(cancelled).map_err(|error| error.to_string())?;
        match model {
            ReviewedModel::Gguf(model) => {
                require_current(&gguf, model)?;
                runtime_package::start_model(profile, &model.id, cancelled)?;
            }
            ReviewedModel::Mlx(model) => {
                require_current(&mlx, model)?;
                crate::local_ai::start_mlx_model(profile, &model.id, cancelled)?;
            }
        }
        Ok(format!("Started {}.", model.name()))
    };
    let results = start_parallel(models, cancelled, run);
    report_changes(models, "Started", ReviewedModel::name, results, cancelled)
}

fn require_current<T: PartialEq>(
    current: &std::io::Result<Option<Vec<T>>>,
    model: &T,
) -> Result<(), String> {
    if current
        .as_ref()
        .map_err(ToString::to_string)?
        .as_ref()
        .is_some_and(|entries| entries.contains(model))
    {
        Ok(())
    } else {
        Err("The model registration changed. Refresh and select it again.".into())
    }
}

fn start_parallel<T: Sync>(
    models: &[T],
    cancelled: &AtomicBool,
    start: impl Fn(&T) -> Result<String, String> + Sync,
) -> Vec<Result<String, String>> {
    const PARALLEL_STARTS: usize = 2;
    let mut results = Vec::with_capacity(models.len());
    for group in models.chunks(PARALLEL_STARTS) {
        if cancelled.load(Ordering::Acquire) {
            break;
        }
        if let [model] = group {
            results.push(start(model));
            continue;
        }
        std::thread::scope(|scope| {
            let start = &start;
            let workers = group
                .iter()
                .map(|model| {
                    std::thread::Builder::new()
                        .name("kortty-model-warmup".into())
                        .spawn_scoped(scope, move || start(model))
                })
                .collect::<Vec<_>>();
            for worker in workers {
                results.push(match worker {
                    Ok(worker) => worker.join().unwrap_or_else(|_| {
                        Err("Model startup worker stopped unexpectedly".into())
                    }),
                    Err(error) => Err(format!("Could not start a model worker ({error})")),
                });
            }
        });
    }
    results
}

pub(super) fn change_models<T>(
    models: &[T],
    verb: &str,
    cancelled: &AtomicBool,
    name: impl Fn(&T) -> &str,
    mut change: impl FnMut(&T) -> Result<String, String>,
) -> String {
    let results = models
        .iter()
        .take_while(|_| !cancelled.load(Ordering::Acquire))
        .map(&mut change);
    report_changes(models, verb, name, results, cancelled)
}

fn report_changes<T>(
    models: &[T],
    verb: &str,
    name: impl Fn(&T) -> &str,
    results: impl IntoIterator<Item = Result<String, String>>,
    cancelled: &AtomicBool,
) -> String {
    let mut completed = 0;
    let mut failed = 0;
    let mut first_error = None;
    for (model, result) in models.iter().zip(results) {
        match result {
            Ok(notice) if models.len() == 1 => return notice,
            Ok(_) => completed += 1,
            Err(error) => {
                failed += 1;
                first_error.get_or_insert_with(|| format!("{}: {error}", name(model)));
            }
        }
    }
    let mut notice = format!("{verb} {completed} of {} models.", models.len());
    if let Some(error) = first_error {
        notice.push_str(&format!(" Failed: {failed}. {error}"));
    }
    if cancelled.load(Ordering::Acquire) {
        notice.push_str(" Operation cancelled.");
    }
    notice
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn start_warms_two_models_concurrently_and_preserves_result_order() {
        let entered = AtomicUsize::new(0);
        let cancelled = AtomicBool::new(false);
        let results = start_parallel(&["first", "second", "third"], &cancelled, |name| {
            let count = entered.fetch_add(1, Ordering::AcqRel);
            if count < 2 {
                let deadline = Instant::now() + Duration::from_secs(5);
                while entered.load(Ordering::Acquire) < 2 {
                    if Instant::now() >= deadline {
                        return Err("The second startup did not run concurrently".into());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Ok((*name).to_owned())
        });
        assert_eq!(
            results,
            [Ok("first".into()), Ok("second".into()), Ok("third".into())]
        );
    }
}
