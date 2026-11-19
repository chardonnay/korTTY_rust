//! Identifies embedded programs and the host formats that own script steps.

use std::ops::RangeInclusive;
use std::sync::LazyLock;

use regex::Regex;

pub(crate) mod scaffold;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum HostFormat {
    AzurePipelines,
    GitHubActions,
    GitLabCi,
    JenkinsDeclarative,
    JenkinsScripted,
    Ansible,
    Puppet,
    Dockerfile,
}

impl HostFormat {
    pub(crate) const CONVERTIBLE: [Self; 7] = [
        Self::AzurePipelines,
        Self::GitHubActions,
        Self::GitLabCi,
        Self::JenkinsDeclarative,
        Self::JenkinsScripted,
        Self::Ansible,
        Self::Puppet,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::AzurePipelines => "Azure Pipelines",
            Self::GitHubActions => "GitHub Actions",
            Self::GitLabCi => "GitLab CI",
            Self::JenkinsDeclarative => "Jenkinsfile (declarative)",
            Self::JenkinsScripted => "Jenkinsfile (scripted)",
            Self::Ansible => "Ansible playbook",
            Self::Puppet => "Puppet manifest",
            Self::Dockerfile => "Dockerfile",
        }
    }

    pub(crate) fn document_type(self) -> (&'static str, &'static str) {
        match self {
            Self::AzurePipelines => ("yaml", "azure-pipelines.yml"),
            Self::GitHubActions => ("yaml", "workflow.yml"),
            Self::GitLabCi => ("yaml", ".gitlab-ci.yml"),
            Self::JenkinsDeclarative | Self::JenkinsScripted => ("groovy", "Jenkinsfile"),
            Self::Ansible => ("yaml", "playbook.yml"),
            Self::Puppet => ("puppet", "manifest.pp"),
            Self::Dockerfile => ("dockerfile", "Dockerfile"),
        }
    }
}

impl std::fmt::Display for HostFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Embedded {
    pub(crate) language: &'static str,
    pub(crate) lines: RangeInclusive<usize>,
}

pub(crate) struct Inspection {
    pub(crate) host: Option<HostFormat>,
    pub(crate) embedded: Vec<Embedded>,
}

impl Inspection {
    pub(crate) fn mixed(&self) -> bool {
        self.embedded.first().is_some_and(|first| {
            self.host.is_none()
                || self
                    .embedded
                    .iter()
                    .any(|block| block.language != first.language)
        })
    }
}

pub(crate) fn inspect(declared: &str, source: &str) -> Inspection {
    let language = super::normalize(declared).unwrap_or_else(|| "plain".into());
    let normalized = source.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<_> = normalized.lines().collect();
    let host = host_format(&language, &lines);
    let embedded = match host {
        Some(HostFormat::JenkinsDeclarative | HostFormat::JenkinsScripted) => jenkins_steps(&lines),
        Some(host @ (HostFormat::Puppet | HostFormat::Dockerfile)) => shell_steps(&lines, host),
        Some(host) => yaml_steps(&lines, host),
        None => {
            let language = if language == "plain" {
                shebang_language(&lines).unwrap_or(&language)
            } else {
                &language
            };
            plain_script(language, &lines)
        }
    };
    Inspection { host, embedded }
}

fn host_format(declared: &str, lines: &[&str]) -> Option<HostFormat> {
    use HostFormat::*;

    static DOCKER_FROM: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^FROM\s+\S").unwrap());
    static DOCKER_STEP: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^(RUN|CMD|ENTRYPOINT|COPY|ADD)\s").unwrap());
    static PIPELINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^pipeline\s*\{").unwrap());
    static NODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^node\s*[({]").unwrap());
    static STAGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(^|[^A-Za-z0-9_.])stage\s*\(").unwrap());
    static PUPPET_EXEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^exec\s*\{").unwrap());
    static PUPPET_DECLARATION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(class|define)\s+\w[\w:]*\s*[(\{]").unwrap());

    let matches = |pattern: &Regex| {
        lines.iter().any(|line| {
            let line = line.trim_start();
            !line.starts_with(['#', '/']) && pattern.is_match(line)
        })
    };
    if !lines.iter().any(|line| !line.trim().is_empty()) {
        return None;
    }
    if declared == "dockerfile" || (matches(&DOCKER_FROM) && matches(&DOCKER_STEP)) {
        return Some(Dockerfile);
    }
    if matches(&PIPELINE) {
        return Some(JenkinsDeclarative);
    }
    if matches(&NODE) && matches(&STAGE) {
        return Some(JenkinsScripted);
    }
    if matches(&PUPPET_EXEC)
        || (matches!(declared, "puppet" | "plain") && matches(&PUPPET_DECLARATION))
    {
        return Some(Puppet);
    }

    let has = |name| {
        lines
            .iter()
            .filter_map(|line| yaml_key(line))
            .any(|key| key.name == name)
    };
    let root = |name| {
        lines
            .iter()
            .filter_map(|line| yaml_key(line))
            .any(|key| key.name == name && key.column == 0)
    };
    if root("jobs") && has("runs-on") {
        return Some(GitHubActions);
    }
    if lines.iter().filter_map(|line| yaml_key(line)).any(|key| {
        key.list_item
            && matches!(
                key.name,
                "task" | "script" | "bash" | "pwsh" | "powershell" | "checkout"
            )
    }) || (["pool", "trigger", "stages"].into_iter().any(root) && has("steps"))
    {
        return Some(AzurePipelines);
    }
    if lines
        .iter()
        .filter_map(|line| yaml_key(line))
        .any(|key| key.name.starts_with("ansible.builtin."))
        || (has("hosts") && ["tasks", "pre_tasks", "post_tasks"].into_iter().any(has))
    {
        return Some(Ansible);
    }
    if root("stages")
        && ["before_script", "script", "after_script"]
            .into_iter()
            .any(has)
    {
        return Some(GitLabCi);
    }
    None
}

struct YamlKey<'a> {
    name: &'a str,
    value: &'a str,
    column: usize,
    list_item: bool,
}

fn yaml_key(line: &str) -> Option<YamlKey<'_>> {
    let text = line.trim_start();
    let item = text
        .strip_prefix('-')
        .filter(|rest| rest.starts_with([' ', '\t']));
    let key = item.map_or(text, str::trim_start);
    let (name, value) = key.split_once(':')?;
    let name = name.trim_end();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    {
        return None;
    }
    Some(YamlKey {
        name,
        value: value.trim(),
        column: line.len() - key.len(),
        list_item: item.is_some(),
    })
}

fn yaml_steps(lines: &[&str], host: HostFormat) -> Vec<Embedded> {
    scan_yaml_steps(lines, host)
        .into_iter()
        .map(|(_, step)| step)
        .collect()
}

fn scan_yaml_steps(lines: &[&str], host: HostFormat) -> Vec<(usize, Embedded)> {
    let mut found = Vec::new();
    let mut next = 0;
    while let Some(line) = lines.get(next) {
        let index = next;
        next += 1;
        let Some(key) = yaml_key(line) else { continue };
        let language = match host {
            HostFormat::AzurePipelines if key.list_item => match key.name {
                "pwsh" | "powershell" => "powershell",
                "bash" | "script" => "bash",
                _ => continue,
            },
            HostFormat::GitHubActions if key.name == "run" => {
                github_step_language(lines, index, &key)
            }
            HostFormat::GitLabCi
                if matches!(key.name, "before_script" | "script" | "after_script") =>
            {
                "bash"
            }
            HostFormat::Ansible
                if matches!(
                    key.name
                        .strip_prefix("ansible.builtin.")
                        .unwrap_or(key.name),
                    "shell" | "command" | "raw" | "script"
                ) =>
            {
                "bash"
            }
            _ => continue,
        };
        let header = key.value.split_ascii_whitespace().next().unwrap_or("");
        if header.is_empty() || matches!(header, "|" | "|-" | "|+" | ">" | ">-" | ">+") {
            let mut end = next;
            for (offset, body) in lines[next..].iter().enumerate() {
                let text = body.trim_start();
                if text.is_empty() {
                    continue;
                }
                if body.len() - text.len() <= key.column {
                    break;
                }
                end = next + offset + 1;
            }
            if end > next {
                found.push((
                    index,
                    Embedded {
                        language,
                        lines: next + 1..=end,
                    },
                ));
                next = end;
            }
        } else if !header.starts_with('#') {
            found.push((
                index,
                Embedded {
                    language,
                    lines: index + 1..=index + 1,
                },
            ));
        }
    }
    found
}

fn github_step_language(lines: &[&str], index: usize, run: &YamlKey<'_>) -> &'static str {
    github_shell(lines, index, run)
        .and_then(|index| yaml_key(lines[index]))
        .and_then(|key| key.value.split_ascii_whitespace().next())
        .map_or("bash", |shell| match shell.trim_matches(['\'', '"']) {
            "pwsh" | "powershell" => "powershell",
            "cmd" => "bat",
            "python" => "python",
            _ => "bash",
        })
}

fn github_shell(lines: &[&str], index: usize, run: &YamlKey<'_>) -> Option<usize> {
    let mut before = lines[..index].iter().enumerate().rev();
    let mut after = lines.iter().enumerate().skip(index + 1);
    for forward in [true, false] {
        if !forward && run.list_item {
            break;
        }
        loop {
            let line = if forward { after.next() } else { before.next() };
            let Some((index, line)) = line else { break };
            let text = line.trim_start();
            if text.is_empty() || text.starts_with('#') {
                continue;
            }
            let Some(key) = yaml_key(line) else {
                if line.len() - text.len() < run.column {
                    break;
                }
                continue;
            };
            if key.column < run.column || (forward && key.list_item && key.column == run.column) {
                break;
            }
            if key.column == run.column {
                if key.name == "shell" {
                    return Some(index);
                }
                if key.list_item || key.name == "run" {
                    break;
                }
            }
        }
    }
    None
}

fn shell_steps(lines: &[&str], host: HostFormat) -> Vec<Embedded> {
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(line) = lines.get(index) {
        let text = line.trim_start();
        let start = index;
        index += 1;
        let is_step = match host {
            HostFormat::Puppet => text
                .strip_prefix("command")
                .is_some_and(|body| body.trim_start().starts_with("=>")),
            HostFormat::Dockerfile => text
                .split_ascii_whitespace()
                .next()
                .is_some_and(|verb| verb.eq_ignore_ascii_case("RUN")),
            _ => false,
        };
        if is_step {
            while index < lines.len() && lines[index - 1].trim_end().ends_with('\\') {
                index += 1;
            }
            found.push(Embedded {
                language: "bash",
                lines: start + 1..=index,
            });
        }
    }
    found
}

static JENKINS_STEP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(^|[^A-Za-z0-9_.])(sh|bat|powershell|pwsh)\s*\(?\s*(?:script\s*:\s*)?('''|"""|'|")"#,
    )
    .unwrap()
});

fn jenkins_steps(lines: &[&str]) -> Vec<Embedded> {
    let mut found = Vec::new();
    let mut index = 0;
    while let Some(line) = lines.get(index) {
        let start = index;
        index += 1;
        if line.trim_start().starts_with(['/', '*']) {
            continue;
        }
        let Some(step) = JENKINS_STEP.captures(line) else {
            continue;
        };
        let language = interpreter(&step[2]).unwrap_or("bash");
        let Some(quote) = step.get(3) else { continue };
        if quote.len() == 3 && !line[quote.end()..].contains(quote.as_str()) {
            while let Some(body) = lines.get(index) {
                index += 1;
                if body.contains(quote.as_str()) {
                    break;
                }
            }
        }
        found.push(Embedded {
            language,
            lines: start + 1..=index,
        });
    }
    found
}

fn interpreter(token: &str) -> Option<&'static str> {
    match token {
        "bash" | "sh" | "zsh" => Some("bash"),
        "python" | "python3" => Some("python"),
        "node" | "nodejs" => Some("javascript"),
        "perl" => Some("perl"),
        "ruby" => Some("ruby"),
        "php" => Some("php"),
        "awk" | "gawk" => Some("awk"),
        "psql" | "mysql" | "sqlplus" => Some("sql"),
        "pwsh" | "powershell" => Some("powershell"),
        "cmd" | "bat" => Some("bat"),
        "osascript" => Some("applescript"),
        "groovy" => Some("groovy"),
        _ => None,
    }
}

fn shebang_language(lines: &[&str]) -> Option<&'static str> {
    let shebang = lines
        .first()?
        .trim_start_matches('\u{feff}')
        .strip_prefix("#!")?;
    let mut words = shebang.split_whitespace();
    let program = words.next()?.rsplit('/').next()?;
    if program == "env" {
        words.find_map(interpreter)
    } else {
        interpreter(program)
    }
}

pub(in crate::snippet_window) fn interpreter_from_source(source: &str) -> Option<&'static str> {
    shebang_language(&[source.split(['\r', '\n']).next()?])
}

fn plain_script(dominant: &str, lines: &[&str]) -> Vec<Embedded> {
    static HEREDOC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(^|[;&|]\s*|\s)(?:(perl|python3?|node|ruby|php|g?awk|psql|mysql|sqlplus)\b[^<]*?)?<<(-?)\s*(?:'([A-Za-z_][A-Za-z0-9_]*)'|"([A-Za-z_][A-Za-z0-9_]*)"|([A-Za-z_][A-Za-z0-9_]*))"#).unwrap()
    });
    static INLINE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(^|[^A-Za-z0-9_./-])((perl\s+-[A-Za-z]*e\b)|(python3?\s+-c\b)|(node\s+-e\b)|(ruby\s+-e\b)|(php\s+-r\b)|(g?awk\s+(?:-\S+\s+)*['"]))"#).unwrap()
    });
    static SHELL_API: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(^|[^A-Za-z0-9_.])(subprocess\.(run|call|check_call|check_output|Popen)\s*\([^)]*shell\s*=\s*True|os\.system\s*\(|system\s*\(|qx[({/\[]|%x[({\[])").unwrap()
    });
    static OPERATOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"&&|\|\||;\s*\S|\S\s*\|\s*\S").unwrap());

    let comment = match dominant {
        "javascript" | "typescript" | "groovy" | "java" => "//",
        "sql" | "applescript" => "--",
        "bat" => "::",
        _ => "#",
    };
    let mut index = 0;
    let mut embedded = Vec::new();
    while let Some(line) = lines.get(index) {
        let text = line.trim_start();
        let start = index;
        index += 1;
        if text.starts_with(comment)
            || (dominant == "bat"
                && text
                    .get(..4)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("rem ")))
        {
            continue;
        }
        if matches!(dominant, "bash" | "plain")
            && let Some(found) = HEREDOC.captures(text)
        {
            let delimiter = (4..=6).find_map(|group| found.get(group)).unwrap().as_str();
            let language = found.get(2).and_then(|name| interpreter(name.as_str()));
            let strip_tabs = found.get(3).is_some_and(|flag| !flag.is_empty());
            while let Some(body) = lines.get(index) {
                let body = if strip_tabs {
                    body.trim_start_matches('\t')
                } else {
                    body
                };
                if body == delimiter {
                    break;
                }
                index += 1;
            }
            if index > start + 1
                && let Some(language) = language.filter(|language| *language != dominant)
            {
                embedded.push(Embedded {
                    language,
                    lines: start + 2..=index,
                });
            }
            index = (index + 1).min(lines.len());
        } else {
            let inline_language = INLINE.captures_iter(text).find_map(|found| {
                let program = found[2].split_ascii_whitespace().next()?;
                interpreter(program).filter(|language| *language != dominant)
            });
            let language = inline_language.or_else(|| {
                (matches!(dominant, "python" | "perl" | "ruby")
                    && SHELL_API.is_match(text)
                    && OPERATOR.is_match(text))
                .then_some("bash")
            });
            if let Some(language) = language {
                embedded.push(Embedded {
                    language,
                    lines: start + 1..=index,
                });
            }
        }
    }
    embedded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_heredoc_bodies_and_inline_programs_in_source_order() {
        let mix = inspect(
            "text",
            "#!/usr/bin/env bash\r\nperl <<-'PERL'\r\n\tprint 1;\r\n\tPERL\r\nawk '{ print $1 }' file\r\npython3 -c 'print(1)'\r\n",
        );
        assert_eq!(mix.host, None);
        assert!(mix.mixed());
        assert_eq!(
            mix.embedded,
            [
                Embedded {
                    language: "perl",
                    lines: 3..=3
                },
                Embedded {
                    language: "awk",
                    lines: 5..=5
                },
                Embedded {
                    language: "python",
                    lines: 6..=6
                },
            ],
        );
    }

    #[test]
    fn ignores_comments_sed_and_literal_heredoc_content() {
        let shell = inspect(
            "bash",
            "# perl -e 'print 1'\nsed -e 's/a/b/' file\ncat <<'DATA'\npython3 -c 'literal example'\nDATA\npython3 script.py\n",
        );
        assert!(shell.embedded.is_empty());
        let python = inspect(
            "python",
            "subprocess.run('ls /tmp', shell=True)\nsubprocess.run('cd /tmp && ls', shell=True)\n",
        );
        assert_eq!(
            python.embedded,
            [Embedded {
                language: "bash",
                lines: 2..=2
            }]
        );
    }

    #[test]
    fn scopes_azure_block_bodies_below_the_step_key() {
        let mix = inspect(
            "yaml",
            "trigger:\n  - main\nsteps:\n  - bash: | # run locally\n      make all\n      make test\n    displayName: Build\n  - pwsh: Write-Host test\n",
        );
        assert_eq!(mix.host, Some(HostFormat::AzurePipelines));
        assert!(mix.mixed());
        assert_eq!(
            mix.embedded,
            [
                Embedded {
                    language: "bash",
                    lines: 5..=6
                },
                Embedded {
                    language: "powershell",
                    lines: 8..=8
                },
            ],
        );
    }

    #[test]
    fn github_shell_is_limited_to_its_own_step() {
        let mix = inspect(
            "yaml",
            "jobs:\n  verify:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo first\n      - shell: pwsh\n        run: Write-Host second\n      - run: |\n          echo third\n        shell: bash\n",
        );
        assert_eq!(mix.host, Some(HostFormat::GitHubActions));
        assert!(mix.mixed());
        assert_eq!(
            mix.embedded
                .iter()
                .map(|step| step.language)
                .collect::<Vec<_>>(),
            ["bash", "powershell", "bash"],
        );
    }

    #[test]
    fn detects_both_jenkins_formats_and_skips_nested_text_in_a_step() {
        let body = "  stage('Build') {\n    sh '''\n      echo first\n      echo \"pwsh 'example'\"\n    '''\n    bat 'echo second'\n  }\n}";
        for (header, kind) in [
            ("pipeline {", HostFormat::JenkinsDeclarative),
            ("node {", HostFormat::JenkinsScripted),
        ] {
            let mix = inspect("groovy", &format!("{header}\n{body}"));
            assert_eq!(mix.host, Some(kind));
            assert_eq!(mix.embedded[0].lines, 3..=6);
            assert_eq!(mix.embedded[1].language, "bat");
            assert!(mix.mixed());
        }
    }

    #[test]
    fn uniform_host_steps_are_not_mixed_scripts() {
        for (language, source, host) in [
            (
                "yaml",
                "steps:\n  - bash: echo a\n  - bash: echo b",
                HostFormat::AzurePipelines,
            ),
            (
                "yaml",
                "stages:\n  - build\nbuild:\n  script:\n    - echo a",
                HostFormat::GitLabCi,
            ),
            (
                "yaml",
                "- hosts: all\n  tasks:\n    - ansible.builtin.shell: echo a",
                HostFormat::Ansible,
            ),
            (
                "puppet",
                "exec { 'demo':\n  command => 'echo a',\n}",
                HostFormat::Puppet,
            ),
            (
                "plain",
                "FROM alpine\nRUN python3 -c 'print(1)'",
                HostFormat::Dockerfile,
            ),
        ] {
            let mix = inspect(language, source);
            assert_eq!(mix.host, Some(host));
            assert!(!mix.embedded.is_empty());
            assert!(!mix.mixed());
        }
        assert!(inspect("", " \n").host.is_none());
    }
}
