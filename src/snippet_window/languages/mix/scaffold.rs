//! Compares the exact portions of a pipeline outside its detected script steps.

use std::ops::Range;

use super::{
    HostFormat, JENKINS_STEP, github_shell, inspect, jenkins_steps, scan_yaml_steps, yaml_key,
};

pub(crate) fn targets(host: HostFormat) -> &'static [&'static str] {
    match host {
        HostFormat::AzurePipelines => &["bash", "powershell"],
        HostFormat::GitHubActions => &["bash", "powershell", "bat", "python"],
        HostFormat::JenkinsDeclarative | HostFormat::JenkinsScripted => {
            &["bash", "powershell", "bat"]
        }
        _ => &[],
    }
}

pub(crate) fn explicit_selectors_match(host: HostFormat, source: &str, target: &str) -> bool {
    if !matches!(host, HostFormat::AzurePipelines | HostFormat::GitHubActions) {
        return true;
    }
    let doc = Document::new(source);
    scan_yaml_steps(&doc.lines, host)
        .iter()
        .all(|(header, step)| {
            let Some(key) = yaml_key(doc.lines[*header]) else {
                return false;
            };
            if host == HostFormat::AzurePipelines {
                return key.name != "script" && step.language == target;
            }
            github_shell(&doc.lines, *header, &key)
                .and_then(|line| shell_token(doc.lines[line]))
                .is_some_and(|(_, language)| language == target)
        })
}

pub(crate) fn preserved(host: HostFormat, original: &str, proposed: &str, target: &str) -> bool {
    if !targets(host).contains(&target) || inspect("", proposed).host != Some(host) {
        return false;
    }
    let before = Document::new(original);
    let after = Document::new(proposed);
    let Some((original_cuts, proposed_cuts)) = (match host {
        HostFormat::AzurePipelines | HostFormat::GitHubActions => {
            yaml_cuts(host, &before, &after, target)
        }
        HostFormat::JenkinsDeclarative | HostFormat::JenkinsScripted => {
            jenkins_cuts(&before, &after, target)
        }
        _ => None,
    }) else {
        return false;
    };
    match (
        before.scaffold(original_cuts),
        after.scaffold(proposed_cuts),
    ) {
        (Some(before), Some(after)) => before == after,
        _ => false,
    }
}

struct Document<'a> {
    source: &'a str,
    lines: Vec<&'a str>,
    offsets: Vec<usize>,
}

struct Cut {
    range: Range<usize>,
    marker: bool,
}

impl<'a> Document<'a> {
    fn new(source: &'a str) -> Self {
        let mut offsets = vec![0];
        let lines = source
            .split_inclusive('\n')
            .map(|line| {
                offsets.push(offsets.last().unwrap() + line.len());
                line.strip_suffix('\n')
                    .map(|line| line.strip_suffix('\r').unwrap_or(line))
                    .unwrap_or(line)
            })
            .collect();
        Self {
            source,
            lines,
            offsets,
        }
    }

    fn cut_line(&self, line: usize, range: Range<usize>) -> Cut {
        let start = self.offsets[line];
        Cut {
            range: start + range.start..start + range.end,
            marker: true,
        }
    }

    fn scaffold(&self, mut cuts: Vec<Cut>) -> Option<String> {
        cuts.sort_by_key(|cut| cut.range.start);
        let mut scaffold = String::new();
        let mut offset = 0;
        for cut in cuts {
            scaffold.push_str(self.source.get(offset..cut.range.start)?);
            if cut.marker {
                scaffold.push('\0');
            }
            offset = cut.range.end;
        }
        scaffold.push_str(self.source.get(offset..)?);
        Some(scaffold)
    }
}

fn yaml_cuts(
    host: HostFormat,
    before: &Document<'_>,
    after: &Document<'_>,
    target: &str,
) -> Option<(Vec<Cut>, Vec<Cut>)> {
    let original = scan_yaml_steps(&before.lines, host);
    let proposed = scan_yaml_steps(&after.lines, host);
    if original.is_empty() || original.len() != proposed.len() {
        return None;
    }
    let mut before_cuts = Vec::new();
    let mut after_cuts = Vec::new();
    for ((before_header, old), (after_header, new)) in original.iter().zip(&proposed) {
        if new.language != target {
            return None;
        }
        if host == HostFormat::AzurePipelines
            && yaml_key(after.lines[*after_header])?.name == "script"
        {
            return None;
        }
        for (doc, &header, step, cuts) in [
            (before, before_header, old, &mut before_cuts),
            (after, after_header, new, &mut after_cuts),
        ] {
            let key = yaml_key(doc.lines[header])?;
            let body = if *step.lines.start() == header + 1 {
                doc.cut_line(header, yaml_value(doc.lines[header], key.column)?)
            } else {
                let start = *step.lines.start() - 1;
                let end = *step.lines.end() - 1;
                let indent = |line: &str| line.len() - line.trim_start().len();
                let mut lines = doc.lines[start..=end]
                    .iter()
                    .filter(|line| !line.trim().is_empty());
                let body_indent = indent(lines.next()?);
                if lines.any(|line| indent(line) < body_indent) {
                    return None;
                }
                // Retain the newline after the last body line, including its style.
                Cut {
                    range: doc.offsets[start]..doc.offsets[end] + doc.lines[end].len(),
                    marker: true,
                }
            };
            cuts.push(body);
            if host == HostFormat::AzurePipelines {
                cuts.push(doc.cut_line(header, key.column..key.column + key.name.len()));
            }
        }
        if host == HostFormat::GitHubActions {
            let old_key = yaml_key(before.lines[*before_header])?;
            let new_key = yaml_key(after.lines[*after_header])?;
            let old_shell = github_shell(&before.lines, *before_header, &old_key);
            let new_shell = github_shell(&after.lines, *after_header, &new_key);
            if let Some(shell) = new_shell {
                let (_, language) = shell_token(after.lines[shell])?;
                if language != target {
                    return None;
                }
            }
            match (old_shell, new_shell) {
                (Some(old), Some(new)) => {
                    before_cuts.push(before.cut_line(old, shell_token(before.lines[old])?.0));
                    after_cuts.push(after.cut_line(new, shell_token(after.lines[new])?.0));
                }
                (None, Some(shell_line)) => {
                    let shell = match target {
                        "bash" => "bash",
                        "powershell" => "pwsh",
                        "bat" => "cmd",
                        "python" => "python",
                        _ => return None,
                    };
                    if shell_line != *new.lines.end()
                        || after.lines[shell_line]
                            != format!("{}shell: {shell}", " ".repeat(new_key.column))
                    {
                        return None;
                    }
                    after_cuts.push(Cut {
                        range: after.offsets[shell_line]..after.offsets[shell_line + 1],
                        marker: false,
                    });
                }
                (_, None) => return None,
            }
        }
    }
    Some((before_cuts, after_cuts))
}

fn shell_token(line: &str) -> Option<(Range<usize>, &'static str)> {
    let key = yaml_key(line)?;
    let value = yaml_value(line, key.column)?;
    let text = line.get(value.clone())?;
    let quote = usize::from(text.starts_with(['\'', '"']));
    let name = text
        .get(quote..)?
        .split(|c: char| c.is_whitespace() || c == '\'' || c == '"')
        .next()?;
    let language = match name {
        "bash" | "sh" => "bash",
        "pwsh" | "powershell" => "powershell",
        "cmd" => "bat",
        "python" => "python",
        _ => return None,
    };
    let start = value.start + quote;
    Some((start..start + name.len(), language))
}

fn yaml_value(line: &str, column: usize) -> Option<Range<usize>> {
    let content = line.get(column..)?;
    let (key, value) = content.split_once(':')?;
    let text = value.trim_start();
    let start = column + key.len() + 1 + value.len() - text.len();
    if text.is_empty() || text.starts_with(['#', '{', '[', '&', '*', '!']) {
        return None;
    }
    let body_length = match text.as_bytes()[0] {
        b'\'' | b'"' => {
            let quote = text.as_bytes()[0];
            let mut bytes = text.bytes().enumerate().skip(1).peekable();
            loop {
                let (index, byte) = bytes.next()?;
                if quote == b'"' && byte == b'\\' {
                    bytes.next();
                } else if byte == quote {
                    if quote == b'\'' && bytes.peek().is_some_and(|(_, next)| *next == quote) {
                        bytes.next();
                    } else {
                        let tail = text[index + 1..].trim_start();
                        if !tail.is_empty() && !tail.starts_with('#') {
                            return None;
                        }
                        if text[1..index].trim().is_empty() {
                            return None;
                        }
                        break index + 1;
                    }
                }
            }
        }
        _ => text
            .char_indices()
            .find_map(|(index, character)| {
                (character == '#' && text[..index].ends_with(char::is_whitespace)).then_some(index)
            })
            .map_or(text, |end| &text[..end])
            .trim_end()
            .len(),
    };
    Some(start..start + body_length)
}

fn jenkins_cuts(
    before: &Document<'_>,
    after: &Document<'_>,
    target: &str,
) -> Option<(Vec<Cut>, Vec<Cut>)> {
    let original = jenkins_steps(&before.lines);
    let proposed = jenkins_steps(&after.lines);
    if original.is_empty()
        || original.len() != proposed.len()
        || proposed.iter().any(|step| step.language != target)
    {
        return None;
    }
    let cuts = |doc: &Document<'_>, steps: &[super::Embedded]| {
        let mut cuts = Vec::new();
        for step in steps {
            let first = *step.lines.start() - 1;
            let last = *step.lines.end() - 1;
            let captures = JENKINS_STEP.captures(doc.lines[first])?;
            let verb = captures.get(2)?;
            let quote = captures.get(3)?;
            let start = doc.offsets[first] + quote.end();
            let end = doc.offsets[last] + doc.lines[last].len();
            let script = doc.source.get(start..end)?;
            let close = script
                .match_indices(quote.as_str())
                .find_map(|(index, _)| {
                    let slashes = script.as_bytes()[..index]
                        .iter()
                        .rev()
                        .take_while(|byte| **byte == b'\\')
                        .count();
                    (slashes % 2 == 0).then_some(index)
                })?;
            if script[..close].trim().is_empty() {
                return None;
            }
            cuts.push(doc.cut_line(first, verb.range()));
            cuts.push(Cut {
                range: start..start + close,
                marker: true,
            });
        }
        Some(cuts)
    };
    Some((cuts(before, &original)?, cuts(after, &proposed)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn azure_preserves_pipeline_bytes_and_checks_the_explicit_interpreter() {
        let original = "# Top\r\nsteps:\r\n  - bash: |-\r\n      echo one\r\n    displayName: Français\r\n\r\n  - pwsh: Write-Host two # Keep\r\n";
        let result = original
            .replace("- bash:", "- pwsh:")
            .replace("echo one", "Write-Host one");
        let host = HostFormat::AzurePipelines;
        assert!(preserved(host, original, &result, "powershell"));
        for changed in [
            result.replace("Français", "English"),
            result.replace("# Keep", "# Changed"),
            result.replace("\r\n\r\n", "\r\n"),
            result.replace("\r\n", "\n"),
            result.replace("  - pwsh: Write-Host two # Keep\r\n", ""),
        ] {
            assert!(!preserved(host, original, &changed, "powershell"));
        }
        assert!(!preserved(
            host,
            original,
            &result.replace("pwsh:", "script:"),
            "bash"
        ));
    }

    #[test]
    fn github_accepts_only_a_scoped_shell_selector_and_preserves_shell_options() {
        let original = "jobs:\n  verify:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo one\n      - run: Write-Host two\n        shell: pwsh\n";
        let result = original.replace("echo one", "Write-Host one\n        shell: pwsh");
        let host = HostFormat::GitHubActions;
        assert!(preserved(host, original, &result, "powershell"));
        let changed = result.replace("shell: pwsh\n", "shell: pwsh -NoProfile {0}\n");
        assert!(!preserved(host, original, &changed, "powershell"));
        assert!(!preserved(
            host,
            original,
            &result.replace("ubuntu-latest", "windows-latest"),
            "powershell"
        ));
    }

    #[test]
    fn jenkins_preserves_parameters_and_scaffold_after_a_quoted_inline_body() {
        let original = "pipeline {\n  sh(script: 'echo one', returnStatus: true)\n  bat '''echo two\n  echo three'''\n}\n";
        let result = original
            .replace("sh(script:", "pwsh(script:")
            .replace("bat '''", "pwsh '''")
            .replace("echo ", "Write-Host ");
        let host = HostFormat::JenkinsDeclarative;
        assert!(preserved(host, original, &result, "powershell"));
        assert!(!preserved(
            host,
            original,
            &result.replace("returnStatus: true", "returnStatus: false"),
            "powershell"
        ));
        assert!(!preserved(
            host,
            original,
            &result.replace("pwsh '''", "bat '''"),
            "powershell"
        ));
    }
}
