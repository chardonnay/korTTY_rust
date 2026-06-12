use crate::model::ai::{AiSkill, AiSkillTarget};
use anyhow::{bail, Result};
use std::collections::HashMap;
use std::path::Path;

const MARKER_KEY: &str = "kortty-ai-skill";
const FORMAT_VERSION: &str = "1";

pub fn normalize_skill(skill: &mut AiSkill) {
    if skill.id.trim().is_empty() {
        skill.id = uuid::Uuid::new_v4().to_string();
    }
    skill.name = skill.name.trim().to_string();
    skill.description = skill
        .description
        .as_ref()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let mut tags = Vec::new();
    for tag in &skill.tags {
        let trimmed = tag.trim();
        if !trimmed.is_empty()
            && !tags
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(trimmed))
        {
            tags.push(trimmed.to_string());
        }
    }
    skill.tags = tags;
}

pub fn export_skill_to_markdown(skill: &AiSkill) -> String {
    let mut skill = skill.clone();
    normalize_skill(&mut skill);
    let mut markdown = String::new();
    markdown.push_str("---\n");
    markdown.push_str(&format!("{MARKER_KEY}: {FORMAT_VERSION}\n"));
    markdown.push_str(&format!(
        "name: {}\n",
        quote_front_matter(non_blank(&skill.name, "AI Skill"))
    ));
    if let Some(description) = skill
        .description
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        append_optional_front_matter(&mut markdown, "description", description);
    }
    if !skill.tags.is_empty() {
        let tags = skill
            .tags
            .iter()
            .map(|tag| quote_front_matter(tag))
            .collect::<Vec<_>>()
            .join(", ");
        markdown.push_str(&format!("tags: [{tags}]\n"));
    }
    markdown.push_str(&format!("enabled: {}\n", skill.enabled));
    markdown.push_str(&format!("target: {:?}\n", skill.target).to_uppercase());
    markdown.push_str("---\n\n");
    markdown.push_str(&skill.content);
    markdown
}

pub fn import_skill_from_markdown(path: Option<&Path>, text: &str) -> Result<AiSkill> {
    let parsed = parse_markdown(text);
    let fallback_name = path
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .map(|name| strip_markdown_extension(&name))
        .unwrap_or_else(|| "AI Skill".into());
    let mut skill = AiSkill::default();

    if parsed.front_matter.is_empty() {
        skill.name = fallback_name;
        skill.enabled = false;
        skill.target = AiSkillTarget::Both;
        skill.content = text.to_string();
        return Ok(skill);
    }

    let is_kortty_skill = parsed
        .front_matter
        .get(MARKER_KEY)
        .is_some_and(|value| value == FORMAT_VERSION);
    skill.name = parsed
        .front_matter
        .get("name")
        .cloned()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(fallback_name);
    skill.description = parsed.front_matter.get("description").cloned();
    skill.tags = parse_tags(parsed.front_matter.get("tags").map(String::as_str));
    skill.enabled = if is_kortty_skill {
        parse_enabled(parsed.front_matter.get("enabled").map(String::as_str))?
    } else {
        false
    };
    skill.target = if is_kortty_skill {
        parse_target(parsed.front_matter.get("target").map(String::as_str))?
    } else {
        AiSkillTarget::Both
    };
    skill.content = parsed.body;
    normalize_skill(&mut skill);
    Ok(skill)
}

pub fn append_skills_to_prompt(
    system_prompt: &str,
    user_prompt: &str,
    skills: &[AiSkill],
    target: AiSkillTarget,
    pinned_skill_ids: &[String],
) -> (String, Vec<String>) {
    let selected = select_relevant_skills(user_prompt, skills, target, pinned_skill_ids);
    if selected.is_empty() {
        return (system_prompt.trim().to_string(), Vec::new());
    }
    let mut prompt = system_prompt.trim().to_string();
    prompt.push_str("\n\nRelevant KorTTY AI skills:\n");
    for skill in &selected {
        prompt.push_str("\n### ");
        prompt.push_str(non_blank(&skill.name, "AI Skill"));
        if let Some(description) = skill
            .description
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            prompt.push('\n');
            prompt.push_str(description.trim());
        }
        prompt.push('\n');
        prompt.push_str(skill.content.trim());
        prompt.push('\n');
    }
    (
        prompt,
        selected
            .into_iter()
            .map(|skill| skill.name.clone())
            .collect::<Vec<_>>(),
    )
}

/// Selects skills for the prompt: candidates pass the relevance auto-detection,
/// while pinned skills (e.g. assigned to the active connection) always survive
/// it. Port of the Java `AiSkillRelevanceSelector.withPinnedSkills` behavior.
pub fn select_relevant_skills<'a>(
    user_prompt: &str,
    skills: &'a [AiSkill],
    target: AiSkillTarget,
    pinned_skill_ids: &[String],
) -> Vec<&'a AiSkill> {
    let query = user_prompt.to_lowercase();
    let candidates: Vec<&AiSkill> = skills
        .iter()
        .filter(|skill| skill.enabled)
        .filter(|skill| skill_applicable(skill, &target, pinned_skill_ids))
        .collect();

    let mut selected: Vec<&AiSkill> = candidates
        .iter()
        .copied()
        .filter(|skill| {
            let name_match =
                !skill.name.trim().is_empty() && query.contains(&skill.name.to_lowercase());
            let tag_match = skill
                .tags
                .iter()
                .any(|tag| !tag.trim().is_empty() && query.contains(&tag.to_lowercase()));
            let desc_match = skill
                .description
                .as_ref()
                .is_some_and(|description| keyword_overlap(&query, description));
            name_match || tag_match || desc_match
        })
        .take(5)
        .collect();

    // Pinned skills always survive the relevance auto-detection.
    for candidate in &candidates {
        if is_pinned(candidate, pinned_skill_ids)
            && !selected.iter().any(|skill| skill.id == candidate.id)
        {
            selected.push(candidate);
        }
    }
    selected
}

fn is_pinned(skill: &AiSkill, pinned_skill_ids: &[String]) -> bool {
    !skill.id.trim().is_empty()
        && pinned_skill_ids
            .iter()
            .any(|pinned| pinned.trim() == skill.id.trim())
}

fn skill_applicable(
    skill: &AiSkill,
    requested: &AiSkillTarget,
    pinned_skill_ids: &[String],
) -> bool {
    // Connection-scoped skills are only sent for connections they are explicitly
    // assigned to (= pinned); the generic auto-detection never picks them up.
    if matches!(skill.target, AiSkillTarget::Connection) {
        return is_pinned(skill, pinned_skill_ids);
    }
    target_applies(&skill.target, requested)
}

fn target_applies(skill_target: &AiSkillTarget, requested: &AiSkillTarget) -> bool {
    matches!(skill_target, AiSkillTarget::Both)
        || matches!(requested, AiSkillTarget::Both)
        || skill_target == requested
}

fn keyword_overlap(query: &str, text: &str) -> bool {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| word.len() >= 4)
        .any(|word| query.contains(&word.to_lowercase()))
}

struct ParsedMarkdown {
    front_matter: HashMap<String, String>,
    body: String,
}

fn parse_markdown(text: &str) -> ParsedMarkdown {
    let content = text;
    if !(content.starts_with("---\n") || content.starts_with("---\r\n")) {
        return ParsedMarkdown {
            front_matter: HashMap::new(),
            body: content.to_string(),
        };
    }
    let first_line_end = content
        .find('\n')
        .map(|index| index + 1)
        .unwrap_or_default();
    let Some(front_matter_end) = find_front_matter_end(content, first_line_end) else {
        return ParsedMarkdown {
            front_matter: HashMap::new(),
            body: content.to_string(),
        };
    };
    let front_matter = parse_front_matter(&content[first_line_end..front_matter_end]);
    let mut body_start = front_matter_end + 3;
    while body_start < content.len() && matches!(content.as_bytes()[body_start], b'\r' | b'\n') {
        body_start += 1;
    }
    ParsedMarkdown {
        front_matter,
        body: content[body_start..].to_string(),
    }
}

fn find_front_matter_end(content: &str, mut index: usize) -> Option<usize> {
    while index < content.len() {
        let line_end = content[index..]
            .find('\n')
            .map(|offset| index + offset)
            .unwrap_or(content.len());
        if content[index..line_end].trim() == "---" {
            return Some(index);
        }
        if line_end == content.len() {
            return None;
        }
        index = line_end + 1;
    }
    None
}

fn parse_front_matter(text: &str) -> HashMap<String, String> {
    let lines = text.lines().collect::<Vec<_>>();
    let mut values = HashMap::new();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let Some(colon) = line.find(':') else {
            index += 1;
            continue;
        };
        let key = line[..colon].trim().to_lowercase();
        let value = line[colon + 1..].trim();
        if value == ">" || value == "|" {
            let folded = value == ">";
            let mut block = String::new();
            index += 1;
            while index < lines.len() {
                let next = lines[index];
                if !next.is_empty() && !next.starts_with(char::is_whitespace) {
                    index = index.saturating_sub(1);
                    break;
                }
                let trimmed = next.trim();
                if !trimmed.is_empty() {
                    if !block.is_empty() {
                        block.push(if folded { ' ' } else { '\n' });
                    }
                    block.push_str(trimmed);
                }
                index += 1;
            }
            values.insert(key, block);
        } else {
            values.insert(key, unquote_front_matter(value));
        }
        index += 1;
    }
    values
}

fn parse_enabled(value: Option<&str>) -> Result<bool> {
    match value.unwrap_or("false").trim().to_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => bail!("Invalid AI skill enabled value: {other}"),
    }
}

fn parse_target(value: Option<&str>) -> Result<AiSkillTarget> {
    match value.unwrap_or("BOTH").trim().to_uppercase().as_str() {
        "CHAT" => Ok(AiSkillTarget::Chat),
        "AGENT" => Ok(AiSkillTarget::Agent),
        "BOTH" => Ok(AiSkillTarget::Both),
        "CONNECTION" => Ok(AiSkillTarget::Connection),
        other => bail!("Invalid AI skill target: {other}"),
    }
}

fn parse_tags(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    let value = raw
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(raw);
    value
        .split(',')
        .map(|tag| unquote_front_matter(tag.trim()))
        .filter(|tag| !tag.trim().is_empty())
        .collect()
}

fn append_optional_front_matter(markdown: &mut String, key: &str, value: &str) {
    let normalized = value.trim();
    if normalized.contains('\n') || normalized.len() > 100 {
        markdown.push_str(&format!("{key}: >\n"));
        for line in normalized.lines() {
            markdown.push_str("  ");
            markdown.push_str(line.trim());
            markdown.push('\n');
        }
    } else {
        markdown.push_str(&format!("{key}: {}\n", quote_front_matter(normalized)));
    }
}

fn quote_front_matter(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn unquote_front_matter(value: &str) -> String {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        value.to_string()
    }
}

fn strip_markdown_extension(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.ends_with(".markdown") {
        name[..name.len() - ".markdown".len()].to_string()
    } else if lower.ends_with(".md") {
        name[..name.len() - ".md".len()].to_string()
    } else {
        name.to_string()
    }
}

fn non_blank<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_round_trip_preserves_kortty_fields() {
        let skill = AiSkill {
            id: "skill-1".into(),
            name: "Logs".into(),
            description: Some("Use journalctl".into()),
            tags: vec!["linux".into(), "logs".into()],
            enabled: true,
            target: AiSkillTarget::Agent,
            content: "Prefer `journalctl -xe`.".into(),
        };

        let markdown = export_skill_to_markdown(&skill);
        let imported = import_skill_from_markdown(None, &markdown).unwrap();

        assert_eq!(imported.name, "Logs");
        assert!(imported.enabled);
        assert_eq!(imported.target, AiSkillTarget::Agent);
        assert_eq!(imported.tags, vec!["linux", "logs"]);
        assert_eq!(imported.content, "Prefer `journalctl -xe`.");
    }

    #[test]
    fn plain_markdown_imports_disabled() {
        let imported = import_skill_from_markdown(None, "# Foreign skill\nBody").unwrap();

        assert!(!imported.enabled);
        assert_eq!(imported.target, AiSkillTarget::Both);
        assert_eq!(imported.content, "# Foreign skill\nBody");
    }

    fn skill(id: &str, name: &str, target: AiSkillTarget) -> AiSkill {
        AiSkill {
            id: id.into(),
            name: name.into(),
            description: None,
            tags: vec![],
            enabled: true,
            target,
            content: format!("Content of {name}."),
        }
    }

    #[test]
    fn pinned_skill_survives_relevance_auto_detection() {
        let skills = vec![
            skill("relevant", "journalctl", AiSkillTarget::Chat),
            skill("pinned", "backup ritual", AiSkillTarget::Both),
            skill("irrelevant", "kubernetes", AiSkillTarget::Chat),
        ];

        let selected = select_relevant_skills(
            "how do I read journalctl output?",
            &skills,
            AiSkillTarget::Chat,
            &["pinned".to_string()],
        );

        let ids: Vec<&str> = selected.iter().map(|skill| skill.id.as_str()).collect();
        assert!(
            ids.contains(&"relevant"),
            "relevance match must stay selected"
        );
        assert!(
            ids.contains(&"pinned"),
            "pinned skill must bypass relevance"
        );
        assert!(!ids.contains(&"irrelevant"));
    }

    #[test]
    fn connection_skills_require_pinning_and_never_auto_detect() {
        let skills = vec![skill("conn-skill", "journalctl", AiSkillTarget::Connection)];

        // Even a perfect keyword match must not select an unpinned connection skill.
        let unpinned = select_relevant_skills(
            "how do I read journalctl output?",
            &skills,
            AiSkillTarget::Chat,
            &[],
        );
        assert!(unpinned.is_empty());

        // Pinned connection skills are included for chat and agent targets.
        for target in [AiSkillTarget::Chat, AiSkillTarget::Agent] {
            let pinned = select_relevant_skills(
                "unrelated prompt",
                &skills,
                target,
                &["conn-skill".to_string()],
            );
            assert_eq!(pinned.len(), 1);
            assert_eq!(pinned[0].id, "conn-skill");
        }
    }

    #[test]
    fn disabled_pinned_skills_stay_excluded() {
        let mut disabled = skill("conn-skill", "journalctl", AiSkillTarget::Connection);
        disabled.enabled = false;
        let skills = vec![disabled];

        let selected = select_relevant_skills(
            "journalctl",
            &skills,
            AiSkillTarget::Chat,
            &["conn-skill".to_string()],
        );
        assert!(selected.is_empty());
    }

    #[test]
    fn pinned_skills_are_not_duplicated_when_already_relevant() {
        let skills = vec![skill("pinned", "journalctl", AiSkillTarget::Both)];

        let selected = select_relevant_skills(
            "how do I read journalctl output?",
            &skills,
            AiSkillTarget::Chat,
            &["pinned".to_string()],
        );
        assert_eq!(selected.len(), 1);
    }

    #[test]
    fn append_skills_to_prompt_includes_pinned_connection_skills() {
        let skills = vec![skill("conn-skill", "Backup", AiSkillTarget::Connection)];

        let (prompt, used) = append_skills_to_prompt(
            "Base prompt.",
            "unrelated question",
            &skills,
            AiSkillTarget::Chat,
            &["conn-skill".to_string()],
        );

        assert!(prompt.contains("Content of Backup."));
        assert_eq!(used, vec!["Backup".to_string()]);

        let (prompt_without_pin, used_without_pin) = append_skills_to_prompt(
            "Base prompt.",
            "unrelated question",
            &skills,
            AiSkillTarget::Chat,
            &[],
        );
        assert_eq!(prompt_without_pin, "Base prompt.");
        assert!(used_without_pin.is_empty());
    }
}
