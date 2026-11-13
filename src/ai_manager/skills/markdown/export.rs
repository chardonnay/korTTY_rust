//! Publish the selected skills as distinct Markdown files in one reviewed folder.

use std::collections::HashSet;
use std::io::{self, Write as _};
use std::path::Path;

use crate::private_files::{Directory, batch::Stage};
use crate::sftp_window::validate_transfer_name;

use super::{Skill, encoded};
use crate::ai_manager::skills::MAX_SKILLS;

const MAX_STEM_BYTES: usize = 80;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_NAME_ATTEMPTS: usize = 1024;

pub(in crate::ai_manager::skills) fn write_batch(
    folder: &Path,
    skills: &[Skill],
) -> Result<String, String> {
    if !(2..=MAX_SKILLS).contains(&skills.len()) || !folder.is_absolute() {
        return Err("Select multiple skills and enter an existing absolute folder path.".into());
    }
    let parent = Directory::open(folder, false)
        .map_err(|error| format!("Could not open the Markdown export folder ({error})."))?;
    let mut names: HashSet<_> = parent
        .names(MAX_DIRECTORY_ENTRIES)
        .map_err(|error| format!("Could not list the Markdown export folder ({error})."))?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .map(|name| name.to_ascii_lowercase())
        .collect();
    let entries = skills
        .iter()
        .map(|skill| {
            let name = unique_name(&skill.name, &mut names, |name| parent.has_entry(name))?;
            Ok((name, encoded(skill)?))
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut stage = Stage::new(&parent, ".kortty-skill-export")
        .map_err(|error| format!("Could not stage the Markdown export ({error})."))?;
    let publication = entries
        .iter()
        .try_for_each(|(name, content)| {
            stage.write_file(name, |file| file.write_all(content.as_bytes()))
        })
        .and_then(|()| {
            stage.publish(|| {
                if !parent.same_as(&Directory::open(folder, false)?)? {
                    return Err(io::Error::other("The Markdown export folder changed."));
                }
                Ok(())
            })
        });
    let cleanup = stage.cleanup(publication.is_ok());
    if let Err(error) = publication {
        let message = format!("Could not export the Markdown files ({error}).");
        return match cleanup {
            Ok(()) => Err(message),
            Err(rollback) => Err(format!(
                "{message} Cleanup could not finish ({rollback}). Recovery folder: {}",
                stage.recovery_path(folder).display()
            )),
        };
    }
    let sync = parent.sync();
    Ok(match cleanup.and(sync) {
        Ok(()) => format!("{} skills exported.", entries.len()),
        Err(error) => format!(
            "{} skills exported. Final folder cleanup or synchronization failed ({error}).",
            entries.len()
        ),
    })
}

fn unique_name(
    name: &str,
    used: &mut HashSet<String>,
    exists: impl Fn(&str) -> io::Result<bool>,
) -> Result<String, String> {
    let mut stem = String::new();
    for character in name.trim().to_lowercase().chars() {
        if stem.len() == MAX_STEM_BYTES {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_') {
            stem.push(character);
        } else if !stem.ends_with('-') {
            stem.push('-');
        }
    }
    let stem = stem.trim_matches(['.', '-']);
    let mut stem = if stem.is_empty() {
        "ai-skill".into()
    } else {
        stem.to_owned()
    };
    if validate_transfer_name(format!("{stem}.md").as_bytes()).is_err() {
        stem.insert_str(0, "skill-");
    }
    for suffix in 1..=MAX_NAME_ATTEMPTS {
        let name = if suffix == 1 {
            format!("{stem}.md")
        } else {
            format!("{stem}-{suffix}.md")
        };
        if !used.contains(&name)
            && !exists(&name)
                .map_err(|error| format!("Could not check a Markdown export filename ({error})."))?
        {
            used.insert(name.clone());
            return Ok(name);
        }
    }
    Err("Could not choose a unique Markdown filename in the export folder.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_markdown_batch_export_uses_portable_bounded_names() {
        let mut used = HashSet::new();
        let mut name = |value: &str| unique_name(value, &mut used, |_| Ok(false)).unwrap();
        assert_eq!(name("CON.log"), "skill-con.log.md");
        assert_eq!(name("漢字"), "ai-skill.md");
        assert_eq!(name(".. / .."), "ai-skill-2.md");
        let long = name(&"A".repeat(256));
        assert_eq!(long, format!("{}.md", "a".repeat(MAX_STEM_BYTES)));
        let alias = unique_name(
            "Report",
            &mut used,
            |candidate| Ok(candidate == "report.md"),
        )
        .unwrap();
        assert_eq!(alias, "report-2.md");
    }

    #[test]
    fn skill_markdown_batch_export_validates_the_whole_selection_before_writing() {
        let profile = crate::snippets::tests::Profile::new();
        let folder = profile.0.canonicalize().unwrap();
        let valid = Skill {
            name: "Valid skill".into(),
            content: "Test instructions.".into(),
            ..Skill::default()
        };
        assert!(write_batch(&folder, &[valid, Skill::default()]).is_err());
        assert_eq!(std::fs::read_dir(folder).unwrap().count(), 0);
    }
}
