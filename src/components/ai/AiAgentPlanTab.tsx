import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Loader2, Play, Send, X } from "lucide-react";
import type {
  TerminalAgentPlanOption,
  TerminalAgentPlanOptionsEvent,
  TerminalAgentPlanQuestionsEvent,
  TerminalAgentPlanRequest,
  TerminalAgentPlanRunState,
} from "../../types/ai";

interface AiAgentPlanTabProps {
  tabId: string;
  initialRequest: TerminalAgentPlanRequest;
  initialRunId: string;
  initialState?: TerminalAgentPlanRunState;
  onTitleChange: (title: string) => void;
  onStartExecution: (runId: string) => Promise<void>;
}

function buildInitialPlanState(
  request: TerminalAgentPlanRequest,
  runId: string,
): TerminalAgentPlanRunState {
  return {
    runId,
    sessionId: request.sessionId,
    phase: "Starting",
    summary: "Starting planning run.",
    userMessage: request.userPrompt,
  };
}

function buildOptionBadge(option: TerminalAgentPlanOption, acceptedOptionId?: string) {
  return option.id === acceptedOptionId
    ? "border-kortty-accent bg-kortty-accent/10"
    : "border-kortty-border bg-kortty-panel/30";
}

export function AiAgentPlanTab({
  tabId,
  initialRequest,
  initialRunId,
  initialState,
  onTitleChange,
  onStartExecution,
}: AiAgentPlanTabProps) {
  const [planState, setPlanState] = useState<TerminalAgentPlanRunState>(
    initialState ?? buildInitialPlanState(initialRequest, initialRunId),
  );
  const [answers, setAnswers] = useState("");
  const [customApproach, setCustomApproach] = useState("");
  const [showCustomApproach, setShowCustomApproach] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [localError, setLocalError] = useState<string | null>(null);
  const runIdRef = useRef(initialRunId);

  const questions = planState.questions ?? [];
  const options = planState.options ?? [];
  const acceptedOptionId = planState.acceptedOptionId;
  const acceptedOption = useMemo(
    () => options.find((option) => option.id === acceptedOptionId) ?? null,
    [acceptedOptionId, options],
  );
  const executionStarted = !!planState.executionStartedRunId;
  const canAnswerQuestions = planState.phase === "AwaitingAnswers";
  const canSelectOption = planState.phase === "AwaitingSelection" || planState.phase === "ReadyToExecute";
  const canStartExecution = !!acceptedOption && !executionStarted;

  useEffect(() => {
    onTitleChange("AI Agent Plan");
  }, [onTitleChange]);

  useEffect(() => {
    runIdRef.current = initialRunId;
    setPlanState(initialState ?? buildInitialPlanState(initialRequest, initialRunId));
  }, [initialRequest, initialRunId, initialState]);

  useEffect(() => {
    let offStatus: (() => void) | null = null;
    let offQuestions: (() => void) | null = null;
    let offOptions: (() => void) | null = null;

    void listen<TerminalAgentPlanRunState>("terminal-agent-plan-status", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }
      setPlanState(event.payload);
      setLocalError(null);
    }).then((fn) => {
      offStatus = fn;
    }).catch(console.error);

    void listen<TerminalAgentPlanQuestionsEvent>("terminal-agent-plan-questions", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }
      setPlanState((current) => ({
        ...current,
        runId: event.payload.runId,
        sessionId: event.payload.sessionId,
        questions: event.payload.questions,
      }));
    }).then((fn) => {
      offQuestions = fn;
    }).catch(console.error);

    void listen<TerminalAgentPlanOptionsEvent>("terminal-agent-plan-options", (event) => {
      if (event.payload.runId !== runIdRef.current) {
        return;
      }
      setPlanState((current) => ({
        ...current,
        runId: event.payload.runId,
        sessionId: event.payload.sessionId,
        options: event.payload.options,
        acceptedOptionId: event.payload.acceptedOptionId,
      }));
    }).then((fn) => {
      offOptions = fn;
    }).catch(console.error);

    return () => {
      offStatus?.();
      offQuestions?.();
      offOptions?.();
    };
  }, [tabId]);

  const handleSubmitAnswers = useCallback(async () => {
    if (!answers.trim()) {
      return;
    }
    setSubmitting(true);
    setLocalError(null);
    try {
      await invoke("answer_terminal_agent_plan_questions", {
        runId: planState.runId,
        answers: answers.trim(),
      });
      setAnswers("");
    } catch (error) {
      setLocalError(`Failed to submit answers: ${String(error)}`);
    } finally {
      setSubmitting(false);
    }
  }, [answers, planState.runId]);

  const handleSubmitCustomApproach = useCallback(async () => {
    if (!customApproach.trim()) {
      return;
    }
    setSubmitting(true);
    setLocalError(null);
    try {
      await invoke("submit_terminal_agent_plan_custom_approach", {
        runId: planState.runId,
        customApproach: customApproach.trim(),
      });
      setCustomApproach("");
      setShowCustomApproach(false);
    } catch (error) {
      setLocalError(`Failed to refine the plan: ${String(error)}`);
    } finally {
      setSubmitting(false);
    }
  }, [customApproach, planState.runId]);

  const handleAcceptOption = useCallback(async (optionId: string) => {
    setSubmitting(true);
    setLocalError(null);
    try {
      await invoke("choose_terminal_agent_plan_option", {
        runId: planState.runId,
        optionId,
      });
    } catch (error) {
      setLocalError(`Failed to accept option: ${String(error)}`);
    } finally {
      setSubmitting(false);
    }
  }, [planState.runId]);

  const handleCancel = useCallback(async () => {
    setSubmitting(true);
    setLocalError(null);
    try {
      await invoke("cancel_terminal_agent_plan", { runId: planState.runId });
    } catch (error) {
      setLocalError(`Failed to cancel planning run: ${String(error)}`);
    } finally {
      setSubmitting(false);
    }
  }, [planState.runId]);

  const handleStartAcceptedPlan = useCallback(async () => {
    setSubmitting(true);
    setLocalError(null);
    try {
      await onStartExecution(planState.runId);
    } catch (error) {
      setLocalError(`Failed to start execution from the accepted plan: ${String(error)}`);
    } finally {
      setSubmitting(false);
    }
  }, [onStartExecution, planState.runId]);

  return (
    <div
      className="flex h-full min-h-0 flex-col bg-kortty-terminal text-kortty-text"
      onContextMenuCapture={(event) => {
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      <div className="border-b border-kortty-border bg-kortty-surface px-4 py-3">
        <div className="flex items-start justify-between gap-3">
          <div>
            <div className="text-sm font-semibold">AI Agent Plan</div>
            <div className="mt-1 text-xs text-kortty-text-dim">
              {initialRequest.connectionDisplayName || initialRequest.sessionId}
            </div>
          </div>
          <div className="text-right text-xs text-kortty-text-dim">
            <div>{planState.phase}</div>
            {submitting && (
              <div className="mt-1 inline-flex items-center gap-1">
                <Loader2 className="h-3 w-3 animate-spin" />
                Working
              </div>
            )}
          </div>
        </div>
        <div className="mt-3 rounded border border-kortty-border bg-kortty-panel/40 px-3 py-2 text-sm">
          {initialRequest.userPrompt}
        </div>
        {planState.probeSummary && (
          <div className="mt-3 rounded border border-kortty-border bg-kortty-panel/20 px-3 py-2 text-xs text-kortty-text-dim">
            Server summary: {planState.probeSummary}
          </div>
        )}
        <div className="mt-3 text-sm">{planState.userMessage || planState.summary}</div>
        {localError && (
          <div className="mt-3 rounded border border-kortty-error/40 bg-kortty-error/10 px-3 py-2 text-xs text-kortty-error">
            {localError}
          </div>
        )}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 space-y-4">
        <section className="space-y-2">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-kortty-text-dim">Clarifying questions</h3>
          {questions.length === 0 ? (
            <div className="rounded border border-kortty-border bg-kortty-panel/20 px-3 py-2 text-sm text-kortty-text-dim">
              The planner has not produced questions yet.
            </div>
          ) : (
            <div className="space-y-2">
              {questions.map((question, index) => (
                <div key={question.id} className="rounded border border-kortty-border bg-kortty-panel/30 px-3 py-2 text-sm">
                  <div className="text-xs text-kortty-text-dim">Question {index + 1}</div>
                  <div className="mt-1">{question.question}</div>
                </div>
              ))}
            </div>
          )}
          <div className="rounded border border-kortty-border bg-kortty-panel/20 p-3">
            <label className="block text-xs text-kortty-text-dim mb-1">Your answers</label>
            <textarea
              className="input-field min-h-24 resize-y"
              value={answers}
              onChange={(event) => setAnswers(event.target.value)}
              disabled={!canAnswerQuestions || submitting}
              placeholder="Answer the questions for the planner here."
            />
            <div className="mt-3 flex justify-end">
              <button
                className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                disabled={!canAnswerQuestions || !answers.trim() || submitting}
                onClick={() => void handleSubmitAnswers()}
              >
                <span className="inline-flex items-center gap-2">
                  <Send className="h-3.5 w-3.5" />
                  Submit answers
                </span>
              </button>
            </div>
          </div>
        </section>

        <section className="space-y-2">
          <div className="flex items-center justify-between gap-2">
            <h3 className="text-xs font-semibold uppercase tracking-wide text-kortty-text-dim">Implementation options</h3>
            <button
              className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors disabled:opacity-50"
              disabled={submitting || questions.length === 0}
              onClick={() => setShowCustomApproach((current) => !current)}
            >
              Use my own approach
            </button>
          </div>
          {showCustomApproach && (
            <div className="rounded border border-kortty-border bg-kortty-panel/20 p-3">
              <label className="block text-xs text-kortty-text-dim mb-1">Your preferred approach</label>
              <textarea
                className="input-field min-h-24 resize-y"
                value={customApproach}
                onChange={(event) => setCustomApproach(event.target.value)}
                disabled={submitting}
                placeholder="Describe how the planner should adapt the solution."
              />
              <div className="mt-3 flex justify-end">
                <button
                  className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                  disabled={!customApproach.trim() || submitting}
                  onClick={() => void handleSubmitCustomApproach()}
                >
                  Refine options
                </button>
              </div>
            </div>
          )}
          {options.length === 0 ? (
            <div className="rounded border border-kortty-border bg-kortty-panel/20 px-3 py-2 text-sm text-kortty-text-dim">
              No implementation options are available yet.
            </div>
          ) : (
            <div className="space-y-3">
              {options.map((option, index) => (
                <div key={option.id} className={`rounded border px-3 py-3 ${buildOptionBadge(option, acceptedOptionId)}`}>
                  <div className="flex items-start justify-between gap-3">
                    <div>
                      <div className="text-xs text-kortty-text-dim">Option {index + 1}</div>
                      <div className="mt-1 text-sm font-semibold">{option.title}</div>
                    </div>
                    <button
                      className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                      disabled={!canSelectOption || submitting}
                      onClick={() => void handleAcceptOption(option.id)}
                    >
                      {option.id === acceptedOptionId ? "Accepted" : "Accept option"}
                    </button>
                  </div>
                  <div className="mt-2 text-sm">{option.summary}</div>
                  <div className="mt-3 grid gap-3 md:grid-cols-2">
                    <div>
                      <div className="text-xs font-semibold text-kortty-text-dim">Feasibility</div>
                      <div className="mt-1 text-sm">{option.feasibility}</div>
                    </div>
                    <div>
                      <div className="text-xs font-semibold text-kortty-text-dim">Prerequisites</div>
                      <ul className="mt-1 space-y-1 text-sm">
                        {(option.prerequisites.length > 0 ? option.prerequisites : ["No explicit prerequisites"]).map((entry, entryIndex) => (
                          <li key={`${option.id}-prerequisite-${entryIndex}`}>• {entry}</li>
                        ))}
                      </ul>
                    </div>
                    <div>
                      <div className="text-xs font-semibold text-kortty-text-dim">Risks</div>
                      <ul className="mt-1 space-y-1 text-sm">
                        {(option.risks.length > 0 ? option.risks : ["No explicit risks"]).map((entry, entryIndex) => (
                          <li key={`${option.id}-risk-${entryIndex}`}>• {entry}</li>
                        ))}
                      </ul>
                    </div>
                    <div>
                      <div className="text-xs font-semibold text-kortty-text-dim">Alternatives</div>
                      <ul className="mt-1 space-y-1 text-sm">
                        {(option.alternatives.length > 0 ? option.alternatives : ["No explicit alternatives"]).map((entry, entryIndex) => (
                          <li key={`${option.id}-alternative-${entryIndex}`}>• {entry}</li>
                        ))}
                      </ul>
                    </div>
                  </div>
                  <div className="mt-3">
                    <div className="text-xs font-semibold text-kortty-text-dim">Steps</div>
                    <ol className="mt-1 space-y-1 text-sm">
                      {option.steps.map((step, stepIndex) => (
                        <li key={`${option.id}-step-${stepIndex}`}>{stepIndex + 1}. {step}</li>
                      ))}
                    </ol>
                  </div>
                </div>
              ))}
            </div>
          )}
        </section>
      </div>

      <div className="border-t border-kortty-border bg-kortty-surface px-4 py-3 flex items-center justify-between gap-3">
        <button
          className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text hover:bg-kortty-border transition-colors disabled:opacity-50"
          disabled={submitting || ["Cancelled", "Done", "Failed", "Blocked"].includes(planState.phase)}
          onClick={() => void handleCancel()}
        >
          <span className="inline-flex items-center gap-2">
            <X className="h-3.5 w-3.5" />
            Abort
          </span>
        </button>
        <div className="text-right">
          {executionStarted && (
            <div className="mb-2 text-xs text-kortty-text-dim">
              Execution started with run {planState.executionStartedRunId}
            </div>
          )}
          <button
            className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
            disabled={!canStartExecution || submitting}
            onClick={() => void handleStartAcceptedPlan()}
          >
            <span className="inline-flex items-center gap-2">
              <Play className="h-3.5 w-3.5" />
              Start accepted plan
            </span>
          </button>
        </div>
      </div>
    </div>
  );
}
