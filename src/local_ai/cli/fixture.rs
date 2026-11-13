//! Standalone native test helper compiled only inside a test's private scratch directory.

use std::io::{Read as _, Write as _};

fn main() {
    let directory = std::env::current_dir().unwrap();
    assert!(
        directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(".ai-cli-")
    );
    let parent = directory.parent().unwrap();
    let command = std::env::args().nth(1);
    match command.as_deref() {
        Some("fixture_ssh_password") => {
            let literal: Vec<_> = std::env::args().skip(2).collect();
            assert_eq!(literal, ["{model}", "with a space", "&&"]);
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert!(input.is_empty());
            std::fs::write(parent.join("fixture-password-started"), b"started").unwrap();
            eprintln!("private diagnostic, never shown");
            println!("fixture-password");
            std::io::stdout()
                .write_all(&vec![b'x'; 128 * 1024])
                .unwrap();
        }
        Some("ps" | "ls") => {
            assert_eq!(std::env::args().skip(2).collect::<Vec<_>>(), ["--json"]);
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            assert!(input.is_empty());
            std::fs::write(parent.join("fixture-discovery-started"), b"started").unwrap();
            if command.as_deref() == Some("ps") {
                println!(
                    "{}",
                    r#"[{"type":"llm","modelKey":"qwen/qwen3.6-35b-a3b","identifier":"fixture-loaded","status":"idle"}]"#,
                );
            } else {
                println!(
                    "{}",
                    r#"Waking up LM Studio service...
[{"type":"llm","modelKey":"qwen/qwen3.6-35b-a3b","displayName":"Qwen3.6"},
 {"type":"llm","modelKey":"openai/gpt-oss-20b","variants":["openai/gpt-oss-20b@mxfp4"]},
 {"type":"embedding","modelKey":"text-embedding-nomic-embed-text-v1.5"}]"#,
                );
            }
        }
        Some("load") => {
            let args = std::env::args().skip(2).collect::<Vec<_>>();
            assert_eq!(args.len(), 2);
            assert_eq!(args[1], "-y");
            let mut stdin = Vec::new();
            std::io::stdin().read_to_end(&mut stdin).unwrap();
            assert!(stdin.is_empty());
            if args[0] != "openai/gpt-oss-20b" {
                eprintln!("fixture load refused");
                std::process::exit(7);
            }
            std::fs::write(parent.join("fixture-loaded-model"), &args[0]).unwrap();
        }
        Some("fixture_answer" | "chat") => {
            if command.as_deref() == Some("chat") {
                assert_eq!(
                    std::env::args().skip(2).collect::<Vec<_>>(),
                    [
                        "-p",
                        "Use the conversation provided on standard input as the complete prompt. Follow its instructions exactly and reply with only the requested answer.",
                    ]
                );
            }
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            std::fs::write(parent.join("fixture-stdin"), input).unwrap();
            #[cfg(target_os = "linux")]
            {
                use std::os::unix::ffi::OsStrExt as _;

                let argument = std::env::args_os().next().unwrap();
                std::fs::write(parent.join("fixture-argv0"), argument.as_bytes()).unwrap();
                let image = std::env::current_exe().unwrap();
                assert!(
                    std::fs::read_dir("/proc/self/fd")
                        .unwrap()
                        .filter_map(Result::ok)
                        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
                        .all(|path| path != image)
                );
            }
            assert!(std::env::var_os("KORTTY_AI_TEST_KEY").is_none());
            respond();
        }
        Some("fixture_files" | "fixture_files_waiting") => {
            let mut stdin = Vec::new();
            std::io::stdin().read_to_end(&mut stdin).unwrap();
            assert!(stdin.is_empty());
            let working = directory.canonicalize().unwrap();
            for path in std::env::args_os().skip(2) {
                let path = std::path::PathBuf::from(path);
                assert!(path.is_absolute());
                assert_eq!(
                    path.canonicalize().unwrap().parent(),
                    Some(working.as_path())
                );
                let name = path.file_name().unwrap().to_str().unwrap();
                assert!(matches!(
                    name,
                    "prompt.txt" | "system-prompt.txt" | "user-prompt.txt"
                ));
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    assert_eq!(
                        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                        0o600
                    );
                }
                std::fs::write(
                    parent.join(format!("fixture-{name}")),
                    std::fs::read(path).unwrap(),
                )
                .unwrap();
            }
            if std::env::args().nth(1).as_deref() == Some("fixture_files_waiting") {
                std::fs::write(parent.join("fixture-started"), b"started").unwrap();
                std::thread::sleep(std::time::Duration::from_secs(12));
            }
            respond();
        }
        Some("fixture_scheduled_proposal") => {
            let response = std::env::args().nth(2).unwrap();
            let path = std::env::args_os().nth(3);
            let mut input = Vec::new();
            std::io::stdin().read_to_end(&mut input).unwrap();
            if let Some(path) = path {
                assert!(input.is_empty());
                input = std::fs::read(path).unwrap();
            }
            std::fs::write(parent.join("fixture-scheduled-prompt"), input).unwrap();
            println!("{response}");
        }
        Some("fixture_modularization") => {
            let mut prompt = String::new();
            std::io::stdin().read_to_string(&mut prompt).unwrap();
            let path = std::env::args_os().nth(2);
            if let Some(path) = &path {
                assert!(prompt.is_empty());
                prompt = std::fs::read_to_string(path).unwrap();
            }
            let (stage, response) = match prompt.split_once("\nGenerate this planned file\n") {
                None => (
                    "plan",
                    r#"{"recommended":true,"rationale":"Extract the helper.","files":[{"path":"entry.py","purpose":"Executable entry point.","symbols":[],"executable":true,"entryPoint":true},{"path":"helper.py","purpose":"Reusable helper.","symbols":["VALUE"],"executable":false,"entryPoint":false}]}"#,
                ),
                Some((_, file)) if file.starts_with("{\n  \"path\": \"helper.py\"") => {
                    ("helper", r#"{"lines":["VALUE = 1"]}"#)
                }
                Some((_, file)) if file.starts_with("{\n  \"path\": \"entry.py\"") => (
                    "entry",
                    r#"{"lines":["from helper import VALUE","print(VALUE)"]}"#,
                ),
                Some(_) => panic!("unexpected modularization module"),
            };
            std::fs::write(
                parent.join(format!("fixture-modularization-{stage}")),
                prompt,
            )
            .unwrap();
            if let Some(path) = path {
                std::fs::write(
                    parent.join(format!("fixture-modularization-{stage}-path")),
                    path.to_str().unwrap(),
                )
                .unwrap();
            }
            println!("{response}");
        }
        Some("fixture_snippet_analysis") => {
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).unwrap();
            let path = std::env::args_os().nth(2);
            if let Some(path) = &path {
                assert!(input.is_empty());
                input = std::fs::read_to_string(path).unwrap();
            }
            let plan = input.contains("Propose whether splitting the supplied code into modules");
            let stage = if plan { "plan" } else { "report" };
            std::fs::write(parent.join(format!("fixture-snippet-{stage}")), input).unwrap();
            if let Some(path) = path {
                std::fs::write(
                    parent.join(format!("fixture-snippet-{stage}-path")),
                    path.to_str().unwrap(),
                )
                .unwrap();
            }
            let response = if plan {
                r#"{"recommended":true,"rationale":"Extract the helper.","files":[{"path":"entry.py","purpose":"Executable entry point.","symbols":[],"executable":true,"entryPoint":true},{"path":"helper.py","purpose":"Reusable helper.","symbols":["VALUE"],"executable":false,"entryPoint":false}]}"#
            } else {
                r#"{"summary":"Checked the selected source.","files":[{"path":"selected.py","purpose":"Main Python file."}],"dependencies":[],"improvements":[]}"#
            };
            println!("{response}");
        }
        Some("fixture_waiting") => {
            std::fs::write(parent.join("fixture-started"), b"started").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(12));
        }
        Some("fixture_journal_summary") => {
            let mut prompt = String::new();
            std::io::stdin().read_to_string(&mut prompt).unwrap();
            if prompt.contains("{\"ready\":true}") {
                std::fs::write(parent.join("fixture-journal-probe"), prompt).unwrap();
                println!("{{\"ready\":true}}");
            } else {
                let name = if prompt.contains("Write a closing session journal summary") {
                    "fixture-journal-close"
                } else {
                    "fixture-journal-window"
                };
                std::fs::write(parent.join(name), prompt).unwrap();
                println!(
                    "{{\"title\":\"Checked build\",\"summary\":\"The build exited 2.\",\"keywords\":[\"build.sh\"]}}"
                );
            }
        }
        Some("fixture_journal_question") => {
            let arguments: Vec<_> = std::env::args_os().skip(2).collect();
            let [system_path, user_path] = arguments.as_slice() else {
                panic!("expected separate system and user prompt files");
            };
            let mut stdin = Vec::new();
            std::io::stdin().read_to_end(&mut stdin).unwrap();
            assert!(stdin.is_empty());
            let system = std::fs::read_to_string(system_path).unwrap();
            let prompt = std::fs::read_to_string(user_path).unwrap();
            for (name, path, contents) in [
                ("system", system_path, system.as_str()),
                ("user", user_path, prompt.as_str()),
            ] {
                std::fs::write(
                    parent.join(format!("fixture-journal-question-{name}")),
                    contents,
                )
                .unwrap();
                std::fs::write(
                    parent.join(format!("fixture-journal-question-{name}-path")),
                    path.to_str().unwrap(),
                )
                .unwrap();
            }
            let findings = ["failure from part two", "Build failure on backup node"]
                .into_iter()
                .filter_map(|quote| {
                    let (before, _) = prompt.split_once(&format!("\"excerpt\":\"{quote}"))?;
                    let (_, citation) = before.rsplit_once("\"citation\":")?;
                    let (citation, _) = citation.split_once(',')?;
                    let citation = citation.parse::<usize>().unwrap();
                    let quote = if prompt.contains("invented citation") {
                        "a sentence outside the reviewed excerpts"
                    } else {
                        quote
                    };
                    Some(format!("{{\"citation\":{citation},\"quote\":\"{quote}\"}}"))
                })
                .collect::<Vec<_>>();
            assert!(!findings.is_empty());
            println!("{{\"findings\":[{}]}}", findings.join(","));
        }
        Some("fixture_too_large") => {
            std::io::copy(&mut std::io::stdin(), &mut std::io::sink()).unwrap();
            let _ = std::io::stdout().write_all(&[b'x'; 64 * 1024 + 1]);
        }
        _ => panic!("unknown fixture command"),
    }
}

fn respond() {
    std::io::stdout()
        .write_all(b"\x1b[31m<think>fixture reasoning</think>fixture final answer\x1b[0m\n")
        .unwrap();
}
