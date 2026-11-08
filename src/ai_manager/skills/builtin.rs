//! Built-in skill Markdown from the pinned Java reference.
use super::Skill;

pub(super) fn all() -> Vec<Skill> {
    [
        include_str!("builtin/builtin.devops.ansible.md"),
        include_str!("builtin/builtin.devops.azure-pipelines.md"),
        include_str!("builtin/builtin.devops.jenkins-declarative.md"),
        include_str!("builtin/builtin.devops.jenkins-scripted.md"),
        include_str!("builtin/builtin.devops.puppet.md"),
        include_str!("builtin/builtin.lang.assembler.md"),
        include_str!("builtin/builtin.lang.c.md"),
        include_str!("builtin/builtin.lang.cpp.md"),
        include_str!("builtin/builtin.lang.csharp.md"),
        include_str!("builtin/builtin.lang.dart.md"),
        include_str!("builtin/builtin.lang.go.md"),
        include_str!("builtin/builtin.lang.groovy.md"),
        include_str!("builtin/builtin.lang.java.md"),
        include_str!("builtin/builtin.lang.javascript.md"),
        include_str!("builtin/builtin.lang.kotlin.md"),
        include_str!("builtin/builtin.lang.lua.md"),
        include_str!("builtin/builtin.lang.macro-assembler.md"),
        include_str!("builtin/builtin.lang.perl.md"),
        include_str!("builtin/builtin.lang.php.md"),
        include_str!("builtin/builtin.lang.python.md"),
        include_str!("builtin/builtin.lang.r.md"),
        include_str!("builtin/builtin.lang.ruby.md"),
        include_str!("builtin/builtin.lang.rust.md"),
        include_str!("builtin/builtin.lang.sql.md"),
        include_str!("builtin/builtin.lang.swift.md"),
        include_str!("builtin/builtin.lang.typescript.md"),
        include_str!("builtin/builtin.lang.visual-basic.md"),
        include_str!("builtin/builtin.markup.html.md"),
        include_str!("builtin/builtin.markup.json.md"),
        include_str!("builtin/builtin.markup.xml.md"),
        include_str!("builtin/builtin.markup.yaml.md"),
        include_str!("builtin/builtin.observability.filebeat.md"),
        include_str!("builtin/builtin.observability.logstash.md"),
        include_str!("builtin/builtin.shell.bash.md"),
        include_str!("builtin/builtin.shell.csh.md"),
        include_str!("builtin/builtin.shell.ksh.md"),
        include_str!("builtin/builtin.shell.powershell.md"),
        include_str!("builtin/builtin.shell.sh.md"),
        include_str!("builtin/builtin.shell.zsh.md"),
    ]
    .into_iter()
    .map(|markdown| Skill::parse(markdown, "AI Skill", true))
    .collect::<Result<_, _>>()
    .expect("embedded Java skill catalog must parse")
}
