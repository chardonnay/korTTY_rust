//! A model proposal uses the same query/page and excerpt reviews as manual research.

use super::{web_lookup::WebAuthority, *};
use crate::terminal_agent::usage::{
    ApprovedWebResult, WebOperation, WebProposal, web_error_result,
};
use iced::widget::{button, column, text};
use iced::{Element, Length};

pub(super) struct Review {
    proposal: Arc<WebProposal>,
    serial: u64,
    scope: web::Scope,
    authority: WebAuthority,
    state: web::State,
    approved_input: Option<String>,
}

impl Drop for Review {
    fn drop(&mut self) {
        self.proposal
            .reject("The model web-tool review was closed.");
    }
}

impl super::Review {
    pub(super) fn can_offer_model_tools(&self, run: &Run) -> bool {
        !run.binding.private()
            && self.transport().0.supports_web_tool_turns()
            && self
                .web_lookup
                .as_ref()
                .is_some_and(|lookup| lookup.scope().uses_direct_tools())
            && local_ai::tools::agent_task_eligible(&run.goal, false)
    }

    pub(in crate::workspace::terminal_agent) fn model_tool_active(&self) -> bool {
        self.model_tool
            .as_ref()
            .is_some_and(|review| !review.proposal.closed())
    }

    pub(in crate::workspace::terminal_agent) fn model_tool_view(
        &self,
        epoch: u64,
        ready: bool,
    ) -> Option<Element<'_, Message>> {
        let review = self
            .model_tool
            .as_ref()
            .filter(|review| !review.proposal.closed())?;
        let serial = review.serial;
        let body = if ready {
            review.state.tool_view()?
        } else {
            column![
                text("Model web-tool review paused"),
                button("Decline tool").on_press(web::Message::Close)
            ]
            .spacing(8)
            .into()
        };
        Some(
            column![
                text(format!(
                    "Model web-tool request · round {} of 2",
                    review.proposal.round
                ))
                .size(16),
                body.map(move |message| Message::ToolWeb(epoch, serial, message))
            ]
            .spacing(8)
            .height(Length::Fill)
            .into(),
        )
    }
}

fn require_call_mask(
    mask: &knowledge_review::Mask,
    call: &local_ai::tools::Call,
) -> Result<(), String> {
    fn check(mask: &knowledge_review::Mask, value: &serde_json::Value) -> Result<(), String> {
        match value {
            serde_json::Value::String(text) => mask.require_current(text)?,
            serde_json::Value::Array(values) => {
                for value in values {
                    check(mask, value)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (key, value) in values {
                    mask.require_current(key)?;
                    check(mask, value)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    mask.require_current(&call.id)?;
    mask.require_current(&call.function.name)?;
    mask.require_current(&call.function.arguments)?;
    let arguments = serde_json::from_str(&call.function.arguments)
        .map_err(|_| "The model tool arguments are invalid.")?;
    check(mask, &arguments).map_err(|_| {
        "The model tool arguments contain text requiring masking. Start a new review.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_tool_mask_checks_raw_numbers_and_decoded_json_strings() {
        let mask = knowledge_review::Mask::new(
            Redactor::for_journal(Secrets::default().seal().unwrap(), Default::default()),
            Some(Zeroizing::new("98765432109876".into())),
            None,
        );
        for arguments in [
            r#"{"query":"public fixture", "extra":98765432109876}"#,
            r#"{"query":"public fixture", "extra":"\u00398765432109876"}"#,
            r#"{"query":"public fixture", "extra":{"98765432109876":true}}"#,
        ] {
            let call: local_ai::tools::Call = serde_json::from_value(serde_json::json!({
                "id":"mask-fixture", "type":"function", "function":{"name":"web_search","arguments":arguments}
            })).unwrap();
            assert!(require_call_mask(&mask, &call).is_err(), "{arguments}");
        }
    }
}

impl Workspace {
    pub(in crate::workspace::terminal_agent) fn begin_model_tool(
        &self,
        run: &mut Run,
        review: &mut super::Review,
        proposal: Arc<WebProposal>,
    ) -> Result<(), String> {
        self.require_model_review_authority(run, review)?;
        if run.swarm
            || !matches!(review.kind, Kind::Run(engine::RunModel::Decision))
            || !review.can_offer_model_tools(run)
            || review.model_tool.is_some()
            || proposal.closed()
        {
            return Err("The model web-tool request is unavailable for this run.".into());
        }
        let scope = review
            .web_lookup
            .as_ref()
            .ok_or("The web integration is unavailable.")?
            .scope()
            .clone();
        let authority = WebAuthority::capture(&scope, self, &review.authority);
        authority.require_current(self)?;
        let mask = authority.mask(run, self.vault.as_ref())?;
        require_call_mask(&mask, &proposal.call)?;
        mask.require_current(&proposal.assistant_context)?;
        let operation = proposal.operation();
        if proposal.limit_reached || operation.is_none() {
            let guard = self.review_dispatch_guard(run, review)?;
            let call = proposal.call.clone();
            let assistant = proposal.assistant_context.clone();
            let content = if proposal.limit_reached {
                web_error_result(
                    "tool_round_limit",
                    "The web-tool round limit was reached. Answer from available evidence.",
                )
            } else {
                web_error_result(
                    "unsupported_or_invalid_tool",
                    "The requested web tool or its arguments are unsupported.",
                )
            };
            proposal.resolve(ApprovedWebResult::new(content, move || {
                authority.check()?;
                guard()?;
                require_call_mask(&mask, &call)?;
                mask.require_current(&assistant)
            }));
            return Ok(());
        }
        let (operation, input) = operation.expect("supported operation");
        let lookup = match operation {
            WebOperation::Search => web::Lookup::Search,
            WebOperation::Page => web::Lookup::Page,
        };
        review.tool_serial = review
            .tool_serial
            .checked_add(1)
            .ok_or("The web-tool review identity is exhausted.")?;
        run.notice = "Review the model's public query or exact page URL before dispatch.".into();
        if proposal.omitted > 0 {
            run.record(format!("The model requested additional tools; {} proposals were omitted by the batch limit.", proposal.omitted));
        }
        review.model_tool = Some(Review {
            proposal,
            serial: review.tool_serial,
            scope: scope.clone(),
            authority,
            state: web::State::Draft(scope, input, lookup),
            approved_input: None,
        });
        Ok(())
    }

    pub(super) fn model_tool_action(
        &self,
        id: window::Id,
        run: &mut Run,
        review: &mut super::Review,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        let Message::ToolWeb(_, serial, message) = message else {
            return Ok(Task::none());
        };
        if review
            .model_tool
            .as_ref()
            .is_none_or(|tool| tool.serial != serial)
        {
            return Ok(Task::none());
        }
        let mut tool = review.model_tool.take().expect("current tool review");
        let checked = (|| {
            if tool.proposal.closed() {
                return Err("The model tool review expired.".into());
            }
            self.require_model_review_authority(run, review)?;
            tool.authority.require_current(self)?;
            let mask = tool.authority.mask(run, self.vault.as_ref())?;
            require_call_mask(&mask, &tool.proposal.call)?;
            mask.require_current(&tool.proposal.assistant_context)?;
            Ok::<_, String>(mask)
        })();
        let mask = match checked {
            Ok(mask) => mask,
            Err(error) => {
                tool.proposal.reject(&error);
                run.notice = error;
                return Ok(Task::none());
            }
        };
        let include = matches!(message, web::Message::Include)
            && matches!(tool.state, web::State::Results(..));
        let close = matches!(message, web::Message::Close);
        if run.paused && !close && !matches!(message, web::Message::Finished(..)) {
            review.model_tool = Some(tool);
            return Ok(Task::none());
        }
        let guard = self.review_dispatch_guard(run, review)?;
        if close {
            let authority = tool.authority.clone();
            let call = tool.proposal.call.clone();
            let assistant = tool.proposal.assistant_context.clone();
            let content = if matches!(&tool.state, web::State::Results(_, results) if results.is_empty())
            {
                web_error_result(
                    "no_results",
                    "The provider returned no usable web excerpts.",
                )
            } else {
                web_error_result("user_declined", "The user declined this web-tool request.")
            };
            tool.proposal
                .resolve(ApprovedWebResult::new(content, move || {
                    authority.check()?;
                    guard()?;
                    require_call_mask(&mask, &call)?;
                    mask.require_current(&assistant)
                }));
            run.record("Model web-tool request declined.".into());
            run.notice = "Continuing the model with the declined-tool result.".into();
            return Ok(Task::none());
        }
        let ticket = if matches!(message, web::Message::Send) {
            review.tool_ticket = review
                .tool_ticket
                .checked_add(1)
                .ok_or("The web request identity is exhausted.")?;
            if let web::State::Reviewed(_, query, _) = &tool.state {
                tool.approved_input = Some(query.clone());
            }
            Some(review.tool_ticket)
        } else {
            None
        };
        let authority = tool.authority.clone();
        let mask_for_worker = mask.clone();
        let epoch = run.epoch;
        let updated = tool.state.update(
            message,
            tool.scope.clone(),
            move |value| mask_for_worker.apply(value),
            ticket,
            move || {
                authority.check()?;
                guard()
            },
            move |message| event(id, Message::ToolWeb(epoch, serial, message)),
        );
        let task = match updated {
            Ok(task) => task,
            Err(error) => {
                run.notice = error;
                Task::none()
            }
        };
        if include && let Some((scope, results)) = tool.state.included() {
            let evidence = web::Evidence::new(scope, results)?;
            evidence.require_mask(|value| mask.apply(value))?;
            let approved_input = tool
                .approved_input
                .as_ref()
                .ok_or("The approved query is unavailable.")?
                .clone();
            mask.require_current(&approved_input)?;
            let mut context = String::new();
            evidence.append_to(&mut context);
            let content = serde_json::json!({"status":"ok","approved_input":approved_input,"evidence":context}).to_string();
            if !tool.proposal.fits_result(&content) {
                tool.state = web::State::Results(tool.scope.clone(), Arc::clone(results));
                run.notice = "The excerpts exceed the remaining model context budget. Decline this tool to continue without them.".into();
                review.model_tool = Some(tool);
                return Ok(task);
            }
            let authority = tool.authority.clone();
            let call = tool.proposal.call.clone();
            let assistant = tool.proposal.assistant_context.clone();
            let guard = self.review_dispatch_guard(run, review)?;
            let output = ApprovedWebResult::new(content, move || {
                authority.check()?;
                guard()?;
                require_call_mask(&mask, &call)?;
                mask.require_current(&assistant)?;
                evidence.require_mask(|value| mask.apply(value))
            });
            if tool.proposal.resolve(output) {
                run.record(format!(
                    "Model web-tool evidence approved for {approved_input}.{context}"
                ));
                run.notice = "Continuing the model with the reviewed web excerpts.".into();
            }
            return Ok(task);
        }
        review.model_tool = Some(tool);
        Ok(task)
    }
}
