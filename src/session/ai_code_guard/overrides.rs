use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, LazyLock, PoisonError, RwLock};

use crate::private_files::{Directory, FileStamp, require_single_link};

use super::{
    package_kind,
    rules::{Kind, RuleSet},
};

const DIRECTORY: &str = "coding-agents";
const MAX_RULE_FILE: u64 = 256 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 256;

type RuleOverrides = HashMap<Kind, Arc<RuleSet>>;
static ACTIVE: LazyLock<RwLock<RuleOverrides>> = LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct OverrideProblem {
    pub(crate) file: String,
    pub(crate) message: String,
}

/// Read a complete configuration snapshot for the active profile at startup.
/// Files missing or invalid in this snapshot use their bundled rules.
pub(crate) fn reload_overrides(profile: &Path) -> Vec<OverrideProblem> {
    let (loaded, rejected) = load(profile);
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = loaded;
    rejected
}

pub(super) fn for_kind(kind: Kind) -> Option<Arc<RuleSet>> {
    ACTIVE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&kind)
        .cloned()
}

fn open_directory(profile: &Path) -> io::Result<Directory> {
    // The application lease already anchors the private profile root.
    // Allow conventional editor-created rules within it without requiring a
    // particular file mode, but resolve every child without following links.
    Directory::open(profile, false)?.subdirectory(DIRECTORY, false)
}

fn load(profile: &Path) -> (RuleOverrides, Vec<OverrideProblem>) {
    let directory = match open_directory(profile) {
        Ok(directory) => directory,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return (HashMap::new(), Vec::new());
        }
        Err(_) => return directory_problem("Could not open the rules directory safely."),
    };
    let Ok(mut names) = directory.names(MAX_DIRECTORY_ENTRIES) else {
        return directory_problem("Could not list the rules directory within the file limit.");
    };
    names.sort();
    let mut loaded = HashMap::new();
    let mut rejected = Vec::new();
    for name in names {
        let Some(name) = name.to_str().filter(|name| rule_filename(name)) else {
            continue;
        };
        let Some(kind) = name.strip_suffix(".json").and_then(package_kind) else {
            rejected.push(OverrideProblem {
                file: name.into(),
                message: "The file name does not identify a supported coding agent.".into(),
            });
            continue;
        };
        match read_rule(&directory, name, kind) {
            Ok(rules) => {
                loaded.insert(kind, Arc::new(rules));
            }
            Err(message) => rejected.push(OverrideProblem {
                file: name.into(),
                message,
            }),
        }
    }
    if !open_directory(profile)
        .and_then(|current| directory.same_as(&current))
        .unwrap_or(false)
    {
        return directory_problem("The rules directory changed while loading.");
    }
    (loaded, rejected)
}

fn directory_problem(message: &str) -> (RuleOverrides, Vec<OverrideProblem>) {
    (
        HashMap::new(),
        vec![OverrideProblem {
            file: DIRECTORY.into(),
            message: message.into(),
        }],
    )
}

fn rule_filename(name: &str) -> bool {
    name.ends_with(".json")
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

fn read_rule(directory: &Directory, name: &str, kind: Kind) -> Result<RuleSet, String> {
    let file = directory
        .open_file(name)
        .map_err(|_| "The rule must be a readable regular file without symbolic links.")?;
    require_single_link(&file).map_err(|_| "The rule must have one hard link.")?;
    let stamp = FileStamp::read(&file).map_err(|_| "Could not verify the rule file.")?;
    let mut bytes = Vec::new();
    (&file)
        .take(MAX_RULE_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the rule file.")?;
    if bytes.len() as u64 > MAX_RULE_FILE {
        return Err("The rule file exceeds 256 KiB.".into());
    }
    let rules = RuleSet::from_override(kind, &bytes)?;
    stamp
        .require_unchanged(&file)
        .and_then(|()| stamp.require_unchanged(&directory.open_file(name)?))
        .map_err(|_| "The rule file changed while loading.")?;
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use kortty_terminal::Terminal;

    use super::*;
    use crate::session::ai_code_guard::rules::{State, matched_state_with};

    const OVERRIDE: &str = r#"{"kind":"CODEX","version":1,"fallbackState":null,"rules":[
        {"id":"site-approval","state":"BLOCKED","priority":10,"region":{},"anyLineRegex":["^Review local action\\?$"]},
        {"id":"later","state":"IDLE","priority":10,"contains":["Review local action?"]},
        {"id":"other","state":"IDLE","priority":-1,"contains":["Review local action?"]}
    ]}"#;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> io::Result<Self> {
            let path = std::env::temp_dir().canonicalize()?.join(format!(
                "kortty-coding-agent-{}",
                crate::private_files::nonce()?
            ));
            fs::create_dir(&path)?;
            Ok(Self(path))
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn state(text: &str, rules: &RuleOverrides) -> Option<State> {
        let mut terminal = Terminal::new(80, 24);
        terminal.advance(text.as_bytes());
        matched_state_with(
            &terminal.snapshot(),
            Kind::Codex,
            rules.get(&Kind::Codex).map(Arc::as_ref),
        )
    }

    #[test]
    fn coding_agent_override_replaces_whole_rule_set_and_falls_back_on_invalid_or_missing_file()
    -> io::Result<()> {
        let profile = TestDirectory::new()?;
        let dir = profile.0.join(DIRECTORY);
        fs::create_dir(&dir)?;
        let file = dir.join("codex.json");
        fs::write(&file, OVERRIDE)?;
        let (rules, problems) = load(&profile.0);
        assert!(problems.is_empty());
        assert_eq!(state("Review local action?", &rules), Some(State::Blocked));
        assert_eq!(state("Allow command?", &rules), None);

        for invalid in [
            OVERRIDE.replace("CODEX", "GEMINI_CLI"),
            OVERRIDE.replace("site-approval", "later"),
            OVERRIDE.replace("BLOCKED", "UNKNOWN"),
            OVERRIDE.replace("^Review local action\\\\?$", "("),
            OVERRIDE.replace("\"region\":{}", "\"region\":{\"unknown\":true}"),
            OVERRIDE.replace("\"priority\":10", "\"priority\":10,\"priority\":12"),
            OVERRIDE.replace(",\"anyLineRegex\":[\"^Review local action\\\\?$\"]", ""),
            " ".repeat(MAX_RULE_FILE as usize + 1),
        ] {
            fs::write(&file, invalid)?;
            let (rules, problems) = load(&profile.0);
            assert!(rules.is_empty());
            assert_eq!(problems.len(), 1);
            assert_eq!(problems[0].file, "codex.json");
            assert_eq!(state("Review local action?", &rules), None);
            assert_eq!(state("Allow command?", &rules), Some(State::Blocked));
        }
        fs::remove_file(file)?;
        let (rules, problems) = load(&profile.0);
        assert!(rules.is_empty() && problems.is_empty());
        assert_eq!(state("Allow command?", &rules), Some(State::Blocked));
        Ok(())
    }

    #[test]
    fn coding_agent_override_ignores_unrelated_entries_and_refuses_linked_rules() -> io::Result<()>
    {
        let profile = TestDirectory::new()?;
        let dir = profile.0.join(DIRECTORY);
        fs::create_dir(&dir)?;
        fs::write(dir.join("notes.txt"), "unrelated notes")?;
        let file = profile.0.join("outside.json");
        fs::write(&file, OVERRIDE)?;
        fs::hard_link(&file, dir.join("codex.json"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&file, dir.join("gemini-cli.json"))?;
        let (rules, problems) = load(&profile.0);
        assert!(rules.is_empty());
        let names = problems
            .iter()
            .map(|problem| problem.file.as_str())
            .collect::<Vec<_>>();
        #[cfg(unix)]
        assert_eq!(names, ["codex.json", "gemini-cli.json"]);
        #[cfg(windows)]
        assert_eq!(names, ["codex.json"]);
        Ok(())
    }
}
