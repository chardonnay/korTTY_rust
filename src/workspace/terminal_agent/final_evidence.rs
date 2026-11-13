//! Reviewed command results available for the run's final model response.

use std::collections::VecDeque;

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{Command, Elevation, Outcome, Phase, Redactor, engine, mask};

const MAX_RESULTS: usize = 24;
const MAX_TAIL_CHARS: usize = 4_000;

#[derive(Default)]
pub(super) struct Evidence {
    pending: Option<(u64, Command)>,
    probe: Option<Output>,
    results: VecDeque<ResultEntry>,
}

impl Evidence {
    pub(super) fn is_empty(&self) -> bool {
        self.results.is_empty()
    }

    pub(super) fn start(&mut self, epoch: u64, command: &Command) {
        self.pending = Some((epoch, command.clone()));
    }

    pub(super) fn record_output(
        &mut self,
        epoch: u64,
        phase: Phase,
        outcome: &Outcome,
        stdout: String,
        stderr: String,
    ) {
        let engine::Status::Exited(exit_status) = outcome.status else {
            return;
        };
        let output = Output {
            stdout: Zeroizing::new(stdout),
            stderr: Zeroizing::new(stderr),
            stdout_truncated: outcome.stdout.truncated(),
            stderr_truncated: outcome.stderr.truncated(),
        };
        match phase {
            Phase::Probe if exit_status == 0 => self.probe = Some(output),
            Phase::Running | Phase::Elevated => self.finish(epoch, exit_status, Some(output)),
            _ => {}
        }
    }

    pub(super) fn record_password(&mut self, epoch: u64, exit_status: u32) {
        self.finish(epoch, exit_status, None);
    }

    fn finish(&mut self, epoch: u64, exit_status: u32, output: Option<Output>) {
        let Some((_, command)) = self.pending.take_if(|(owner, command)| {
            *owner == epoch && (command.elevation == Elevation::PasswordSudo) == output.is_none()
        }) else {
            return;
        };
        if self.results.len() == MAX_RESULTS {
            self.results.pop_front();
        }
        self.results.push_back(ResultEntry {
            command,
            exit_status,
            output,
        });
    }

    pub(super) fn probe(&self, redactor: &Redactor) -> Result<Option<Value>, String> {
        self.probe
            .as_ref()
            .map(|output| {
                let mut value = output.input(redactor)?;
                value["exitStatus"] = json!(0);
                Ok(value)
            })
            .transpose()
    }

    pub(super) fn input(&self, redactor: &Redactor) -> Result<Vec<Value>, String> {
        self.results
            .iter()
            .map(|result| result.input(redactor))
            .collect()
    }
}

struct ResultEntry {
    command: Command,
    exit_status: u32,
    output: Option<Output>,
}

impl ResultEntry {
    fn input(&self, redactor: &Redactor) -> Result<Value, String> {
        let mut value = match &self.output {
            Some(output) => output.input(redactor)?,
            None => json!({"outputWithheld":true}),
        };
        value["exitStatus"] = json!(self.exit_status);
        value["command"] = json!(mask::prompt(redactor, &self.command.command)?);
        value["purpose"] = json!(mask::prompt(redactor, &self.command.purpose)?);
        value["risk"] = json!(match self.command.risk {
            engine::Risk::ReadOnly => "read_only",
            engine::Risk::RequiresConfirmation => "requires_confirmation",
        });
        value["elevation"] = json!(match self.command.elevation {
            Elevation::None => "none",
            Elevation::PasswordlessSudo => "passwordless_sudo",
            Elevation::PasswordSudo => "password_sudo",
        });
        Ok(value)
    }
}

struct Output {
    stdout: Zeroizing<String>,
    stderr: Zeroizing<String>,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

impl Output {
    fn input(&self, redactor: &Redactor) -> Result<Value, String> {
        // A password learned after an earlier command must be masked before
        // taking a tail or escaping quotes and backslashes for JSON.
        let stdout = Zeroizing::new(mask::prompt(redactor, &self.stdout)?);
        let stderr = Zeroizing::new(mask::prompt(redactor, &self.stderr)?);
        let (stdout, stdout_truncated) = tail(&stdout, self.stdout_truncated);
        let (stderr, stderr_truncated) = tail(&stderr, self.stderr_truncated);
        Ok(json!({
            "stdoutTail": stdout,
            "stderrTail": stderr,
            "stdoutTruncated": stdout_truncated,
            "stderrTruncated": stderr_truncated,
        }))
    }
}

fn tail(value: &str, already_truncated: bool) -> (&str, bool) {
    match value.char_indices().rev().nth(MAX_TAIL_CHARS) {
        Some((start, character)) => (&value[start + character.len_utf8()..], true),
        None => (value, already_truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_logging::{Policy, Secrets};

    fn command(elevation: Elevation) -> Command {
        Command {
            command: "printf observation".into(),
            purpose: "Check the service".into(),
            risk: engine::Risk::ReadOnly,
            elevation,
        }
    }

    fn outcome(status: engine::Status) -> Outcome {
        Outcome {
            status,
            stdout: Default::default(),
            stderr: Default::default(),
        }
    }

    #[test]
    fn probe_and_unowned_or_partial_results_do_not_enable_finalization() {
        let redactor = Redactor::for_journal(Secrets::default().seal().unwrap(), Policy::default());
        let mut evidence = Evidence::default();
        evidence.record_output(
            1,
            Phase::Probe,
            &outcome(engine::Status::Exited(0)),
            "environment".into(),
            String::new(),
        );
        assert_eq!(
            evidence.probe(&redactor).unwrap().unwrap()["stdoutTail"],
            "environment"
        );

        evidence.start(2, &command(Elevation::None));
        evidence.record_output(
            1,
            Phase::Running,
            &outcome(engine::Status::Exited(0)),
            "stale".into(),
            String::new(),
        );
        evidence.record_output(
            2,
            Phase::Running,
            &outcome(engine::Status::TimedOut),
            "partial".into(),
            String::new(),
        );
        assert!(evidence.is_empty());

        for stdout in ["approved output", "duplicate completion"] {
            evidence.record_output(
                2,
                Phase::Running,
                &outcome(engine::Status::Exited(7)),
                stdout.into(),
                String::new(),
            );
        }
        let results = evidence.input(&redactor).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["exitStatus"], 7);
        assert_eq!(results[0]["command"], "printf observation");
        assert_eq!(results[0]["stdoutTail"], "approved output");
    }

    #[test]
    fn final_command_results_remask_learned_passwords_and_withhold_privileged_output() {
        const SECRET: &str = r#"some"quoted\password"#;
        let secrets = Secrets::default();
        secrets.add(SECRET);
        let redactor = Redactor::for_journal(secrets.seal().unwrap(), Policy::default());
        let mut evidence = Evidence::default();
        evidence.start(1, &command(Elevation::None));
        evidence.record_output(
            1,
            Phase::Running,
            &outcome(engine::Status::Exited(0)),
            format!(
                "{}\nLearned later {SECRET}\n終",
                "山".repeat(MAX_TAIL_CHARS)
            ),
            String::new(),
        );

        evidence.start(2, &command(Elevation::PasswordSudo));
        evidence.record_output(
            2,
            Phase::Elevated,
            &outcome(engine::Status::Exited(0)),
            "privileged output is unavailable".into(),
            String::new(),
        );
        evidence.record_password(1, 0);
        assert_eq!(evidence.input(&redactor).unwrap().len(), 1);
        evidence.record_password(2, 0);

        let results = evidence.input(&redactor).unwrap();
        assert_eq!(results.len(), 2);
        let tail = results[0]["stdoutTail"].as_str().unwrap();
        assert_eq!(tail.chars().count(), MAX_TAIL_CHARS);
        assert!(tail.ends_with("Learned later ***\n終"));
        assert_eq!(results[0]["stdoutTruncated"], true);
        assert_eq!(results[1]["outputWithheld"], true);
        assert_eq!(results[1]["exitStatus"], 0);
        assert!(results[1].get("stdoutTail").is_none());
        assert!(results[1].get("stderrTail").is_none());
    }
}
