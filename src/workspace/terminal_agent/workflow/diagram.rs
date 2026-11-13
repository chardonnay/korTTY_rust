//! Reviews and renders a flowchart of one edited workflow draft.

use iced::widget::{button, column, row, scrollable, text, text_input};
use iced::{Element, Font, Length, Size};

use crate::clipboard_policy::ClipboardPolicy;
use crate::diagram_window::{AnalysisFlowchartRequest, Chart, DiagramWindow};
use crate::snippets::diagrams::{Saved, Scope as SourceScope, Source};

use super::{
    Arc, Authority, Desktop, Dialog, Language, MAX_DRAFT, MAX_INSTRUCTIONS, Model, Policy,
    Redactor, RunMessage, Stage, Task, Workspace, dialog_placement, draft_source, local_ai, mask,
    window,
};

#[cfg(test)]
mod tests;

const SIZE: Size = Size::new(1050.0, 660.0);

#[derive(Clone)]
pub enum Message {
    Open,
    Regenerate,
    Instructions(u64, String),
    Generate(u64),
    Local(u64),
    Reply(u64, Result<Chart, String>),
    Cancel,
}

#[derive(Default)]
pub(in crate::workspace::terminal_agent) struct State {
    phase: Option<Phase>,
    preview: Option<window::Id>,
}

enum Phase {
    Review(Box<Review>),
    Running(Box<Pending>),
}

struct Scope {
    ticket: u64,
    window: window::Id,
    owner: window::Id,
    run: window::Id,
    run_epoch: u64,
    generation: u64,
    result: usize,
    language: Language,
    source: Source,
    model: Model,
    policy: Policy,
}

struct Review {
    scope: Scope,
    edit_ticket: u64,
    destination: String,
    instructions: String,
    prepared: Result<Prepared, String>,
}

struct Prepared {
    instructions: String,
    request: Arc<AnalysisFlowchartRequest>,
    within_limit: bool,
}

struct Pending {
    scope: Scope,
    instructions: String,
    _cancel: Option<local_ai::Cancel>,
}

impl State {
    pub(in crate::workspace::terminal_agent) fn running(&self) -> bool {
        matches!(self.phase, Some(Phase::Running(_)))
    }

    pub(in crate::workspace::terminal_agent) fn cancel(&mut self) {
        self.phase = None;
    }

    pub(super) fn view(&self) -> Option<Element<'_, Message>> {
        let content = match self.phase.as_ref()? {
            Phase::Review(review) => {
                let ticket = review.scope.ticket;
                let edit_ticket = review.edit_ticket;
                let mut form = column![
                    text("Review workflow flowchart").size(20),
                    text(&review.destination).size(13),
                    text_input("Optional flowchart instructions", &review.instructions)
                        .on_input(move |value| Message::Instructions(edit_ticket, value)),
                ]
                .spacing(8);
                match &review.prepared {
                    Ok(prepared) => {
                        form = form.push(
                            scrollable(
                                column![
                                    text("System instructions").size(14),
                                    prompt_text(prepared.request.system()),
                                    text("Request body").size(14),
                                    prompt_text(prepared.request.input()),
                                ]
                                .spacing(6)
                                .padding([4, 10]),
                            )
                            .height(Length::Fill),
                        );
                        if !prepared.within_limit {
                            form = form.push(text("This request exceeds the model's context limit. Shorten the instructions or use the local flowchart.").size(13));
                        }
                    }
                    Err(error) => form = form.push(text(error).size(13)),
                }
                form.push(
                    row![
                        button("Generate reviewed flowchart").on_press_maybe(
                            review
                                .prepared
                                .as_ref()
                                .is_ok_and(|input| input.within_limit)
                                .then_some(Message::Generate(ticket)),
                        ),
                        button("Render locally").on_press_maybe(
                            review.prepared.is_ok().then_some(Message::Local(ticket)),
                        ),
                        button("Back").on_press(Message::Cancel),
                    ]
                    .spacing(6)
                    .wrap(),
                )
                .height(Length::Fill)
            }
            Phase::Running(_) => column![
                text("Generating workflow flowchart…").size(20),
                button("Cancel request").on_press(Message::Cancel),
            ]
            .spacing(8)
            .height(Length::Fill),
        };
        Some(content.into())
    }
}

fn prompt_text(value: &str) -> text::Text<'_> {
    text(value)
        .font(Font::MONOSPACE)
        .size(13)
        .wrapping(text::Wrapping::WordOrGlyph)
}

impl Scope {
    fn current<'a>(
        &self,
        workspace: &'a Workspace,
        id: window::Id,
        dialog: &Dialog,
    ) -> Result<Authority<'a>, String> {
        let stale = || "The workflow draft or its AI settings changed. Review it again.".to_owned();
        if dialog.private
            || id != self.window
            || dialog.run != self.run
            || dialog.run_epoch != self.run_epoch
            || dialog.generation != self.generation
        {
            return Err(stale());
        }
        let Stage::Results(results) = &dialog.stage else {
            return Err(stale());
        };
        let item = results
            .items
            .get(self.result)
            .filter(|item| results.selected == self.result && item.language == self.language)
            .ok_or_else(stale)?;
        let content = item.content.as_ref().ok_or_else(stale)?;
        if !self.source.matches(&draft_source(content)) {
            return Err(stale());
        }
        let authority = workspace.workflow_authority(self.run, self.run_epoch)?;
        if authority.owner != self.owner
            || authority.model.transport != self.model.transport
            || authority.model.limit != self.model.limit
            || authority.model.name != self.model.name
            || authority.model.destination != self.model.destination
            || !authority.redactor.uses_redactions(&self.policy)
        {
            return Err(stale());
        }
        check_source(&authority.redactor, &self.source.text)?;
        Ok(authority)
    }

    fn prepare(&self, raw: &str, redactor: &Redactor) -> Result<Prepared, String> {
        if raw.len() > MAX_INSTRUCTIONS {
            return Err("Use at most 4 KiB of flowchart instructions.".into());
        }
        let instructions = mask::prompt(redactor, raw.trim())?;
        let request = AnalysisFlowchartRequest::for_workflow(
            Arc::clone(&self.source.text),
            self.language.snippet_language(),
            &instructions,
        )?;
        let within_limit =
            request.system().chars().count() + request.input().chars().count() <= self.model.limit;
        Ok(Prepared {
            instructions,
            request: Arc::new(request),
            within_limit,
        })
    }
}

impl Workspace {
    pub(super) fn workflow_diagram_action(
        &mut self,
        id: window::Id,
        dialog: &mut Dialog,
        message: Message,
    ) -> Result<Task<Desktop>, String> {
        if dialog.private {
            return Ok(Task::none());
        }
        match message {
            Message::Open | Message::Regenerate if dialog.diagram.phase.is_none() => {
                self.review_workflow_diagram(id, dialog, matches!(message, Message::Regenerate))
            }
            Message::Instructions(ticket, value) => {
                self.edit_workflow_diagram_instructions(id, dialog, ticket, value)
            }
            Message::Generate(ticket) | Message::Local(ticket) => self.start_workflow_diagram(
                id,
                dialog,
                ticket,
                matches!(message, Message::Local(_)),
            ),
            Message::Reply(ticket, result) => {
                self.finish_workflow_diagram(id, dialog, ticket, result)
            }
            Message::Cancel => {
                dialog.diagram.cancel();
                dialog.notice.clear();
                Ok(Task::none())
            }
            _ => Ok(Task::none()),
        }
    }

    fn review_workflow_diagram(
        &mut self,
        id: window::Id,
        dialog: &mut Dialog,
        regenerate: bool,
    ) -> Result<Task<Desktop>, String> {
        let Stage::Results(results) = &dialog.stage else {
            return Ok(Task::none());
        };
        let Some(item) = results.items.get(results.selected) else {
            return Ok(Task::none());
        };
        let Some(content) = &item.content else {
            return Ok(Task::none());
        };
        let code = draft_source(content);
        let ticket = self.allocate();
        let authority = self.workflow_authority(dialog.run, dialog.run_epoch)?;
        check_source(&authority.redactor, &code)?;
        if !regenerate
            && let Some(saved) = item
                .diagram
                .as_ref()
                .filter(|saved| saved.is_current(&code))
        {
            let (chart, _) = saved.restore(&code)?;
            check_chart(&authority.redactor, &chart)?;
            dialog.notice.clear();
            return Ok(self.open_workflow_diagram_preview(id, dialog, &code, chart));
        }
        let instructions = item
            .diagram
            .as_ref()
            .map_or_else(String::new, |saved| saved.instructions.clone());
        let destination = mask::prompt(
            &authority.redactor,
            &format!("{} · {}", authority.model.name, authority.model.destination),
        )?;
        let scope = Scope {
            ticket,
            window: id,
            owner: authority.owner,
            run: dialog.run,
            run_epoch: dialog.run_epoch,
            generation: dialog.generation,
            result: results.selected,
            language: item.language,
            source: Source::capture(&code, SourceScope::Whole)?,
            model: authority.model,
            policy: authority.redactor.redaction_policy().clone(),
        };
        let prepared = scope.prepare(&instructions, &authority.redactor);
        dialog.diagram.phase = Some(Phase::Review(Box::new(Review {
            scope,
            edit_ticket: ticket,
            destination,
            instructions,
            prepared,
        })));
        dialog.notice.clear();
        Ok(Task::none())
    }

    fn edit_workflow_diagram_instructions(
        &mut self,
        id: window::Id,
        dialog: &mut Dialog,
        ticket: u64,
        value: String,
    ) -> Result<Task<Desktop>, String> {
        if !matches!(&dialog.diagram.phase, Some(Phase::Review(review)) if review.edit_ticket == ticket)
        {
            return Ok(Task::none());
        }
        let Some(Phase::Review(mut review)) = dialog.diagram.phase.take() else {
            unreachable!("matching flowchart review");
        };
        let next_ticket = self.allocate();
        let authority = review.scope.current(self, id, dialog)?;
        review.prepared = review.scope.prepare(&value, &authority.redactor);
        if value.len() <= MAX_INSTRUCTIONS {
            review.instructions = value;
        }
        review.scope.ticket = next_ticket;
        dialog.diagram.phase = Some(Phase::Review(review));
        dialog.notice.clear();
        Ok(Task::none())
    }

    fn start_workflow_diagram(
        &mut self,
        id: window::Id,
        dialog: &mut Dialog,
        ticket: u64,
        local: bool,
    ) -> Result<Task<Desktop>, String> {
        let Some(Phase::Review(review)) = &dialog.diagram.phase else {
            return Ok(Task::none());
        };
        if review.scope.ticket != ticket {
            return Ok(Task::none());
        }
        let authority = match review.scope.current(self, id, dialog) {
            Ok(authority) => authority,
            Err(error) => {
                dialog.diagram.cancel();
                return Err(error);
            }
        };
        let prepared = review.prepared.as_ref().map_err(Clone::clone)?;
        if mask::prompt(&authority.redactor, review.instructions.trim())? != prepared.instructions {
            dialog.diagram.cancel();
            return Err("The flowchart redactions changed. Review the request again.".into());
        }
        if !local && !prepared.within_limit {
            return Err("The reviewed flowchart request exceeds the model's context limit.".into());
        }
        let request = Arc::clone(&prepared.request);
        let instructions = prepared.instructions.clone();
        let (task, cancel) = if local {
            (
                Task::perform(async move { request.local_workflow() }, move |result| {
                    response(id, ticket, result)
                }),
                None,
            )
        } else {
            let redactor = authority.redactor;
            let local_ai::Pending { reply, cancel } = local_ai::start(
                review.scope.model.transport.clone(),
                move |client| {
                    request.complete_reviewed_guarded(
                        client,
                        || {
                            let policy = ClipboardPolicy::load();
                            (policy.ai_agent_allowed()
                                && redactor.uses_redactions(&policy.terminal_logging))
                                .then_some(())
                                .ok_or_else(|| "Managed policy changed before sending the workflow flowchart request.".into())
                        },
                        |mermaid| mask::prompt(&redactor, mermaid),
                    )
                },
            )?;
            let task = Task::perform(
                async move {
                    reply
                        .recv()
                        .await
                        .unwrap_or_else(|_| Err("The flowchart worker stopped.".into()))
                },
                move |result| response(id, ticket, result),
            );
            (task, Some(cancel))
        };
        let Some(Phase::Review(review)) = dialog.diagram.phase.take() else {
            unreachable!("flowchart worker owns the reviewed request");
        };
        dialog.diagram.phase = Some(Phase::Running(Box::new(Pending {
            scope: review.scope,
            instructions,
            _cancel: cancel,
        })));
        dialog.notice.clear();
        Ok(task)
    }

    fn finish_workflow_diagram(
        &mut self,
        id: window::Id,
        dialog: &mut Dialog,
        ticket: u64,
        result: Result<Chart, String>,
    ) -> Result<Task<Desktop>, String> {
        if !matches!(&dialog.diagram.phase, Some(Phase::Running(pending)) if pending.scope.ticket == ticket)
        {
            return Ok(Task::none());
        }
        let Some(Phase::Running(pending)) = dialog.diagram.phase.take() else {
            unreachable!("matching flowchart request");
        };
        let scope = &pending.scope;
        let authority = scope.current(self, id, dialog)?;
        let chart = result?;
        check_chart(&authority.redactor, &chart)?;
        if mask::prompt(&authority.redactor, &pending.instructions)? != pending.instructions {
            return Err("The flowchart instructions require masking. Review them again.".into());
        }
        let Stage::Results(results) = &mut dialog.stage else {
            unreachable!("flowchart source was checked");
        };
        let item = &mut results.items[scope.result];
        item.diagram = Some(Saved::capture(
            &chart,
            &scope.source,
            "Workflow flowchart",
            &pending.instructions,
            item.diagram.as_ref(),
        )?);
        dialog.notice = if chart.is_fallback() {
            "Opened the locally generated flowchart.".into()
        } else {
            "Workflow flowchart opened.".into()
        };
        Ok(self.open_workflow_diagram_preview(id, dialog, &scope.source.text, chart))
    }

    fn open_workflow_diagram_preview(
        &mut self,
        owner: window::Id,
        dialog: &mut Dialog,
        source: &str,
        chart: Chart,
    ) -> Task<Desktop> {
        if let Some(id) = dialog.diagram.preview
            && let Some(preview) = self.diagrams.get_mut(&id)
        {
            preview.replace_saved_chart(Some(source), chart);
            return window::minimize(id, false).chain(window::gain_focus(id));
        }
        let (id, opened) = self.open_modeless_placed_dialog(
            owner,
            dialog_placement::Kind::Diagram,
            window::Settings {
                size: SIZE,
                min_size: Some(Size::new(600.0, 320.0)),
                exit_on_close_request: false,
                ..window::Settings::default()
            },
        );
        self.diagrams
            .insert(id, DiagramWindow::with_workflow_chart(SIZE, source, chart));
        dialog.diagram.preview = Some(id);
        opened
    }

    pub(in crate::workspace::terminal_agent) fn close_workflow_diagram_preview(
        &mut self,
        dialog: &mut Dialog,
    ) -> Task<Desktop> {
        dialog
            .diagram
            .preview
            .take()
            .filter(|id| self.diagrams.remove(id).is_some())
            .map_or_else(Task::none, |id| self.close_placed_dialog(id))
    }
}

fn check_source(redactor: &Redactor, source: &str) -> Result<(), String> {
    if source.trim().is_empty()
        || source.len() > MAX_DRAFT
        || mask::prompt(redactor, source)? != source
    {
        return Err("Review the edited workflow for sensitive text or invalid source.".into());
    }
    Ok(())
}

pub(super) fn check_chart(redactor: &Redactor, chart: &Chart) -> Result<(), String> {
    let mermaid = chart.diagram().to_mermaid();
    if mask::prompt(redactor, &mermaid)? != mermaid {
        return Err("The workflow flowchart contains text that requires masking.".into());
    }
    if let Some(reason) = chart.fallback_reason()
        && mask::prompt(redactor, reason)? != reason
    {
        return Err("The workflow flowchart contains text that requires masking.".into());
    }
    Ok(())
}

fn response(id: window::Id, ticket: u64, result: Result<Chart, String>) -> Desktop {
    Desktop::TerminalAgent(
        id,
        RunMessage::Workflow(super::Message::Flowchart(Message::Reply(ticket, result))),
    )
}
