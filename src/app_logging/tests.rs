use super::*;

use toml_edit::DocumentMut;

fn policy(source: &str) -> io::Result<Policy> {
    let document = source.parse::<DocumentMut>().unwrap();
    let mut policy = Policy::default();
    if let Some(rules) = document.get("rule") {
        for rule in rules.as_array_of_tables().unwrap() {
            policy.read_rule(rule)?;
        }
    }
    Ok(policy)
}

#[test]
fn app_logging_defaults_and_directory_resolution_match_the_reference() {
    let settings: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings.retention_days, 7);
    let profile = std::env::current_dir().unwrap().join("test-profile");
    assert_eq!(settings.directory(&profile).unwrap(), profile.join("logs"));
    let mut configured = settings.clone();
    configured.directory = "logs/../diagnostics".into();
    assert_eq!(
        configured.directory(&profile).unwrap(),
        std::env::current_dir().unwrap().join("diagnostics")
    );
    configured.directory = "~/diagnostics".into();
    assert_eq!(
        configured.directory(&profile).unwrap(),
        std::env::home_dir().unwrap().join("diagnostics")
    );
    configured.directory = String::new();
    configured.retention_days = 0;
    assert!(configured.validate().is_ok());
    configured.retention_days = 3651;
    assert!(configured.validate().is_err());
    configured.retention_days = 7;
    configured.directory = "invalid\nfolder".into();
    assert!(configured.validate().is_err());
}

#[test]
fn app_logging_policy_resolves_all_six_fields_before_the_writer_starts() {
    let profile = std::env::current_dir().unwrap().join("test-profile");
    let managed = policy(
        r#"
[[rule]]
[rule.logging]
directory = "z-log"
retention-days = 0
compress = false
format = "text"
rotation-max-files = 0
rotation-total-size-mb = 40
[[rule]]
[rule.logging]
directory = "a-log"
retention-days = 3
compress = true
format = "JSON"
rotation-max-files = 9
rotation-total-size-mb = 0
[[rule]]
[rule.logging]
retention-days = 10
rotation-max-files = 12
rotation-total-size-mb = 20
"#,
    )
    .unwrap();
    let effective = managed.effective(&Settings::default(), &profile).unwrap();
    assert_eq!(
        effective.directory,
        std::env::current_dir().unwrap().join("a-log")
    );
    assert_eq!(effective.retention_days, 3);
    assert!(effective.compress);
    assert_eq!(effective.format, Format::Json);
    assert_eq!(effective.rotation_max_files, 9);
    assert_eq!(effective.rotation_total_size_mb, 20);
    assert_eq!(managed.directory(), Some(Path::new("a-log")));
    assert_eq!(managed.retention_days(), Some(3));
    assert_eq!(managed.compress(), Some(true));
}

#[test]
fn app_logging_policy_rejects_invalid_types_and_caps() {
    for field in [
        "directory = false",
        "retention-days = -1",
        "retention-days = 2.5",
        "compress = 1",
        "format = 'xml'",
        "rotation-max-files = 2147483648",
        "rotation-total-size-mb = -1",
    ] {
        assert!(
            policy(&format!("[[rule]]\n[rule.logging]\n{field}\n")).is_err(),
            "{field}"
        );
    }
}

#[test]
fn preparing_the_next_writer_preserves_a_retired_writer_error() {
    let profile = std::env::temp_dir().join(format!(
        "kortty-app-log-retirement-{}",
        crate::private_files::nonce().unwrap()
    ));
    std::fs::create_dir(&profile).unwrap();
    // Test double for a retired worker's failed completion. The replacement
    // writer below still opens and drains a real private log file.
    let (complete, done) = async_channel::bounded(1);
    complete
        .try_send(Err("test writer sync failure".into()))
        .unwrap();
    drop(complete);
    let mut runtime = Runtime {
        initialized: true,
        retired: vec![done],
        ..Runtime::default()
    };
    let candidate = runtime
        .prepare(&Settings::default(), &Policy::default(), &profile)
        .unwrap();
    runtime.commit(candidate);
    let problem = runtime.problem.clone();
    for done in runtime.stop() {
        done.recv_blocking().unwrap().unwrap();
    }
    std::fs::remove_dir_all(&profile).unwrap();
    assert!(problem.is_some_and(|problem| problem.contains("test writer sync failure")));
}

struct LoggingScratch {
    profile: PathBuf,
    runtime: Runtime,
}

impl LoggingScratch {
    fn new() -> io::Result<Self> {
        let folder = std::env::temp_dir().join(format!(
            "kortty-app-log-identity-{}",
            crate::private_files::nonce()?
        ));
        std::fs::create_dir(&folder)?;
        let profile = std::fs::canonicalize(&folder).inspect_err(|_| {
            let _ = std::fs::remove_dir(&folder);
        })?;
        let mut scratch = Self {
            profile,
            runtime: Runtime::default(),
        };
        scratch.runtime = Runtime::initialize(
            &scratch.settings("ActiveLogs"),
            &Policy::default(),
            &scratch.profile,
        );
        if let Some(problem) = &scratch.runtime.problem {
            return Err(io::Error::other(problem.clone()));
        }
        Ok(scratch)
    }

    fn settings(&self, name: &str) -> Settings {
        Settings {
            directory: self.profile.join(name).to_str().unwrap().to_owned(),
            retention_days: 28,
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        let mut error = None;
        for done in self.runtime.stop() {
            let result = done
                .recv_blocking()
                .map_err(io::Error::other)
                .and_then(|result| result.map_err(io::Error::other));
            if let Err(problem) = result {
                error.get_or_insert(problem);
            }
        }
        error.map_or(Ok(()), Err)
    }
}

impl Drop for LoggingScratch {
    fn drop(&mut self) {
        let _ = self.finish();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

#[test]
fn app_logging_directory_alias_reuses_active_writer() -> io::Result<()> {
    use crate::private_files::Directory;

    let mut scratch = LoggingScratch::new()?;
    let active = Directory::open(&scratch.profile.join("ActiveLogs"), false)?;
    let alias = match Directory::open(&scratch.profile.join("activelogs"), false) {
        Ok(alias) => alias,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("Case alias requires a case-insensitive test directory");
            return scratch.finish();
        }
        Err(error) => return Err(error),
    };
    assert!(active.same_as(&alias)?);
    let settings = scratch.settings("activelogs");
    let candidate = scratch
        .runtime
        .prepare(&settings, &Policy::default(), &scratch.profile)
        .map_err(|error| {
            io::Error::other(format!(
                "Preparing the same log directory by its case alias failed: {error}"
            ))
        })?
        .expect("changing the configured path prepares a candidate");
    let reused = candidate.replacement.is_none();
    scratch.runtime.commit(Some(candidate));
    scratch.runtime.record(Event::WindowClosed);
    scratch.finish()?;
    assert!(reused, "an alias must keep the active writer");
    assert!(
        std::fs::read_to_string(scratch.profile.join("ActiveLogs/kortty.log"))?
            .contains("Application window closed")
    );
    Ok(())
}

#[test]
fn app_logging_distinct_directory_replaces_active_writer() -> io::Result<()> {
    use crate::private_files::Directory;

    let mut scratch = LoggingScratch::new()?;
    let settings = scratch.settings("SeparateLogs");
    let candidate = scratch
        .runtime
        .prepare(&settings, &Policy::default(), &scratch.profile)?
        .expect("changing to a distinct directory prepares a candidate");
    let replaced = candidate.replacement.is_some();
    scratch.runtime.commit(Some(candidate));
    let active = Directory::open(&scratch.profile.join("ActiveLogs"), false)?;
    let separate = Directory::open(&scratch.profile.join("SeparateLogs"), false)?;
    assert!(!active.same_as(&separate)?);
    scratch.runtime.record(Event::WindowClosed);
    scratch.finish()?;
    assert!(replaced);
    assert!(
        std::fs::read_to_string(scratch.profile.join("ActiveLogs/kortty.log"))?
            .contains("Application started")
    );
    assert!(
        std::fs::read_to_string(scratch.profile.join("SeparateLogs/kortty.log"))?
            .contains("Application window closed")
    );
    Ok(())
}
