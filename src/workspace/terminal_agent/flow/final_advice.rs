//! The turn-limit advisory uses only the run's own reviewed evidence.

use serde_json::Value;

use super::*;

impl Run {
    pub(in crate::workspace::terminal_agent) fn final_advice_input(
        &self,
        limit: usize,
    ) -> Result<String, String> {
        let target = self
            .target
            .as_ref()
            .ok_or("The reviewed agent target is unavailable.")?;
        let results = self.final_evidence.input(&self.redactor)?;
        if results.is_empty() {
            return Err(TURN_LIMIT_REACHED.into());
        }
        let mask = |value| mask::prompt(&self.redactor, value);
        let mut input = json!({
            "goal": mask(self.goal.trim())?,
            "target": {
                "label": mask(&self.label)?,
                "shell": mask(target.shell())?,
                "directory": mask(target.directory())?,
            },
            "context": mask(&self.context)?,
            "acceptedPlan": self.plan.as_deref().map(mask).transpose()?,
            "turn": self.turns,
            "turnLimit": engine::MAX_TURNS,
            "probe": self.final_evidence.probe(&self.redactor)?,
            "commandResults": results,
        });
        if let Some(encoded) = within_limit(&input, limit) {
            return Ok(encoded);
        }
        input["probe"] = Value::Null;
        if let Some(encoded) = within_limit(&input, limit) {
            return Ok(encoded);
        }
        while input["commandResults"]
            .as_array()
            .is_some_and(|results| results.len() > 1)
        {
            input["commandResults"].as_array_mut().unwrap().remove(0);
            if let Some(encoded) = within_limit(&input, limit) {
                return Ok(encoded);
            }
        }
        // Preserve the latest reviewed command and its status when surrounding
        // context, plan, or output tails would exceed the profile limit.
        input["context"] = Value::Null;
        if let Some(encoded) = within_limit(&input, limit) {
            return Ok(encoded);
        }
        input["acceptedPlan"] = Value::Null;
        if let Some(encoded) = within_limit(&input, limit) {
            return Ok(encoded);
        }
        for cap in [1000, 250, 0] {
            let result = &mut input["commandResults"][0];
            for (tail, truncated) in [
                ("stdoutTail", "stdoutTruncated"),
                ("stderrTail", "stderrTruncated"),
            ] {
                if let Some(text) = result[tail].as_str()
                    && let Some((index, character)) = text.char_indices().rev().nth(cap)
                {
                    result[tail] = json!(&text[index + character.len_utf8()..]);
                    result[truncated] = Value::Bool(true);
                }
            }
            if let Some(encoded) = within_limit(&input, limit) {
                return Ok(encoded);
            }
        }
        Err("The final response evidence exceeds the selected profile's context limit.".into())
    }

    pub(super) fn accept_final_advice(
        &mut self,
        workspace: &Workspace,
        result: &Result<Decision, String>,
    ) -> Result<(), String> {
        match result {
            Ok(decision) if matches!(decision.action, Action::Done | Action::Blocked) => {
                self.accept_decision(workspace, decision)
            }
            Ok(_) => Err(self.final_advice_failed("The final response requires done or blocked.")),
            Err(error) => Err(self.final_advice_failed(error)),
        }
    }

    pub(in crate::workspace::terminal_agent) fn final_advice_failed(
        &mut self,
        error: &str,
    ) -> String {
        let detail = mask::prompt(&self.redactor, error)
            .unwrap_or_else(|_| "The final response could not be used safely.".into());
        self.record(format!("Final AI response unavailable\n{detail}"));
        TURN_LIMIT_REACHED.into()
    }
}

fn within_limit(value: &Value, limit: usize) -> Option<String> {
    let encoded = value.to_string();
    (encoded.chars().count() <= limit).then_some(encoded)
}
