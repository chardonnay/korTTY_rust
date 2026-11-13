//! Parses only text captured from local keyboard input or an accepted paste.

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize as _, Zeroizing};

use super::Mode;

pub(crate) const DEFAULT_ALIAS: &str = "agent";
pub(crate) const MAX_ALIAS_BYTES: usize = 32;
const MAX_PROMPT_BYTES: usize = 8192;
const MAX_OPTIONS_BYTES: usize = 512;
const MAX_INPUT_BYTES: usize = MAX_PROMPT_BYTES + MAX_OPTIONS_BYTES + 64;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Preferences {
    pub(crate) enabled: bool,
    pub(crate) name: String,
    pub(crate) case_insensitive: bool,
}

impl Preferences {
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        Aliases::new(&self.name, self.case_insensitive).map(|_| ())
    }

    pub(crate) fn enabled_aliases(&self) -> Option<Aliases> {
        self.enabled
            .then(|| Aliases::new(&self.name, self.case_insensitive).ok())
            .flatten()
    }

    pub(crate) fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct Aliases {
    name: String,
    case_insensitive: bool,
}

pub(crate) struct Invocation<'a> {
    pub(crate) mode: Mode,
    pub(crate) goal: &'a str,
    pub(crate) profile: Option<&'a str>,
}

pub(crate) enum Parsed<'a> {
    Ordinary,
    Incomplete,
    Ready(Invocation<'a>),
    Rejected(&'static str),
}

impl Aliases {
    pub(crate) fn new(name: &str, case_insensitive: bool) -> Result<Self, &'static str> {
        let name = match name.trim() {
            "" => DEFAULT_ALIAS,
            name => name,
        };
        if name.len() > MAX_ALIAS_BYTES
            || !name.as_bytes()[0].is_ascii_alphabetic()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(
                "Enter an agent alias of at most 32 letters, digits, hyphens or underscores, beginning with a letter.",
            );
        }
        Ok(Self {
            name: name.into(),
            case_insensitive,
        })
    }

    pub(crate) fn commands(&self) -> [String; 3] {
        [
            self.name.clone(),
            format!("{}-ask", self.name),
            format!("{}-plan", self.name),
        ]
    }

    pub(crate) fn completion_variants(&self, text: &str) -> Vec<String> {
        if text.is_empty() || text.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        self.commands()
            .into_iter()
            .filter(|variant| self.strip_prefix(variant, text).is_some())
            .collect()
    }

    pub(crate) fn history_prefix<'a>(&self, text: &'a str) -> Option<&'a str> {
        let end = text.find(char::is_whitespace)?;
        let (token, rest) = text.split_at(end);
        (self.alias_mode(token).is_some() && rest.trim().is_empty()).then_some(token)
    }

    pub(crate) fn alias_mode(&self, command: &str) -> Option<Mode> {
        let (mode, tail) = self.command_head(command)?;
        tail.is_empty().then_some(mode)
    }

    pub(crate) fn parse<'a>(&self, input: &'a str) -> Parsed<'a> {
        let input = input.trim();
        let Some((mut mode, mut rest)) = self.command_head(input) else {
            return Parsed::Ordinary;
        };
        if input.len() > MAX_INPUT_BYTES || !input.chars().all(shortcut_character) {
            return Parsed::Rejected(
                "The agent shortcut contains invalid text or exceeds its size limit.",
            );
        }
        let mut profile = None;
        if mode == Mode::Ask && rest.trim_start().starts_with('(') {
            return Parsed::Rejected("The Ask shortcut takes a question after the alias.");
        }
        if let Err(error) = parse_options(&mut rest, &mut profile) {
            return Parsed::Rejected(error);
        }
        if mode == Mode::Run
            && rest.starts_with(char::is_whitespace)
            && let Some(flag) = self
                .strip_prefix(rest.trim_start(), "-plan")
                .filter(|tail| boundary(tail))
        {
            mode = Mode::Plan;
            rest = flag;
            if let Err(error) = parse_options(&mut rest, &mut profile) {
                return Parsed::Rejected(error);
            }
        }
        let goal = if let Some(after_colon) = rest.trim_start().strip_prefix(':') {
            after_colon.trim()
        } else if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            rest.trim()
        } else {
            return Parsed::Rejected("Put a space or a colon before the agent prompt.");
        };
        if goal.is_empty() {
            Parsed::Incomplete
        } else if goal.len() > MAX_PROMPT_BYTES {
            Parsed::Rejected("Enter an agent prompt of at most 8 KiB.")
        } else {
            Parsed::Ready(Invocation {
                mode,
                goal,
                profile,
            })
        }
    }

    fn command_head<'a>(&self, input: &'a str) -> Option<(Mode, &'a str)> {
        let rest = self.strip_prefix(input, &self.name)?;
        if let Some(tail) = self
            .strip_prefix(rest, "-ask")
            .filter(|tail| boundary(tail))
        {
            return Some((Mode::Ask, tail));
        }
        if let Some(tail) = self
            .strip_prefix(rest, "-plan")
            .filter(|tail| boundary(tail))
        {
            return Some((Mode::Plan, tail));
        }
        boundary(rest).then_some((Mode::Run, rest))
    }

    fn strip_prefix<'a>(&self, input: &'a str, prefix: &str) -> Option<&'a str> {
        if self.case_insensitive {
            let (candidate, rest) = input.split_at_checked(prefix.len())?;
            candidate.eq_ignore_ascii_case(prefix).then_some(rest)
        } else {
            input.strip_prefix(prefix)
        }
    }

    fn prefix(&self, text: &str) -> Prefix {
        if text.is_empty() {
            return Prefix::Ordinary;
        }
        if self
            .command_head(text)
            .is_some_and(|(_, rest)| !rest.is_empty())
        {
            return Prefix::Shortcut;
        }
        if self.strip_prefix(&self.name, text).is_some()
            || self.strip_prefix(text, &self.name).is_some_and(|tail| {
                ["-ask", "-plan"]
                    .iter()
                    .any(|candidate| self.strip_prefix(candidate, tail).is_some())
            })
        {
            Prefix::Potential
        } else {
            Prefix::Ordinary
        }
    }
}

fn boundary(tail: &str) -> bool {
    tail.is_empty() || tail.starts_with([':', '(']) || tail.starts_with(char::is_whitespace)
}

fn parse_options<'a>(
    rest: &mut &'a str,
    profile: &mut Option<&'a str>,
) -> Result<(), &'static str> {
    let Some(open) = rest.trim_start().strip_prefix('(') else {
        return Ok(());
    };
    let (options, remaining) = open
        .split_once(')')
        .ok_or("Close the agent shortcut options with a parenthesis.")?;
    if options.len() > MAX_OPTIONS_BYTES {
        return Err("The agent shortcut options exceed their size limit.");
    }
    for option in options
        .split(',')
        .map(str::trim)
        .filter(|option| !option.is_empty())
    {
        let (key, value) = option.split_once('=').unwrap_or((option, ""));
        let value = value.trim();
        match key.trim().to_ascii_lowercase().as_str() {
            "profile" if !value.is_empty() && value.len() <= 128 => *profile = Some(value),
            "profile" => return Err("Enter a profile name after profile=."),
            "ask" if boolean(value).is_some() => {}
            "root" if boolean(value) == Some(false) => {}
            "root" if boolean(value) == Some(true) => {
                return Err("Approve elevated commands individually in the agent activity window.");
            }
            "root" | "ask" => return Err("Enter true or false for the agent approval option."),
            _ => return Err("Unknown agent shortcut option."),
        }
    }
    *rest = remaining;
    Ok(())
}

fn boolean(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Some(true),
        "" | "0" | "false" | "no" | "n" | "off" => Some(false),
        _ => None,
    }
}

enum Prefix {
    Potential,
    Shortcut,
    Ordinary,
}

/// Buffers exact terminal bytes only while the command could be an alias.
/// The caller owns prompt/stream identity and discards this state on a change.
#[derive(Default)]
pub(crate) struct PendingPrefix {
    text: Zeroizing<String>,
    bytes: Zeroizing<Vec<u8>>,
    composing: bool,
}

pub(crate) enum Input {
    /// Forward the current input through the existing terminal route.
    Forward,
    /// Preserve the pending prefix without writing terminal bytes.
    Wait,
    /// Send exact bytes once, optionally before forwarding the current event.
    Replay {
        bytes: Zeroizing<Vec<u8>>,
        before_current: bool,
    },
    /// Submit the local composer. No terminal bytes were sent.
    Compose(Zeroizing<String>),
    Rejected(&'static str),
}

impl PendingPrefix {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn composing(&self) -> bool {
        self.composing
    }

    pub(crate) fn backspace(&mut self) {
        self.text.pop();
    }

    pub(crate) fn replace_with_completion(&mut self, text: &str) {
        self.text.zeroize();
        self.text.push_str(text);
        self.bytes.zeroize();
        self.composing = true;
    }

    pub(crate) fn push(&mut self, aliases: &Aliases, text: &str, bytes: &[u8]) -> Input {
        if text.is_empty() {
            return Input::Forward;
        }
        if !self.composing
            && self.text.is_empty()
            && matches!(aliases.prefix(text), Prefix::Ordinary)
        {
            return Input::Forward;
        }
        if self.text.len().saturating_add(text.len()) > MAX_INPUT_BYTES
            || self.bytes.len().saturating_add(bytes.len()) > 4 * MAX_INPUT_BYTES
        {
            let mut head = Zeroizing::new(self.text.to_string());
            let remaining = 64usize.saturating_sub(head.len());
            let end = text.floor_char_boundary(text.len().min(remaining));
            head.push_str(&text[..end]);
            if self.composing || matches!(aliases.prefix(&head), Prefix::Shortcut) {
                self.composing = true;
                self.bytes.zeroize();
                return Input::Rejected("The agent shortcut exceeds its input size limit.");
            }
            return self.replay_before();
        }
        self.text.push_str(text);
        if self.composing {
            return Input::Wait;
        }
        self.bytes.extend_from_slice(bytes);
        match aliases.prefix(&self.text) {
            Prefix::Potential => Input::Wait,
            Prefix::Shortcut => {
                self.composing = true;
                self.bytes.zeroize();
                Input::Wait
            }
            Prefix::Ordinary => self.flush(false),
        }
    }

    pub(crate) fn paste(&mut self, aliases: &Aliases, text: &str) -> Input {
        if !text.chars().all(shortcut_character) {
            return if self.composing {
                Input::Rejected("Paste a single line of text into the agent shortcut.")
            } else {
                self.replay_before()
            };
        }
        if !self.composing {
            let mut head = Zeroizing::new(self.text.to_string());
            let remaining = 64usize.saturating_sub(head.len());
            let end = text.floor_char_boundary(text.len().min(remaining));
            head.push_str(&text[..end]);
            if !matches!(aliases.prefix(&head), Prefix::Shortcut) {
                return self.replay_before();
            }
            self.composing = true;
            self.bytes.zeroize();
        }
        self.push(aliases, text, &[])
    }

    fn flush(&mut self, before_current: bool) -> Input {
        self.text.zeroize();
        if self.bytes.is_empty() {
            Input::Forward
        } else {
            Input::Replay {
                bytes: std::mem::take(&mut self.bytes),
                before_current,
            }
        }
    }

    pub(crate) fn replay_before(&mut self) -> Input {
        self.flush(true)
    }

    /// Enter submits a reserved alias. A partial name replays before Enter.
    pub(crate) fn submit(&mut self, aliases: &Aliases) -> Input {
        if self.composing || aliases.command_head(&self.text).is_some() {
            self.bytes.zeroize();
            return Input::Compose(std::mem::take(&mut self.text));
        }
        self.replay_before()
    }
}

pub(crate) fn shortcut_character(character: char) -> bool {
    !character.is_control() && crate::session_journal::is_display_character(character)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_alias_completion_only_matches_captured_variants_and_empty_goals() {
        let aliases = Aliases::new("inspect", true).unwrap();
        assert_eq!(
            aliases.completion_variants("INSPECT"),
            ["inspect", "inspect-ask", "inspect-plan"]
        );
        assert_eq!(aliases.completion_variants("INSPECT-P"), ["inspect-plan"]);
        assert!(aliases.completion_variants("").is_empty());
        assert!(aliases.completion_variants("inspection").is_empty());
        assert_eq!(aliases.history_prefix("INSPECT-ASK "), Some("INSPECT-ASK"));
        assert!(aliases.history_prefix("inspect task").is_none());
        assert!(aliases.history_prefix("inspect -plan ").is_none());
        assert!(aliases.history_prefix("inspect(profile=model) ").is_none());
    }

    #[test]
    fn agent_alias_parser_matches_pinned_modes_profiles_and_boundaries() {
        let aliases = Aliases::new(" inspect ", true).unwrap();
        for (text, mode, goal, profile) in [
            ("INSPECT: check ä", Mode::Run, "check ä", None),
            ("inspect-ASK ask here", Mode::Ask, "ask here", None),
            (
                "inspect-plan(profile=Local model) : design",
                Mode::Plan,
                "design",
                Some("Local model"),
            ),
            (
                "inspect(profile=First) -PLAN(profile=Second) rebuild",
                Mode::Plan,
                "rebuild",
                Some("Second"),
            ),
        ] {
            let Parsed::Ready(invocation) = aliases.parse(text) else {
                panic!("expected an agent invocation");
            };
            assert_eq!(invocation.mode, mode);
            assert_eq!(invocation.goal, goal);
            assert_eq!(invocation.profile, profile);
        }
        assert!(matches!(aliases.parse("inspection ask"), Parsed::Ordinary));
        assert!(matches!(aliases.parse("inspect-plan"), Parsed::Incomplete));
        assert!(matches!(
            aliases.parse("inspect(root=true) elevated"),
            Parsed::Rejected(_)
        ));
        assert!(matches!(
            aliases.parse(&format!("inspect {}", "a".repeat(MAX_PROMPT_BYTES + 1))),
            Parsed::Rejected(_)
        ));
        assert!(Aliases::new("#wrong", false).is_err());
    }

    #[test]
    fn agent_alias_prefix_replays_exact_non_shortcut_input_and_never_replays_a_committed_alias() {
        let aliases = Aliases::new("", false).unwrap();
        let mut ordinary = PendingPrefix::default();
        assert!(matches!(ordinary.push(&aliases, "a", b"a"), Input::Wait));
        let Input::Replay {
            bytes,
            before_current: false,
        } = ordinary.push(&aliases, "pt", b"pt")
        else {
            panic!("expected terminal replay");
        };
        assert_eq!(bytes.as_slice(), b"apt");
        assert!(matches!(
            ordinary.push(&aliases, " agent", b" agent"),
            Input::Forward
        ));

        let mut command = PendingPrefix::default();
        assert!(matches!(
            command.push(&aliases, "agent-p", b"agent-p"),
            Input::Wait
        ));
        assert!(matches!(
            command.push(&aliases, "lan ", b"lan "),
            Input::Wait
        ));
        assert!(command.composing());
        assert!(matches!(
            command.paste(&aliases, "make ä plan"),
            Input::Wait
        ));
        assert!(matches!(
            command.paste(&aliases, "newline\ncommand"),
            Input::Rejected(_)
        ));
        let Input::Compose(text) = command.submit(&aliases) else {
            panic!("expected local composer");
        };
        assert_eq!(&*text, "agent-plan make ä plan");
        assert!(matches!(command.replay_before(), Input::Forward));
    }

    #[test]
    fn agent_alias_prefix_limits_never_turn_oversized_shortcuts_into_terminal_input() {
        let aliases = Aliases::new("", false).unwrap();
        let mut state = PendingPrefix::default();
        let oversized = format!("agent {}", "a".repeat(MAX_INPUT_BYTES));
        assert!(matches!(
            state.push(&aliases, &oversized, oversized.as_bytes()),
            Input::Rejected(_)
        ));
        let mut state = PendingPrefix::default();
        assert!(matches!(state.push(&aliases, "a", b"a"), Input::Wait));
        let ordinary = format!("pt {}", "b".repeat(MAX_INPUT_BYTES));
        let Input::Replay {
            bytes,
            before_current: true,
        } = state.push(&aliases, &ordinary, ordinary.as_bytes())
        else {
            panic!("expected exact earlier prefix before untouched input");
        };
        assert_eq!(bytes.as_slice(), b"a");
    }

    #[test]
    fn agent_alias_enter_keeps_complete_reserved_name_local_and_replays_a_partial_name() {
        let aliases = Aliases::new("", false).unwrap();
        let mut state = PendingPrefix::default();
        assert!(matches!(
            state.push(&aliases, "agent", b"agent"),
            Input::Wait
        ));
        let Input::Compose(command) = state.submit(&aliases) else {
            panic!("expected local alias help");
        };
        assert_eq!(&*command, "agent");
        assert!(matches!(aliases.parse(&command), Parsed::Incomplete));

        let mut state = PendingPrefix::default();
        assert!(matches!(state.push(&aliases, "ag", b"ag"), Input::Wait));
        let Input::Replay {
            bytes,
            before_current: true,
        } = state.submit(&aliases)
        else {
            panic!("expected the original partial name before Enter");
        };
        assert_eq!(bytes.as_slice(), b"ag");
    }
}
