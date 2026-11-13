//! Current screen and process evidence for guarding code typed into a terminal.

use kortty_terminal::ScreenSnapshot;

mod input;
mod monitor;
mod overrides;
mod rules;
pub(crate) use input::{CodingAgentInput, CodingAgentKey};
pub(super) use input::{InputRequest, authorize_agent_input};
pub(super) use monitor::Monitor;
pub(crate) use monitor::{CodingAgentDetails, CodingAgentStatus};
pub(crate) use overrides::reload_overrides;
pub(crate) use rules::State as CodingAgentState;
use rules::{Kind, State};

const MAX_LINES: usize = 20;
const MAX_CELLS_PER_LINE: usize = 256;

struct Adapter {
    kind: Kind,
    id: &'static str,
    executables: &'static [&'static str],
    titles: &'static [&'static str],
}

// The ids and executable aliases are pinned to Java CodingAgentKind.
const ADAPTERS: &[Adapter] = &[
    adapter(Kind::Aider, "aider", &["aider"], &["Aider"]),
    adapter(Kind::Amp, "amp", &["amp"], &["Amp"]),
    adapter(
        Kind::Antigravity,
        "antigravity",
        &["agy", "antigravity"],
        &["Antigravity CLI"],
    ),
    adapter(
        Kind::ClaudeCode,
        "claude-code",
        &["claude", "claude-code"],
        &["Claude Code", "claude-code"],
    ),
    adapter(Kind::Cline, "cline", &["cline"], &["Cline"]),
    adapter(Kind::Codex, "codex", &["codex"], &["Codex", "OpenAI Codex"]),
    adapter(Kind::Continue, "continue", &["cn"], &["Continue CLI"]),
    adapter(
        Kind::Copilot,
        "copilot",
        &["copilot"],
        &["GitHub Copilot CLI"],
    ),
    adapter(Kind::Crush, "crush", &["crush"], &["Crush"]),
    adapter(
        Kind::CursorAgent,
        "cursor-agent",
        &["cursor-agent"],
        &["Cursor Agent", "cursor-agent"],
    ),
    adapter(Kind::GeminiCli, "gemini-cli", &["gemini"], &["Gemini CLI"]),
    adapter(Kind::Goose, "goose", &["goose"], &[]),
    adapter(Kind::Grok, "grok", &["grok"], &["Grok CLI"]),
    adapter(
        Kind::GrokBuild,
        "grok-build",
        &[
            "grok-linux-aarch64",
            "grok-linux-x86_64",
            "grok-macos-aarch64",
            "grok-macos-x86_64",
            "grok-windows-aarch64",
            "grok-windows-x86_64",
        ],
        &["Grok Build"],
    ),
    adapter(Kind::Jules, "jules", &["jules"], &["Jules CLI"]),
    adapter(Kind::Kilo, "kilo", &["kilo", "kilocode"], &["Kilo Code"]),
    adapter(
        Kind::Kiro,
        "kiro",
        &["kiro-cli", "kiro-cli-chat"],
        &["Kiro CLI", "kiro-cli-chat"],
    ),
    adapter(
        Kind::MinimaxCode,
        "minimax-code",
        &["mcode", "minimax-code"],
        &["MiniMax Code", "minimax-code"],
    ),
    adapter(Kind::Opencode, "opencode", &["opencode"], &["OpenCode"]),
    adapter(
        Kind::QwenCode,
        "qwen-code",
        &["qwen", "qwen-code"],
        &["Qwen Code", "qwen-code"],
    ),
];

const SCOPED_PACKAGES: &[(&str, &str, Kind)] = &[
    ("@continuedev", "cli", Kind::Continue),
    ("@github", "copilot", Kind::Copilot),
    ("@minimax-ai", "code", Kind::MinimaxCode),
    ("@vibe-kit", "grok-cli", Kind::Grok),
    ("cursor-agent", "versions", Kind::CursorAgent),
];

const fn adapter(
    kind: Kind,
    id: &'static str,
    executables: &'static [&'static str],
    titles: &'static [&'static str],
) -> Adapter {
    Adapter {
        kind,
        id,
        executables,
        titles,
    }
}

fn known_title(screen: &ScreenSnapshot) -> Option<Kind> {
    let title = screen.title.trim().trim_start_matches(['✳', '✶']).trim();
    ADAPTERS.iter().find_map(|adapter| {
        adapter
            .titles
            .iter()
            .any(|name| title.eq_ignore_ascii_case(name))
            .then_some(adapter.kind)
    })
}

pub(super) fn process_blocks_action(
    screen: &ScreenSnapshot,
    run: bool,
    image: &str,
    args: &[&str],
) -> bool {
    let Some((kind, needs_screen)) = process_kind(image, args) else {
        return false;
    };
    if run && !needs_screen {
        return true;
    }
    let state = rules::matched_state(screen, kind);
    if run {
        state.is_some()
    } else {
        state == Some(State::Blocked)
    }
}

fn process_kind(image: &str, args: &[&str]) -> Option<(Kind, bool)> {
    let image = image.to_ascii_lowercase();
    let host = executable_name(&image);
    if host == "grok" {
        return Some((Kind::GrokBuild, true));
    }
    if let Some(kind) = executable_kind(host) {
        // These bare native names can refer to unrelated applications.
        return Some((kind, matches!(host, "goose" | "antigravity")));
    }
    let python = ["python", "pypy"].into_iter().any(|name| {
        host.strip_prefix(name).is_some_and(|version| {
            version.is_empty()
                || version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
    });
    if !python && !matches!(host, "node" | "nodejs" | "bun" | "deno") {
        return None;
    }

    script_kind(host, python, args).map(|kind| (kind, false))
}

fn script_kind(host: &str, python: bool, args: &[&str]) -> Option<Kind> {
    let mut arguments = args.iter().copied();
    while let Some(arg) = arguments.next() {
        match arg {
            "-m" if python => return arguments.next().and_then(agent_script),
            "--" => return arguments.next().and_then(agent_script),
            "-c" | "-e" | "--eval" | "-p" | "--print" => return None,
            "-r"
            | "--require"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "--conditions"
            | "-W"
            | "-X" => {
                arguments.next();
            }
            "run" if matches!(host, "bun" | "deno") => {}
            flag if flag.starts_with('-') => {
                if ["-e", "-c", "-p", "--eval=", "--print="]
                    .into_iter()
                    .any(|prefix| flag.starts_with(prefix))
                {
                    return None;
                }
            }
            script if !script.is_empty() => return agent_script(script),
            _ => {}
        }
    }
    None
}

fn executable_name(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or_default();
    [".exe", ".cmd", ".bat", ".com", ".mjs", ".cjs", ".js"]
        .into_iter()
        .find_map(|extension| name.strip_suffix(extension))
        .unwrap_or(name)
}

fn executable_kind(name: &str) -> Option<Kind> {
    ADAPTERS
        .iter()
        .find(|adapter| adapter.executables.contains(&name))
        .map(|adapter| adapter.kind)
}

fn package_kind(id: &str) -> Option<Kind> {
    ADAPTERS
        .iter()
        .find(|adapter| adapter.id == id)
        .map(|adapter| adapter.kind)
}

fn agent_display(kind: Kind) -> (&'static str, &'static str) {
    let adapter = ADAPTERS
        .iter()
        .find(|adapter| adapter.kind == kind)
        .expect("known coding-agent adapter");
    let name = match kind {
        Kind::Continue => "Continue",
        Kind::Goose => "Goose",
        Kind::Jules => "Jules",
        _ => adapter.titles[0],
    };
    (adapter.id, name)
}

fn agent_script(script: &str) -> Option<Kind> {
    let path = script.to_ascii_lowercase();
    let name = executable_name(&path);
    if let Some(kind) = executable_kind(name).or_else(|| package_kind(name)) {
        return Some(kind);
    }
    let parts = path.split(['/', '\\']).filter(|part| !part.is_empty());
    let mut parent = "";
    for part in parts {
        let scope = parent.strip_prefix("npm:").unwrap_or(parent);
        if let Some((_, _, kind)) = SCOPED_PACKAGES
            .iter()
            .find(|(parent, child, _)| *parent == scope && *child == part)
        {
            return Some(*kind);
        }
        if matches!(
            scope,
            "node_modules" | "@anthropic-ai" | "@openai" | "@google" | "@qwen-code"
        ) && let Some(kind) = package_kind(part)
        {
            return Some(kind);
        }
        parent = part;
    }
    None
}

fn visible_lines(screen: &ScreenSnapshot) -> Vec<String> {
    (0..screen.rows)
        .rev()
        .filter_map(|row| {
            let mut text = String::new();
            for cell in screen.row(row)?.iter().take(MAX_CELLS_PER_LINE) {
                if cell.width == 0 {
                    continue;
                }
                if cell.style.hidden || cell.text.is_empty() {
                    text.push(' ');
                } else {
                    text.extend(cell.text.chars().take(8));
                }
            }
            let text = text.trim_start_matches([
                ' ', '│', '┃', '|', '>', '›', '❯', '→', '●', '○', '•', '◆', '△',
            ]);
            let words = text.split_whitespace().collect::<Vec<_>>();
            (!words.is_empty()).then(|| words.join(" ").to_ascii_lowercase())
        })
        .take(MAX_LINES)
        .collect()
}

fn approval(lines: &[String], agent_title: bool) -> bool {
    let starts = |prefix: &str| lines.iter().any(|line| line.starts_with(prefix));
    let contains = |needle: &str| lines.iter().any(|line| line.contains(needle));
    // These cues identify an approval picker without requiring the agent's title.
    if [
        "[y] approve [n] deny",
        "run (once) (y)",
        "skip & tell the agent what to do instead (esc or n)",
        "allow once always allow reject",
        "allow once allow always reject",
        "allow allow for session deny",
        "cline is asking a question",
        "goose would like to call the above tool",
        "tell kilo what to do differently",
        "↑/↓ choose · enter confirm · esc deny",
    ]
    .into_iter()
    .any(starts)
    {
        return true;
    }
    if contains("esc to interrupt") {
        return false;
    }
    // Short questions occur in ordinary shells, so look for the associated picker.
    let question = [
        "allow command?",
        "would you like to run the following command?",
        "do you want to proceed?",
        "do you want to make this edit",
        "do you want to create",
        "do you want to run",
        "do you want to allow",
        "would you like to proceed?",
        "allow execution of:",
        "apply this change?",
        "run this command?",
    ]
    .into_iter()
    .any(starts);
    let choices = starts("1. yes (y)")
        || starts("1. yes, proceed (y)")
        || starts("1. yes, allow once")
        || starts("1. yes, allow always")
        || (starts("1. yes") && starts("2. yes, and don't ask again"));
    let footer = starts("press enter to confirm or esc to cancel");
    question && (choices || footer)
        || (agent_title
            && (question
                || footer
                || starts("do you trust the contents of this directory")
                || contains("(y)es/(n)o")
                || lines.iter().any(|line| line.ends_with("[y/n]"))))
}

pub(super) fn coding_agent_visible(screen: &ScreenSnapshot) -> bool {
    if known_title(screen).is_some() {
        return true;
    }
    let lines = visible_lines(screen);
    approval(&lines, false)
        || lines.iter().any(|line| {
            [
                "_ openai codex (v",
                "openai codex (v",
                "ask mcode to do anything",
                "plan, search, build anything",
                "type your message or @path/to/file",
                "message · enter send",
                "prompt · enter send",
            ]
            .into_iter()
            .any(|prefix| line.starts_with(prefix))
                || line.ends_with("plan ● act (tab)")
                || line.ends_with("plan ○ act (tab)")
        })
}

pub(super) fn approval_prompt_visible(screen: &ScreenSnapshot) -> bool {
    let kind = known_title(screen);
    kind.and_then(|kind| rules::matched_state(screen, kind)) == Some(State::Blocked)
        || approval(&visible_lines(screen), kind.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kortty_terminal::Terminal;

    fn snapshot(bytes: &[u8]) -> ScreenSnapshot {
        let mut terminal = Terminal::new(100, 24);
        terminal.advance(bytes);
        terminal.snapshot()
    }

    #[test]
    fn coding_agent_guard_distinguishes_idle_composers_and_approval_pickers() {
        let title = snapshot("\x1b]2;✳ Claude Code\x07❯ ".as_bytes());
        assert!(coding_agent_visible(&title));
        assert!(!approval_prompt_visible(&title));
        let composer = snapshot(b">_ OpenAI Codex (v0.162.0)\r\nEnter a prompt");
        assert!(coding_agent_visible(&composer));
        assert!(!approval_prompt_visible(&composer));

        for prompt in [
            "Allow command?\r\n› 1. Yes (y)\r\n2. No",
            "│ Do you want to proceed? │\r\n│ ❯ 1. Yes │\r\n│ 2. Yes, and don't ask again │",
            "┃ Allow once   Always allow   Reject",
            "[y] Approve   [n] Deny",
            "Run this command?\r\n→ Run (once) (y)",
            "Allow execution of: curl?\r\n› 1. Yes, allow once",
        ] {
            let screen = snapshot(prompt.as_bytes());
            assert!(approval_prompt_visible(&screen), "{prompt}");
            assert!(coding_agent_visible(&screen), "{prompt}");
        }
    }

    #[test]
    fn coding_agent_guard_ignores_shell_text_concealed_cells_and_history() {
        let screen = snapshot(
            b"\x1b]2;user@host:~/codex\x07Allow command?\r\n1. Yes\r\n2. No\r\nContinue [y/n]\r\nuser@host$ ",
        );
        assert!(!coding_agent_visible(&screen));
        assert!(!approval_prompt_visible(&screen));
        let screen = snapshot(b"\x1b[8m[y] Approve   [n] Deny\x1b[0m");
        assert!(!approval_prompt_visible(&screen));

        let mut terminal = Terminal::new(80, 2);
        terminal.advance(b"[y] Approve   [n] Deny\r\ncleared\r\nnormal shell$ ");
        let screen = terminal.snapshot();
        assert!(screen.scrollback_lines > 0);
        assert!(!approval_prompt_visible(&screen));
    }

    #[test]
    fn coding_agent_guard_matches_executables_and_interpreter_scripts() {
        let screen = snapshot(b"");
        for (image, args) in [
            (r"C:\tools\Codex.EXE", &[][..]),
            ("/bin/grok-linux-x86_64", &[]),
            (
                "/usr/bin/nodejs",
                &[
                    "-r",
                    "dotenv/config",
                    "/lib/node_modules/@openai/codex/cli.mjs",
                ],
            ),
            (
                "/opt/cursor-agent/node",
                &["/opt/cursor-agent/versions/2026.1/index.js"],
            ),
            ("/usr/bin/python3.13", &["-m", "aider"]),
            (
                "/usr/bin/deno",
                &["run", "--allow-net", "npm:@openai/codex"],
            ),
            (
                "/usr/bin/node",
                &["/opt/node_modules/@github/copilot/index.js"],
            ),
        ] {
            assert!(
                process_blocks_action(&screen, true, image, args),
                "{image} {args:?}"
            );
        }
        for (image, args) in [
            ("/usr/bin/bash", &["/usr/bin/codex"][..]),
            ("/usr/bin/node", &["/home/me/codex/server.js"]),
            (
                "/usr/bin/node",
                &["server.js", "/lib/node_modules/@openai/codex"],
            ),
            ("/usr/bin/node", &["--eval", "0", "codex"]),
            ("/usr/bin/python3", &["-cprint(0)", "aider"]),
            ("/usr/bin/python-app", &["aider"]),
            ("/usr/bin/goose", &[]),
            ("/usr/bin/grok", &[]),
            ("/usr/bin/antigravity", &[]),
            ("/usr/bin/kiro-cli-term", &[]),
        ] {
            assert!(
                !process_blocks_action(&screen, true, image, args),
                "{image} {args:?}"
            );
        }
    }

    #[test]
    fn coding_agent_guard_requires_matching_process_or_title_for_unbranded_picker() {
        let picker = snapshot(
            "│ How would you like to authenticate for this project? │\r\n│ (Use Enter to select) │"
                .as_bytes(),
        );
        let typed = snapshot("│ Type your message or @path/to/file │".as_bytes());
        assert!(!approval_prompt_visible(&picker));
        assert!(!process_blocks_action(
            &picker,
            false,
            "/usr/bin/node",
            &["other.js"]
        ));
        let args = &["/lib/node_modules/@google/gemini-cli/index.js"];
        assert!(process_blocks_action(&picker, false, "/usr/bin/node", args));
        assert!(!process_blocks_action(&typed, false, "/usr/bin/node", args));
        assert!(process_blocks_action(&typed, true, "/usr/bin/node", args));

        let titled = snapshot("\x1b]2;Gemini CLI\x07│ How would you like to authenticate for this project? │\r\n│ (Use Enter to select) │".as_bytes());
        assert!(approval_prompt_visible(&titled));
        let hidden = snapshot("\x1b]2;Gemini CLI\x07\x1b[8m│ How would you like to authenticate for this project? │\r\n│ (Use Enter to select) │\x1b[0m".as_bytes());
        assert!(!approval_prompt_visible(&hidden));
    }

    #[test]
    fn coding_agent_guard_keeps_generic_approval_on_a_branded_idle_screen() {
        let screen = snapshot(
            "\x1b]2;Gemini CLI\x07│ Type your message or @path/to/file │\r\n[y] Approve   [n] Deny"
                .as_bytes(),
        );
        assert_eq!(
            rules::matched_state(&screen, Kind::GeminiCli),
            Some(State::Idle)
        );
        assert!(approval_prompt_visible(&screen));
    }
}
