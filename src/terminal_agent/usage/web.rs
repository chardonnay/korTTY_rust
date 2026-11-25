//! Model tool rounds awaiting separately reviewed provider results.

use std::{
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use async_channel::{Receiver, Sender};
use serde_json::json;
use zeroize::Zeroizing;

use super::{Attempt, AttemptKind, Progress, authorize_resend, interpret_reply};
use crate::local_ai::{self, Error, tools};

const MAX_EXECUTED_ROUNDS: usize = 2;
const MAX_RESULT_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebOperation {
    Search,
    Page,
}

pub(crate) struct ApprovedWebResult {
    content: Zeroizing<String>,
    guard: Box<dyn Fn() -> Result<(), String> + Send>,
}

impl ApprovedWebResult {
    pub(crate) fn new(
        content: String,
        guard: impl Fn() -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        if content.trim().is_empty() || content.len() > MAX_RESULT_BYTES {
            return Err("The reviewed web result exceeds its response budget.".into());
        }
        guard()?;
        Ok(Self {
            content: Zeroizing::new(content),
            guard: Box::new(guard),
        })
    }
}

pub(crate) struct WebProposal {
    pub(crate) call: tools::Call,
    pub(crate) assistant_context: Zeroizing<String>,
    max_result_chars: usize,
    pub(crate) round: usize,
    pub(crate) omitted: usize,
    pub(crate) limit_reached: bool,
    response: Sender<Result<ApprovedWebResult, String>>,
}

impl fmt::Debug for WebProposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Model web-tool review")
    }
}

impl WebProposal {
    pub(crate) fn fits_result(&self, content: &str) -> bool {
        content.len() <= MAX_RESULT_BYTES
            && serde_json::to_string(&tools::Message::Tool {
                tool_call_id: self.call.id.clone(),
                content: content.into(),
            })
            .is_ok_and(|encoded| encoded.chars().count() <= self.max_result_chars)
    }
    pub(crate) fn operation(&self) -> Option<(WebOperation, String)> {
        operation(&self.call)
    }
    pub(crate) fn resolve(&self, result: Result<ApprovedWebResult, String>) -> bool {
        let sent = self.response.try_send(result).is_ok();
        self.response.close();
        sent
    }
    pub(crate) fn reject(&self, reason: &str) {
        self.resolve(Err(reason.into()));
    }
    pub(crate) fn closed(&self) -> bool {
        self.response.is_closed()
    }
}

impl Drop for WebProposal {
    fn drop(&mut self) {
        self.response.close();
    }
}

#[derive(Default)]
pub(super) struct Session {
    history: Vec<tools::Message>,
    guards: Vec<Box<dyn Fn() -> Result<(), String> + Send>>,
    rounds: usize,
    final_only: bool,
}

pub(super) struct Completion<'a> {
    pub system: &'a str,
    pub input: &'a str,
    pub limit: usize,
    pub progress: &'a Sender<Progress>,
    pub kind: AttemptKind,
    pub deadline: Instant,
    pub output_limit: u32,
}

#[derive(Default)]
struct Totals {
    samples: usize,
    input: Option<u64>,
    output: Option<u64>,
    total: Option<u64>,
}

impl Totals {
    fn observe(&mut self, usage: Option<local_ai::Usage>) {
        let usage = usage.unwrap_or_default();
        let total = usage
            .total_tokens
            .or_else(|| usage.input_tokens?.checked_add(usage.output_tokens?));
        let add = |previous: Option<u64>, next: Option<u64>| {
            if self.samples == 0 {
                next
            } else {
                previous?.checked_add(next?)
            }
        };
        self.input = add(self.input, usage.input_tokens);
        self.output = add(self.output, usage.output_tokens);
        self.total = add(self.total, total);
        self.samples += 1;
    }
}

impl Session {
    fn check(
        &self,
        client: &local_ai::Client,
        deadline: Instant,
        guard: &impl Fn() -> Result<(), String>,
    ) -> Result<(), Error> {
        if !client.active() {
            return Err(Error::Cancelled);
        }
        if deadline <= Instant::now() {
            return Err(Error::TimedOut);
        }
        guard().map_err(Error::Review)?;
        for guard in &self.guards {
            guard().map_err(Error::Review)?;
        }
        Ok(())
    }

    pub(super) fn complete<T>(
        &mut self,
        client: &local_ai::Client,
        request: Completion<'_>,
        parse: &impl Fn(&str) -> Result<T, &'static str>,
        guard: &impl Fn() -> Result<(), String>,
    ) -> (Attempt, Result<T, String>) {
        let started = Instant::now();
        let mut totals = Totals::default();
        let mut stop = None;
        let response = (|| loop {
            self.check(client, request.deadline, guard)?;
            let web = tools::Request {
                history: &self.history,
                offer: !self.final_only,
            };
            if request.system.chars().count()
                + request.input.chars().count()
                + web.context_chars()?
                > request.limit
            {
                return Err(Error::Review(
                    "The reviewed web-tool conversation exceeds the model context limit.".into(),
                ));
            }
            let send = |format| {
                client.complete_agent_turn_before_guarded(
                    request.system,
                    request.input,
                    Some(tools::Request {
                        history: &self.history,
                        offer: !self.final_only,
                    }),
                    format,
                    request.deadline,
                    || {
                        self.check(client, request.deadline, guard)
                            .map_err(|error| error.message())
                    },
                )
            };
            let (mut reply, mut observed_stop) = send(true);
            totals.observe(client.take_usage());
            if matches!(reply, Err(Error::UnsupportedResponseFormat)) {
                authorize_resend(
                    client,
                    request.progress,
                    request.deadline,
                    "The web-tool JSON compatibility retry was not authorized.",
                    |permit| Progress::FormatFallback(request.kind, permit),
                )?;
                (reply, observed_stop) = send(false);
                totals.observe(client.take_usage());
            }
            stop = observed_stop;
            let mut reply = reply?;
            self.check(client, request.deadline, guard)?;
            match &mut reply {
                tools::Reply::Answer(answer) => return Ok(std::mem::take(answer)),
                tools::Reply::Calls {
                    content,
                    calls,
                    omitted,
                } => {
                    let calls = std::mem::take(calls);
                    let assistant_context = content.clone().unwrap_or_default();
                    self.history.push(tools::Message::Assistant {
                        content: content.take(),
                        tool_calls: calls.clone(),
                    });
                    for (index, call) in calls.iter().enumerate() {
                        let result = {
                            let used = request.system.chars().count()
                                + request.input.chars().count()
                                + tools::Request {
                                    history: &self.history,
                                    offer: !self.final_only,
                                }
                                .context_chars()?;
                            let max_result_chars =
                                request.limit.saturating_sub(used + tools::MAX_CALLS * 256);
                            if max_result_chars < 256 {
                                return Err(Error::Review("The web-tool continuation cannot fit its reviewed context budget.".into()));
                            }
                            let (response, receiver) = async_channel::bounded(1);
                            request
                                .progress
                                .try_send(Progress::WebTool(Arc::new(WebProposal {
                                    call: call.clone(),
                                    round: self.rounds + 1,
                                    omitted: if index == 0 { *omitted } else { 0 },
                                    assistant_context: Zeroizing::new(assistant_context.clone()),
                                    max_result_chars,
                                    limit_reached: self.rounds >= MAX_EXECUTED_ROUNDS,
                                    response,
                                })))
                                .map_err(|_| {
                                    Error::Review(
                                        "The model web-tool review is unavailable.".into(),
                                    )
                                })?;
                            let mut approved =
                                self.await_result(client, &request, receiver, guard)?;
                            let encoded = serde_json::to_string(&tools::Message::Tool {
                                tool_call_id: call.id.clone(),
                                content: approved.content.to_string(),
                            })
                            .map_err(|_| Error::Service)?;
                            if approved.content.len() > MAX_RESULT_BYTES
                                || encoded.chars().count() > max_result_chars
                            {
                                return Err(Error::Review(
                                    "The reviewed web result exceeds its budget.".into(),
                                ));
                            }
                            (approved.guard)().map_err(Error::Review)?;
                            let content = std::mem::take(&mut *approved.content);
                            self.guards.push(approved.guard);
                            content
                        };
                        self.history.push(tools::Message::Tool {
                            tool_call_id: call.id.clone(),
                            content: result,
                        });
                    }
                    self.rounds += 1;
                    self.final_only = self.rounds > MAX_EXECUTED_ROUNDS;
                }
            }
        })();
        let (outcome, result) = interpret_reply(response, stop, request.output_limit, parse);
        (
            Attempt {
                kind: request.kind,
                outcome,
                elapsed: started.elapsed(),
                input_tokens: totals.input,
                output_tokens: totals.output,
                total_tokens: totals.total,
            },
            result,
        )
    }

    fn await_result(
        &self,
        client: &local_ai::Client,
        request: &Completion<'_>,
        receiver: Receiver<Result<ApprovedWebResult, String>>,
        guard: &impl Fn() -> Result<(), String>,
    ) -> Result<ApprovedWebResult, Error> {
        loop {
            self.check(client, request.deadline, guard)?;
            match receiver.try_recv() {
                Ok(result) => return result.map_err(Error::Review),
                Err(async_channel::TryRecvError::Closed) => {
                    return Err(Error::Review(
                        "The model web-tool review was closed.".into(),
                    ));
                }
                Err(async_channel::TryRecvError::Empty) => {
                    if !client.wait_until(
                        request
                            .deadline
                            .min(Instant::now() + Duration::from_millis(25)),
                        &|| false,
                    ) {
                        return Err(Error::Cancelled);
                    }
                }
            }
        }
    }
}

fn operation(call: &tools::Call) -> Option<(WebOperation, String)> {
    let value: serde_json::Value = serde_json::from_str(&call.function.arguments).ok()?;
    let (operation, input, limit) = match call.function.name.as_str() {
        "web_search" => (
            WebOperation::Search,
            value.get("query").or_else(|| value.get("url"))?.as_str()?,
            local_ai::web_search::MAX_QUERY_BYTES,
        ),
        "web_extract" => (
            WebOperation::Page,
            value.get("url")?.as_str()?,
            local_ai::web_search::MAX_PAGE_URL_BYTES,
        ),
        _ => return None,
    };
    (!input.trim().is_empty() && input.len() <= limit && !input.chars().any(char::is_control))
        .then(|| (operation, input.to_owned()))
}

pub(crate) fn error_result(kind: &str, message: &str) -> String {
    json!({"status":"error","provider":"kortty","errorType":kind,"message":message}).to_string()
}

#[cfg(test)]
mod tests;
