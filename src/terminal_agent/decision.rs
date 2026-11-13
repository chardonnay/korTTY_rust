//! Bounded model decisions. A parsed command still needs an exact user review.

use std::fmt;

use serde::{Deserialize, de::DeserializeOwned};

pub(crate) const MAX_TURNS: usize = 8;
pub(crate) const MAX_QUESTIONS: usize = 3;
pub(super) const MAX_COMMAND_BYTES: usize = 4096;
const MAX_RESPONSE_BYTES: usize = 24 * 1024;
const MAX_COMMANDS: usize = 3;

#[derive(PartialEq, Eq)]
pub(crate) struct Decision {
    pub(crate) summary: String,
    pub(crate) message: String,
    pub(crate) action: Action,
}

impl fmt::Debug for Decision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Terminal agent decision")
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum Action {
    Commands(Vec<Command>),
    Input {
        question: String,
        answers: Vec<String>,
    },
    Done,
    Blocked,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Command {
    pub(crate) command: String,
    pub(crate) purpose: String,
    pub(crate) risk: Risk,
    #[serde(default)]
    pub(crate) elevation: Elevation,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Elevation {
    #[default]
    None,
    PasswordlessSudo,
    PasswordSudo,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Risk {
    ReadOnly,
    RequiresConfirmation,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Response {
    status: Status,
    summary: String,
    user_message: String,
    #[serde(default)]
    commands: Vec<Command>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    answers: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    RunCommands,
    NeedsConfirmation,
    NeedsInput,
    Done,
    Blocked,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FinalResponse {
    status: FinalStatus,
    summary: String,
    user_message: String,
    #[serde(default, rename = "commands")]
    _commands: [(); 0],
    #[serde(default)]
    needs_reprobe: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FinalStatus {
    Done,
    Blocked,
}

impl Decision {
    pub(super) fn parse(text: &str, can_execute: bool) -> Result<Self, &'static str> {
        let response: Response = parse_response(text)?;
        validate_message(&response.summary, &response.user_message)?;
        if !matches!(response.status, Status::NeedsInput)
            && (response.question.is_some() || !response.answers.is_empty())
        {
            return Err("Questions and answers are only valid for an input decision.");
        }
        if !matches!(
            response.status,
            Status::RunCommands | Status::NeedsConfirmation
        ) && !response.commands.is_empty()
        {
            return Err("This agent decision cannot contain commands.");
        }
        let action = match response.status {
            Status::RunCommands | Status::NeedsConfirmation if !can_execute => {
                return Err("This agent run has no command execution authority.");
            }
            Status::RunCommands | Status::NeedsConfirmation => {
                if response.commands.is_empty() || response.commands.len() > MAX_COMMANDS {
                    return Err("An execution decision requires one to three commands.");
                }
                if response.commands.iter().any(|command| {
                    !valid_command(&command.command) || !valid_text(&command.purpose, 512, false)
                }) {
                    return Err("A proposed command is invalid or contains a masked value.");
                }
                Action::Commands(response.commands)
            }
            Status::NeedsInput => {
                let question = response
                    .question
                    .filter(|question| valid_text(question, 512, false))
                    .ok_or("An input decision requires one short question.")?;
                if !(2..=4).contains(&response.answers.len())
                    || response
                        .answers
                        .iter()
                        .any(|answer| !valid_text(answer, 512, false))
                {
                    return Err("An input decision requires two to four suggested answers.");
                }
                Action::Input {
                    question,
                    answers: response.answers,
                }
            }
            Status::Done => Action::Done,
            Status::Blocked => Action::Blocked,
        };
        Ok(Self {
            summary: response.summary,
            message: response.user_message,
            action,
        })
    }

    pub(super) fn parse_final(text: &str) -> Result<Self, &'static str> {
        let response: FinalResponse = parse_response(text)?;
        validate_message(&response.summary, &response.user_message)?;
        if response.needs_reprobe {
            return Err("The final response cannot request a new probe.");
        }
        Ok(Self {
            summary: response.summary,
            message: response.user_message,
            action: match response.status {
                FinalStatus::Done => Action::Done,
                FinalStatus::Blocked => Action::Blocked,
            },
        })
    }
}

fn parse_response<T: DeserializeOwned>(text: &str) -> Result<T, &'static str> {
    if text.len() > MAX_RESPONSE_BYTES {
        return Err("The agent response exceeds 24 KiB.");
    }
    serde_json::from_str(text).map_err(|_| "The agent must return one valid decision object.")
}

fn validate_message(summary: &str, message: &str) -> Result<(), &'static str> {
    if valid_text(summary, 256, false) && valid_text(message, 8192, true) {
        Ok(())
    } else {
        Err("The agent decision requires a summary and message within their limits.")
    }
}

pub(super) fn valid_command(value: &str) -> bool {
    valid_text(value, MAX_COMMAND_BYTES, true) && !value.contains("***")
}

fn valid_text(value: &str, max: usize, multiline: bool) -> bool {
    value.len() <= max
        && !value.trim().is_empty()
        && value.chars().all(|character| {
            if multiline && matches!(character, '\n' | '\t') {
                return true;
            }
            !character.is_control()
                && !matches!(character,
                    '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
                    | '\u{2060}'..='\u{206f}' | '\u{feff}'
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_decision_requires_approval_capability_and_preserves_command_bytes() {
        let text = r#"{"status":"run_commands","summary":"Inspect","userMessage":"Check the path","commands":[{"command":"printf '%s' 'file one'  \n","purpose":"Check a file","risk":"read_only"}]}"#;
        assert!(Decision::parse(text, false).is_err());
        let Action::Commands(commands) = Decision::parse(text, true).unwrap().action else {
            panic!("expected proposed commands");
        };
        assert_eq!(commands[0].command, "printf '%s' 'file one'  \n");
        assert!(commands[0].elevation == Elevation::None);
        assert!(Decision::parse(&format!("{text}{{}}"), true).is_err());
        assert!(!valid_command("echo ***"));
        assert!(!valid_command("echo \u{202e}data"));
        assert!(!valid_command("echo \u{1b}data"));
    }

    #[test]
    fn agent_decision_passwordless_sudo_is_typed_and_never_authorizes_execution() {
        let text = r#"{"status":"run_commands","summary":"Inspect","userMessage":"Check the path","commands":[{"command":"id -u","purpose":"Check the effective user","risk":"requires_confirmation","elevation":"passwordless_sudo"}]}"#;
        let Action::Commands(commands) = Decision::parse(text, true).unwrap().action else {
            panic!("expected proposed commands");
        };
        assert_eq!(commands[0].command, "id -u");
        assert!(commands[0].elevation == Elevation::PasswordlessSudo);
        let password = text.replace("passwordless_sudo", "password_sudo");
        let Action::Commands(commands) = Decision::parse(&password, true).unwrap().action else {
            panic!("expected typed password sudo command");
        };
        assert!(commands[0].elevation == Elevation::PasswordSudo);
        assert!(Decision::parse(&password, false).is_err());
        assert!(Decision::parse(text, false).is_err());
        assert!(
            Decision::parse(&text.replace("passwordless_sudo", "sudo_password"), true).is_err()
        );
        assert!(Decision::parse(&text.replace("passwordless_sudo", "auto_approve"), true).is_err());
    }

    #[test]
    fn agent_decision_rejects_cross_field_and_unknown_data() {
        for bad in [
            r#"{"status":"done","summary":"Done","userMessage":"Result","commands":[{"command":"ls","purpose":"List","risk":"read_only"}]}"#,
            r#"{"status":"done","summary":"Done","userMessage":"Result","tool":"exec"}"#,
            r#"{"status":"needs_input","summary":"Question","userMessage":"Choose","question":"Which file?","answers":["One"]}"#,
            r#"{"status":"run_commands","summary":"Inspect","userMessage":"Check the path","commands":[{"command":"ls","purpose":"List","risk":"safe"}]}"#,
        ] {
            assert!(Decision::parse(bad, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn final_advice_parses_only_terminal_decisions_without_commands_or_reprobe() {
        let done = r#"{"status":"done","summary":"Done","userMessage":"Completed the review","commands":[],"needsReprobe":false}"#;
        let blocked =
            r#"{"status":"blocked","summary":"Blocked","userMessage":"Need the process list"}"#;
        assert!(matches!(
            Decision::parse_final(done).unwrap().action,
            Action::Done
        ));
        assert!(matches!(
            Decision::parse_final(blocked).unwrap().action,
            Action::Blocked
        ));
        for bad in [
            done.replace("done", "run_commands"),
            done.replace("done", "needs_confirmation"),
            done.replace("done", "needs_input"),
            done.replace(
                "[]",
                r#"[{"command":"id","purpose":"Get user","risk":"read_only"}]"#,
            ),
            done.replace("false", "true"),
            done.replace("\"commands\":[]", "\"question\":\"What's next?\""),
            done.replace("Completed the review", ""),
        ] {
            assert!(Decision::parse_final(&bad).is_err(), "{bad}");
        }
    }
}
