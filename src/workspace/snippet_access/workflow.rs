//! Opens generated workflow code through the ordinary unsaved editor routes.

use std::collections::HashSet;

use iced::{Task, window};

use super::{Message, SnippetWindow, Workspace, snippets};
use crate::snippets::diagrams::Saved;

impl Workspace {
    pub(in crate::workspace) fn open_file_comparison_snippet(
        &mut self,
        source: window::Id,
        content: std::sync::Arc<str>,
    ) -> Task<Message> {
        self.open_generated_snippet(source, |editor| {
            let input = snippets::Input {
                name: unique_name(editor.latest_library(), "File comparison draft")?,
                language: "text".into(),
                category: String::new(),
                content,
            };
            editor.seed_workflow(
                input,
                snippets::Metadata {
                    description: "Draft generated from a reviewed SFTP file comparison.".into(),
                    ..Default::default()
                },
            );
            Ok(())
        })
    }

    pub(in crate::workspace) fn open_workflow_snippet(
        &mut self,
        source: window::Id,
        input: snippets::Input,
        masked_goal: &str,
        detected_os: Option<&str>,
        diagrams: Vec<Saved>,
    ) -> Task<Message> {
        self.open_generated_snippet(source, |editor| {
            seed(editor, input, masked_goal, detected_os, diagrams)
        })
    }

    fn open_generated_snippet(
        &mut self,
        source: window::Id,
        seed_editor: impl FnOnce(&mut SnippetWindow) -> Result<(), String>,
    ) -> Task<Message> {
        if self.exiting || self.scheduler_draining {
            return Task::none();
        }
        let mut snippets = match snippets::profile_path().and_then(SnippetWindow::open) {
            Ok(snippets) => snippets,
            Err(_) => {
                self.notice(source, "Could not open the snippet library.".into());
                return Task::none();
            }
        };
        if let Err(error) = seed_editor(&mut snippets) {
            self.notice(source, error);
            return Task::none();
        }
        let owner = self
            .windows
            .contains_key(&source)
            .then_some(source)
            .or_else(|| self.windows.keys().copied().next())
            .unwrap_or(source);
        self.install_snippet_surface(owner, snippets, false).1
    }
}

fn seed(
    editor: &mut SnippetWindow,
    mut input: snippets::Input,
    masked_goal: &str,
    detected_os: Option<&str>,
    diagrams: Vec<Saved>,
) -> Result<(), String> {
    if !masked_goal
        .chars()
        .all(crate::session_journal::is_display_character)
    {
        return Err("The workflow goal contains invalid text.".into());
    }
    let metadata = snippets::Metadata {
        description: format!("Reproducible workflow for {}", masked_goal.trim()),
        operating_system: detected_os.and_then(operating_system).map(str::to_owned),
        tags: vec!["workflow".into()],
        diagrams,
        ..snippets::Metadata::default()
    };
    metadata.validate().map_err(|error| error.to_string())?;
    if metadata
        .diagrams
        .iter()
        .any(|diagram| !diagram.captured_for(&input.content))
    {
        return Err("A workflow diagram belongs to different source text. Regenerate it before opening the snippet.".into());
    }
    input.name = unique_name(editor.latest_library(), &input.name)?;
    editor.seed_workflow(input, metadata);
    Ok(())
}

fn unique_name(library: &snippets::Library, base: &str) -> Result<String, String> {
    let names: HashSet<_> = library
        .entries()
        .iter()
        .map(|entry| entry.name().to_lowercase())
        .collect();
    if !names.contains(&base.to_lowercase()) {
        return Ok(base.into());
    }
    let (stem, extension) = match base.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < base.len() => base.split_at(dot),
        _ => (base, ""),
    };
    for number in 2..=names.len() + 1 {
        let suffix = format!("_{number}{extension}");
        let Some(available) = snippets::MAX_NAME_BYTES.checked_sub(suffix.len()) else {
            break;
        };
        let prefix = stem.floor_char_boundary(stem.len().min(available));
        let name = format!("{}{suffix}", &stem[..prefix]);
        if !names.contains(&name.to_lowercase()) {
            return Ok(name);
        }
    }
    Err("Could not assign a unique workflow snippet name.".into())
}

fn operating_system(raw: &str) -> Option<&'static str> {
    let raw = raw.to_ascii_lowercase();
    if ["windows", "mingw", "msys", "cygwin"]
        .into_iter()
        .any(|name| raw.contains(name))
    {
        return Some("Windows");
    }
    if ["darwin", "mac os", "macos", "os x"]
        .into_iter()
        .any(|name| raw.contains(name))
    {
        return Some("MacOS");
    }
    let mut words = raw.split(|ch: char| !ch.is_ascii_alphanumeric());
    if raw.contains("linux")
        || raw.contains("red hat")
        || words.any(|word| {
            matches!(
                word,
                "ubuntu"
                    | "debian"
                    | "fedora"
                    | "redhat"
                    | "rhel"
                    | "centos"
                    | "suse"
                    | "arch"
                    | "alpine"
                    | "rocky"
                    | "almalinux"
                    | "gentoo"
                    | "manjaro"
                    | "mint"
            )
        })
    {
        return Some("Linux");
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use iced::widget::text_editor;

    use crate::diagram_window::Chart;
    use crate::snippet_window::{Message, Update};
    use crate::snippets::{diagrams, tests::Profile};

    use super::*;

    fn input(name: &str, code: &str) -> snippets::Input {
        snippets::Input {
            name: name.into(),
            language: "bash".into(),
            category: "Agent workflows".into(),
            content: code.into(),
        }
    }

    fn diagram(code: &str, scope: diagrams::Scope) -> Saved {
        let source = diagrams::Source::capture(code, scope).unwrap();
        let chart = Chart::stored(
            kortty_diagram::parse_mermaid("flowchart TD\nstart[Run] --> done[Done]").unwrap(),
            Default::default(),
            false,
            None,
        );
        Saved::capture(&chart, &source, "Workflow chart", "", None).unwrap()
    }

    #[test]
    fn workflow_snippet_metadata_and_diagram_persist_only_after_explicit_save() {
        let profile = Profile::new();
        let code = "#!/usr/bin/env bash\nprintf ready";
        let mut library = snippets::Library::open(profile.path()).unwrap();
        library
            .save(None, input("INSPECT.SH", "echo original"))
            .unwrap();
        library
            .save(None, input("inspect_2.sh", "echo second"))
            .unwrap();
        let mut editor = SnippetWindow::open(profile.path()).unwrap();
        let chart = diagram(code, diagrams::Scope::Whole);
        seed(
            &mut editor,
            input("inspect.sh", code),
            "Inspect services password=***",
            Some("Fedora Linux 44 (Workstation Edition)"),
            vec![chart.clone()],
        )
        .unwrap();
        assert!(editor.dirty());
        assert!(!editor.draft_autosave_pending());
        assert!(
            editor
                .due_drafts(Instant::now() + Duration::from_secs(5))
                .is_empty()
        );
        assert_eq!(
            snippets::Library::open(profile.path())
                .unwrap()
                .entries()
                .len(),
            2
        );
        assert_eq!(editor.update(Message::Save), Update::Changed);
        let saved = snippets::Library::open(profile.path()).unwrap();
        let saved = saved
            .entries()
            .iter()
            .find(|entry| entry.name() == "inspect_3.sh")
            .unwrap();
        assert_eq!(saved.content(), code);
        assert_eq!(saved.language(), "bash");
        assert_eq!(
            saved.metadata().description,
            "Reproducible workflow for Inspect services password=***"
        );
        assert_eq!(saved.metadata().tags, ["workflow"]);
        assert_eq!(saved.metadata().operating_system.as_deref(), Some("Linux"));
        assert_eq!(saved.metadata().diagrams, [chart]);
        assert!(saved.metadata().diagrams[0].is_current(saved.content()));
        editor.sync_recovery(&[], &[]);
        let _ = editor.update(Message::Name("Renamed after saving".into()));
        assert!(editor.draft_autosave_pending());
    }

    #[test]
    fn workflow_snippet_omits_a_diagram_when_the_unsaved_source_changes() {
        let profile = Profile::new();
        let code = "echo first\necho selected";
        let chart = diagram(code, diagrams::Scope::Lines { start: 2, end: 2 });
        let mut editor = SnippetWindow::open(profile.path()).unwrap();
        assert!(
            seed(
                &mut editor,
                input("workflow.sh", "echo another\necho selected"),
                "Inspect",
                None,
                vec![chart.clone()],
            )
            .is_err()
        );
        seed(
            &mut editor,
            input("workflow.sh", code),
            "Inspect",
            None,
            vec![chart],
        )
        .unwrap();
        let _ = editor.update(Message::Edit(text_editor::Action::Edit(
            text_editor::Edit::Paste(Arc::new("# User edit\n".into())),
        )));
        assert!(!editor.draft_autosave_pending());
        assert!(
            editor
                .due_drafts(Instant::now() + Duration::from_secs(5))
                .is_empty()
        );
        assert_eq!(editor.update(Message::Save), Update::Changed);
        let saved = snippets::Library::open(profile.path()).unwrap();
        let saved = &saved.entries()[0];
        assert!(saved.content().contains("# User edit"));
        assert!(saved.metadata().diagrams.is_empty());
        assert!(saved.metadata().operating_system.is_none());
    }

    #[test]
    fn workflow_snippet_uses_only_recognized_probe_operating_systems() {
        assert_eq!(operating_system("Darwin 25.0 arm64"), Some("MacOS"));
        assert_eq!(operating_system("MSYS_NT-10.0 x86_64"), Some("Windows"));
        assert_eq!(operating_system("Ubuntu 22.04"), Some("Linux"));
        assert_eq!(operating_system("FreeBSD aarch64"), None);
        assert_eq!(operating_system(""), None);
    }
}
