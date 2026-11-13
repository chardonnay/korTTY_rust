use std::env;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use kortty_control::{
    AGENT_START_WAIT_DEFAULT_MS, AGENT_WAIT_DEFAULT_MS, Client, ClientError, Method,
    PROTOCOL_VERSION, TAB_CREATE_ISOLATIONS, profile_root,
};
use serde_json::{Map, Value, json};

#[path = "kortty-cli/policy_value.rs"]
mod policy_value_command;

const MAX_STDIN: u64 = 1_048_576;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, error, quiet)) => {
            if !quiet {
                eprintln!("kortty-cli: {error}");
            }
            ExitCode::from(code)
        }
    }
}

type Failure = (u8, String, bool);

fn run() -> Result<(), Failure> {
    let mut args = env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|_| (2, "Arguments must be UTF-8".into(), false))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args
        .first()
        .is_some_and(|command| policy_value_command::matches(command))
    {
        return policy_value_command::run(args);
    }
    let options = Options::take(&mut args)?;
    let syntax = |error: String| (2, error, options.quiet);
    if options.version {
        if !options.quiet {
            println!(
                "kortty-cli {} (protocol {PROTOCOL_VERSION})",
                env!("CARGO_PKG_VERSION")
            );
        }
        return Ok(());
    }
    if options.help || args.is_empty() {
        if !options.quiet {
            help(&args);
        }
        return Ok(());
    }
    if args.first().is_some_and(|command| command == "mcp") {
        if args.len() != 1 || options.raw || options.pretty {
            return Err(syntax(
                "Usage: kortty-cli mcp [--config-dir <path>] [--timeout <ms>]".into(),
            ));
        }
        let root = options
            .root
            .map_or_else(profile_root, Ok)
            .map_err(|error| (3, error.to_string(), options.quiet))?;
        return kortty_control::mcp::serve_stdio(&root, options.timeout)
            .map_err(|error| (1, error.to_string(), options.quiet));
    }
    let (name, mut params, method, current) = invocation(&args).map_err(syntax)?;
    if options.raw && method != Some(Method::Read) {
        return Err(syntax("--raw requires pane read".into()));
    }
    let timeout = options.timeout.unwrap_or_else(|| {
        if matches!(
            method,
            Some(Method::Wait | Method::AgentWait | Method::AgentPrompt | Method::AgentStart)
        ) {
            params
                .get("timeout_ms")
                .and_then(Value::as_u64)
                .unwrap_or(match method {
                    Some(Method::AgentStart) => AGENT_START_WAIT_DEFAULT_MS,
                    Some(Method::AgentWait | Method::AgentPrompt) => AGENT_WAIT_DEFAULT_MS,
                    _ => 30_000,
                })
                .min(600_000)
                + if method == Some(Method::AgentStart) {
                    params
                        .get("ready_timeout_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(AGENT_WAIT_DEFAULT_MS)
                        .min(600_000)
                        * 2
                        + 15_000
                } else {
                    5_000
                }
        } else if method == Some(Method::EventsSubscribe) {
            3_600_000
        } else {
            5_000
        }
    });
    let root = match options.root {
        Some(root) => root,
        None => profile_root().map_err(|error| (3, error.to_string(), options.quiet))?,
    };
    let mut client = Client::connect(&root, Duration::from_millis(timeout))
        .map_err(|error| (error.exit_code(), error.to_string(), options.quiet))?;
    if current {
        let pane = client
            .resolve_current_pane()
            .map_err(|error| (error.exit_code(), error.to_string(), options.quiet))?
            .ok_or_else(|| {
                (
                    1,
                    "--current needs a local kortty shell. Provide --pane or --focused explicitly."
                        .into(),
                    options.quiet,
                )
            })?;
        params.insert("pane".into(), json!(pane));
    }
    if method.is_some_and(Method::mutates) && !params.contains_key("instance") {
        params.insert("instance".into(), json!(client.instance()));
    }
    let result = match client.call(&name, &Value::Object(params)) {
        Ok(result) => result,
        Err(error) => {
            let exit = error.exit_code();
            if method == Some(Method::CreateTab)
                && options.pretty
                && !options.quiet
                && let ClientError::Server { rpc, .. } = &error
            {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(rpc).unwrap_or_else(|_| rpc.to_string())
                );
                return Err((exit, String::new(), true));
            }
            return Err((exit, error.to_string(), options.quiet));
        }
    };
    if method == Some(Method::EventsSubscribe) {
        if !options.quiet {
            writeln!(io::stdout().lock(), "{result}")
                .map_err(|error| (1, error.to_string(), false))?;
        }
        loop {
            let event = client
                .next_event()
                .map_err(|error| (error.exit_code(), error.to_string(), options.quiet))?;
            if !options.quiet {
                writeln!(io::stdout().lock(), "{event}")
                    .map_err(|error| (1, error.to_string(), false))?;
            }
        }
    }
    if options.quiet {
        return Ok(());
    }
    let output = if options.raw {
        result
            .get("lines")
            .and_then(Value::as_array)
            .ok_or_else(|| (1, "Expected terminal lines".into(), false))?
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    } else if options.pretty {
        serde_json::to_string_pretty(&result).map_err(|error| (1, error.to_string(), false))?
    } else {
        result.to_string()
    };
    writeln!(io::stdout().lock(), "{output}").map_err(|error| (1, error.to_string(), false))
}

#[derive(Default)]
struct Options {
    root: Option<PathBuf>,
    timeout: Option<u64>,
    quiet: bool,
    pretty: bool,
    raw: bool,
    version: bool,
    help: bool,
}

impl Options {
    fn take(args: &mut Vec<String>) -> Result<Self, Failure> {
        let mut options = Self::default();
        // A value following a command flag can itself look like a global flag.
        let mut index = 0;
        while index < args.len() {
            let mut consume = 1;
            match args[index].as_str() {
                "-h" | "--help" => options.help = true,
                "--version" => options.version = true,
                "-q" | "--quiet" => options.quiet = true,
                "--pretty" => options.pretty = true,
                "--raw" => options.raw = true,
                "--config-dir" | "--timeout" => {
                    let flag = &args[index];
                    let next = args
                        .get(index + 1)
                        .ok_or_else(|| (2, format!("Missing value for {flag}"), options.quiet))?;
                    if flag == "--config-dir" {
                        options.root = Some(PathBuf::from(next));
                    } else {
                        options.timeout = Some(
                            next.parse::<u64>()
                                .ok()
                                .filter(|ms| (1..=3_600_000).contains(ms))
                                .ok_or_else(|| (2, "Invalid --timeout".into(), options.quiet))?,
                        );
                    }
                    consume = 2;
                }
                flag if value_flag(flag) => {
                    index += 2;
                    continue;
                }
                _ => {
                    index += 1;
                    continue;
                }
            }
            args.drain(index..index + consume);
        }
        Ok(options)
    }
}

fn value_flag(flag: &str) -> bool {
    matches!(
        flag,
        "--window"
            | "--connection"
            | "--isolation"
            | "--tab"
            | "--pane"
            | "--instance"
            | "--text"
            | "--command"
            | "--bracketed"
            | "--mode"
            | "--lines"
            | "--timeout-ms"
            | "--poll-ms"
            | "--contains"
            | "--regex"
            | "--method"
            | "--state"
            | "--kind"
            | "--alias"
            | "--wait-until"
            | "--until"
            | "--prompt"
            | "--split-from"
            | "--ready-timeout-ms"
            | "--title"
            | "--body"
            | "--kinds"
            | "--panes"
            | "--subscription-id"
    )
}

type Invocation = (String, Map<String, Value>, Option<Method>, bool);

fn invocation(args: &[String]) -> Result<Invocation, String> {
    if args[0] == "raw" {
        let [_, name, rest @ ..] = args else {
            return Err("Use raw <method> [JSON object]".into());
        };
        let params = match rest {
            [] => Map::new(),
            [params] => serde_json::from_str::<Map<String, Value>>(params)
                .map_err(|_| "Expected a JSON object".to_owned())?,
            _ => return Err("Use raw <method> [JSON object]".into()),
        };
        return Ok((name.clone(), params, Method::from_name(name), false));
    }
    let first = &args[0];
    let second = args.get(1).map(String::as_str).unwrap_or_default();
    let command = if matches!(first.as_str(), "window" | "tab" | "pane" | "agent")
        || first == "events" && second == "unsubscribe"
    {
        format!("{first} {second}")
    } else {
        first.clone()
    };
    let method = Method::ALL
        .iter()
        .copied()
        .find(|method| method.cli() == command)
        .ok_or_else(|| "Unknown or reserved command. Run kortty-cli --help".to_owned())?;
    let offset = if command.contains(' ') { 2 } else { 1 };
    let mut args = args[offset..].iter();
    let mut params = Map::new();
    let mut keys = Vec::new();
    let mut current = false;
    while let Some(arg) = args.next() {
        let (field, value) = match arg.as_str() {
            "--current" => {
                if !method.params().contains(&"pane") {
                    return Err("--current is unavailable for this command".into());
                }
                if current || params.contains_key("pane") {
                    return Err("Provide exactly one pane selector".into());
                }
                current = true;
                continue;
            }
            "--focused" => (
                if method == Method::FocusTab {
                    "tab"
                } else {
                    "pane"
                },
                json!("@focused"),
            ),
            "--visible" => ("mode", json!("visible")),
            "--recent" => ("mode", json!("recent")),
            "--detection" => ("mode", json!("detection")),
            "--horizontal" => ("orientation", json!("horizontal")),
            "--vertical" => ("orientation", json!("vertical")),
            "--no-focus" => ("focus", json!(false)),
            "--no-raise" => ("raise", json!(false)),
            "--local-shell-only" => ("local_shell_only", json!(true)),
            "--incognito" => ("incognito", json!(true)),
            "--submit" => ("submit", json!(true)),
            "--wait" => ("wait", json!(true)),
            "--include-evidence" => ("include_evidence", json!(true)),
            "--allow-shortcut-conflict" => ("allow_shortcut_conflict", json!(true)),
            "--stdin" if matches!(method, Method::SendText | Method::AgentPrompt) => {
                ("text", json!(read_stdin()?))
            }
            flag if value_flag(flag) => {
                let field = match flag {
                    "--timeout-ms" => "timeout_ms",
                    "--poll-ms" => "poll_ms",
                    "--wait-until" => "wait_until",
                    "--split-from" => "split_from",
                    "--ready-timeout-ms" => "ready_timeout_ms",
                    "--subscription-id" => "subscription_id",
                    _ => &flag[2..],
                };
                let next = args
                    .next()
                    .ok_or_else(|| format!("Missing value for {flag}"))?;
                let value = if matches!(
                    field,
                    "lines" | "timeout_ms" | "poll_ms" | "ready_timeout_ms"
                ) {
                    json!(
                        next.parse::<u64>()
                            .map_err(|_| format!("Invalid number for {flag}"))?
                    )
                } else if matches!(field, "kinds" | "panes") {
                    json!(
                        next.split(',')
                            .filter(|part| !part.is_empty())
                            .collect::<Vec<_>>()
                    )
                } else if method == Method::AgentStart && field == "command" {
                    let parts: Vec<String> = serde_json::from_str(next)
                        .map_err(|_| "Agent --command expects a JSON string array".to_owned())?;
                    json!(parts)
                } else if method == Method::CreateTab && field == "isolation" {
                    let isolation = next.trim().to_ascii_lowercase();
                    if !TAB_CREATE_ISOLATIONS.contains(&isolation.as_str()) {
                        return Err("--isolation must be one of none, process, sandbox".into());
                    }
                    json!(isolation)
                } else {
                    json!(next)
                };
                (field, value)
            }
            arg if arg.starts_with('-') => return Err("Unknown command option".into()),
            arg if matches!(method, Method::SendKeys | Method::AgentKeys) => {
                keys.push(arg.to_owned());
                continue;
            }
            arg if method.params().contains(&"pane") && !params.contains_key("pane") => {
                ("pane", json!(arg))
            }
            _ => return Err("Unexpected positional argument".into()),
        };
        if !method.params().contains(&field) {
            return Err(format!("Option {arg} is unavailable for this command"));
        }
        if (field == "pane" && current) || params.insert(field.to_owned(), value).is_some() {
            return Err(format!("Repeated option for {field}"));
        }
    }
    if matches!(method, Method::SendKeys | Method::AgentKeys) {
        if keys.is_empty() {
            return Err("Provide at least one key name".into());
        }
        params.insert("keys".into(), json!(keys));
    }
    for required in [
        "pane",
        "tab",
        "text",
        "command",
        "connection",
        "title",
        "body",
        "kind",
        "until",
    ] {
        if method.params().contains(&required)
            && (required != "pane" || method != Method::AgentStart)
            && (required != "command" || method == Method::Run)
            && (required != "kind" || method == Method::AgentStart)
            && (required != "until" || method == Method::AgentWait)
            && (required != "tab" || method == Method::FocusTab)
            && (required != "pane" || !current)
            && !params.contains_key(required)
        {
            return Err(format!("Provide --{required}"));
        }
    }
    if method == Method::Wait && params.contains_key("regex") == params.contains_key("contains") {
        return Err("Provide one of --regex or --contains".into());
    }
    if method == Method::AgentStart
        && (params.contains_key("pane") || current) == params.contains_key("split_from")
    {
        return Err("Provide exactly one of --pane/--current/--focused or --split-from".into());
    }
    Ok((method.name().to_owned(), params, Some(method), current))
}

fn read_stdin() -> Result<String, String> {
    let mut text = String::new();
    io::stdin()
        .take(MAX_STDIN + 1)
        .read_to_string(&mut text)
        .map_err(|_| "Could not read UTF-8 from stdin")?;
    if text.len() as u64 > MAX_STDIN {
        return Err("Input exceeds 1 MiB".into());
    }
    Ok(text)
}

fn help(args: &[String]) {
    println!("kortty-cli <command> [--config-dir <path>] [--timeout <ms>] [--pretty] [--quiet]");
    if args.is_empty() || args.first().is_some_and(|command| command == "mcp") {
        println!("  mcp                  Serve MCP tools over stdio");
    }
    if args.is_empty()
        || args
            .first()
            .is_some_and(|command| policy_value_command::matches(command))
    {
        println!(
            "  encrypt-policy-value [value]  Encrypt a policy value from stdin or an argument"
        );
    }
    for method in Method::ALL.iter().filter(|method| !method.cli().is_empty()) {
        if args.is_empty()
            || args
                .first()
                .is_some_and(|group| method.cli().starts_with(group))
        {
            if *method == Method::CreateTab {
                println!(
                    "  tab create --connection <id|name> [--isolation none|process|sandbox] [--incognito] [--window <id>]"
                );
            } else {
                let parameters = method.params().join(", ");
                println!("  {:20} {}", method.cli(), parameters);
            }
        }
    }
    println!(
        "  raw <method> [JSON object]\nUse --pane <id>, --focused or --current to target a pane."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, String> {
        invocation(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn agent_commands_keep_explicit_targets_and_typed_arguments() {
        let (_, params, method, current) = parse(&[
            "agent",
            "prompt",
            "--pane",
            "p1",
            "--text",
            "inspect",
            "--wait-until",
            "done",
            "--timeout-ms",
            "2000",
        ])
        .unwrap();
        assert_eq!(method, Some(Method::AgentPrompt));
        assert_eq!(params["wait_until"], "done");
        assert_eq!(params["timeout_ms"], 2000);
        assert!(!current);
        let (_, params, _, _) =
            parse(&["agent", "send-keys", "--pane", "p1", "y", "enter"]).unwrap();
        assert_eq!(params["keys"], json!(["y", "enter"]));
        assert!(parse(&["agent", "start", "--kind", "codex"]).is_err());
        assert!(
            parse(&[
                "agent",
                "start",
                "--kind",
                "codex",
                "--pane",
                "p1",
                "--split-from",
                "p2"
            ])
            .is_err()
        );
        let (_, params, _, _) = parse(&[
            "agent",
            "start",
            "--kind",
            "codex",
            "--split-from",
            "p1",
            "--command",
            "[\"codex\",\"--version\"]",
            "--wait",
        ])
        .unwrap();
        assert_eq!(params["command"], json!(["codex", "--version"]));
        assert_eq!(params["wait"], true);
    }

    #[test]
    fn events_and_notification_flags_do_not_consume_global_options() {
        let mut args = vec![
            "notify", "--title", "--help", "--body", "complete", "--pretty",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let options = Options::take(&mut args).unwrap();
        assert!(!options.help);
        assert!(options.pretty);
        let (_, params, method, _) = invocation(&args).unwrap();
        assert_eq!(method, Some(Method::Notify));
        assert_eq!(params["title"], "--help");
        let (_, params, method, _) = parse(&[
            "events",
            "--kinds",
            "agent.added,agent.removed",
            "--panes",
            "p1,p2",
            "--include-evidence",
        ])
        .unwrap();
        assert_eq!(method, Some(Method::EventsSubscribe));
        assert_eq!(params["panes"], json!(["p1", "p2"]));
        assert_eq!(params["include_evidence"], true);
    }
}
