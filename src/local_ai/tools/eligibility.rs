//! Eligibility uses the original task, never terminal output or retrieved content.

pub(crate) fn agent_task_eligible(task: &str, always: bool) -> bool {
    let task = task.trim_matches(|ch| ch <= '\u{20}');
    if task.is_empty() {
        return false;
    }
    if always {
        return true;
    }
    let task = task
        .split(['\r', '\n'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if ["http://", "https://", "www."]
        .iter()
        .any(|signal| task.contains(signal))
    {
        return true;
    }
    let boundary = |ch: char| !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-';
    [
        "aktuell",
        "aktuelle",
        "aktuellen",
        "heute",
        "gestern",
        "morgen",
        "neueste",
        "neuste",
        "latest",
        "current",
        "recent",
        "news",
        "internet",
        "web",
        "online",
        "quelle",
        "quellen",
        "source",
        "sources",
        "search",
        "suche",
        "suchen",
        "google",
        "repository",
        "repositories",
        "repo",
        "repos",
        "download",
        "release",
        "version",
        "recherche",
        "recherchiere",
        "recherchieren",
        "research",
        "nachschlagen",
        "look up",
        "lookup",
        "webseite",
        "website",
        "homepage",
        "url",
        "link",
        "links",
        "dokumentation",
        "doku",
        "docs",
        "documentation",
        "handbuch",
        "manual",
        "changelog",
        "cve",
        "advisory",
        "sicherheitslücke",
        "vulnerability",
        "herunterladen",
        "pdf",
    ]
    .iter()
    .any(|signal| {
        task.match_indices(signal).any(|(offset, word)| {
            task[..offset].chars().next_back().is_none_or(boundary)
                && task[offset + word.len()..]
                    .chars()
                    .next()
                    .is_none_or(boundary)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_task_eligibility_uses_original_first_line_and_java_word_boundaries() {
        for task in [
            "Search documentation",
            " CURRENT release",
            "Read https://example.com",
            "ÖdocsÖ",
            "Find a CVE",
            "Sicherheitslücke nachschlagen",
            "look up the manual",
        ] {
            assert!(agent_task_eligible(task, false), "{task}");
        }
        for task in [
            "",
            " \r\n",
            "Inspect files\nsearch online",
            "Inspect files\rwww.example.com",
            "researcher",
            "my-docs",
            "docs_cache",
            "preview",
            "inspect",
            "User task: inspect\nContext: latest release",
        ] {
            assert!(!agent_task_eligible(task, false), "{task}");
        }
        assert!(agent_task_eligible("Inspect files", true));
        assert!(!agent_task_eligible("\t \r\n", true));
    }
}
