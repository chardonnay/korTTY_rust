export type AiAction = "Summarize" | "SolveProblem" | "Ask" | "GenerateChatTitle";
export type TerminalAgentExecutionTarget = "TerminalWindow" | "ChatWindow";
export type TerminalAgentPhase =
  | "Starting"
  | "Probing"
  | "Planning"
  | "AwaitingApproval"
  | "AwaitingPassword"
  | "RunningCommands"
  | "Done"
  | "Blocked"
  | "Cancelled"
  | "Failed";
export type TerminalAgentPlanPhase =
  | "Starting"
  | "Probing"
  | "Questioning"
  | "AwaitingAnswers"
  | "GeneratingOptions"
  | "AwaitingSelection"
  | "ReadyToExecute"
  | "Done"
  | "Blocked"
  | "Cancelled"
  | "Failed";
export type TerminalAgentRisk = "read_only" | "requires_confirmation";
export type TerminalAgentEventKind =
  | "command_started"
  | "stdout"
  | "stderr"
  | "command_finished";

export type AiTokenizerType =
  | "Estimate"
  | "Cl100kBase"
  | "O200kBase"
  | "P50kBase"
  | "R50kBase";

export type AiTokenLimitUnit = "Thousands" | "Millions";
export type AiTokenWarningLevel = "None" | "Yellow" | "Red";
export type AiChatRole = "User" | "Assistant";

export interface AiProfile {
  id: string;
  name: string;
  apiUrl: string;
  model: string;
  apiKey: string;
  maxSelectionChars: number;
  tokenizerType: AiTokenizerType;
  tokenLimitAmount?: number;
  tokenLimitUnit: AiTokenLimitUnit;
  tokenWarningYellowPercent: number;
  tokenWarningRedPercent: number;
  tokenResetPeriodDays: number;
  tokenResetAnchorDate?: string;
  tokenUsageCycleStartDate?: string;
  usedPromptTokens: number;
  usedCompletionTokens: number;
  usedTotalTokens: number;
}

export interface AiTokenUsage {
  promptTokens: number;
  completionTokens: number;
  totalTokens: number;
}

export interface AiTokenUsageSnapshot {
  usedPromptTokens: number;
  usedCompletionTokens: number;
  usedTotalTokens: number;
  maxTokens: number;
  remainingTokens?: number;
  cycleStartDate: string;
  nextResetDate: string;
  warningLevel: AiTokenWarningLevel;
  unlimited: boolean;
}

export interface AiExecutionResult {
  content: string;
  usage?: AiTokenUsage;
  usageSnapshot?: AiTokenUsageSnapshot;
  activeProfileId?: string;
  activeProfileName?: string;
}

export interface TerminalAgentRequest {
  sessionId: string;
  profileId: string;
  userPrompt: string;
  connectionDisplayName?: string;
  acceptedPlanContext?: string;
  executionTarget: TerminalAgentExecutionTarget;
  showDebugMessages: boolean;
  showRuntimeMessages: boolean;
  askConfirmationBeforeEveryCommand: boolean;
  autoApproveRootCommands: boolean;
}

export interface TerminalAgentPlanRequest {
  sessionId: string;
  profileId: string;
  userPrompt: string;
  connectionDisplayName?: string;
}

export interface TerminalAgentPlanStartResponse {
  runId: string;
  initialState: TerminalAgentPlanRunState;
}

export interface TerminalAgentStartResponse {
  runId: string;
}

export interface TerminalAgentPlanExecutionResponse {
  runId: string;
  request: TerminalAgentRequest;
}

export interface TerminalAgentPlannedCommand {
  command: string;
  purpose: string;
  risk: TerminalAgentRisk;
}

export interface TerminalAgentApproval {
  runId: string;
  sessionId: string;
  executionTarget: TerminalAgentExecutionTarget;
  summary: string;
  userMessage: string;
  commands: TerminalAgentPlannedCommand[];
}

export interface TerminalAgentPasswordRequest {
  runId: string;
  sessionId: string;
  executionTarget: TerminalAgentExecutionTarget;
  summary: string;
  userMessage: string;
  command: string;
}

export interface TerminalAgentProbeSnapshot {
  osRelease: string;
  kernel: string;
  architecture: string;
  shell: string;
  currentUser: string;
  uid: string;
  gid: string;
  groups: string[];
  homeDir: string;
  currentDir: string;
  availableDiskKb?: number;
  availableDiskPath: string;
  packageManagers: string[];
  serviceManagers: string[];
  alreadyRoot: boolean;
  sudoAvailable: boolean;
  passwordlessSudo: boolean;
  sudoNonInteractive: boolean;
  sudoNListSummary: string;
  rootEscalationMode: string;
}

export interface TerminalAgentPlanQuestion {
  id: string;
  question: string;
}

export interface TerminalAgentPlanOption {
  id: string;
  title: string;
  summary: string;
  feasibility: string;
  risks: string[];
  prerequisites: string[];
  steps: string[];
  alternatives: string[];
}

export interface TerminalAgentPlanQuestionsEvent {
  runId: string;
  sessionId: string;
  questions: TerminalAgentPlanQuestion[];
}

export interface TerminalAgentPlanOptionsEvent {
  runId: string;
  sessionId: string;
  options: TerminalAgentPlanOption[];
  acceptedOptionId?: string;
}

export interface TerminalAgentPlanRunState {
  runId: string;
  sessionId: string;
  phase: TerminalAgentPlanPhase;
  summary: string;
  userMessage?: string;
  probeSummary?: string;
  questions?: TerminalAgentPlanQuestion[];
  options?: TerminalAgentPlanOption[];
  acceptedOptionId?: string;
  executionStartedRunId?: string;
}

export interface TerminalAgentCommandResult {
  command: string;
  purpose: string;
  risk: TerminalAgentRisk;
  exitStatus?: number;
  exitSignal?: string;
  stdoutTail: string;
  stderrTail: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  cancelled: boolean;
  timedOut: boolean;
}

export interface TerminalAgentRunState {
  runId: string;
  sessionId: string;
  executionTarget: TerminalAgentExecutionTarget;
  phase: TerminalAgentPhase;
  summary: string;
  userMessage?: string;
  pendingApproval?: TerminalAgentApproval;
  pendingPasswordRequest?: TerminalAgentPasswordRequest;
  currentCommand?: string;
  turn: number;
}

export interface TerminalAgentEvent {
  runId: string;
  sessionId: string;
  executionTarget: TerminalAgentExecutionTarget;
  kind: TerminalAgentEventKind;
  command?: string;
  purpose?: string;
  chunk?: string;
  result?: TerminalAgentCommandResult;
}

export interface AiRequestPayload {
  action: AiAction;
  profileId: string;
  selectedText: string;
  connectionDisplayName?: string;
  responseLanguageCode?: string;
  userPrompt?: string;
  conversationContext?: string;
}

export interface SavedAiChatMessage {
  role: AiChatRole;
  content: string;
  createdAt: number;
  aiProfileId?: string;
  aiProfileName?: string;
}

export interface SavedAiChat {
  id: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  selectedText: string;
  connectionDisplayName?: string;
  responseLanguageCode?: string;
  activeAiProfileId?: string;
  activeAiProfileName?: string;
  messages: SavedAiChatMessage[];
}

export function createEmptyAiProfile(): AiProfile {
  return {
    id: crypto.randomUUID(),
    name: "",
    apiUrl: "",
    model: "",
    apiKey: "",
    maxSelectionChars: 1_000_000,
    tokenizerType: "Estimate",
    tokenLimitUnit: "Thousands",
    tokenWarningYellowPercent: 75,
    tokenWarningRedPercent: 90,
    tokenResetPeriodDays: 30,
    usedPromptTokens: 0,
    usedCompletionTokens: 0,
    usedTotalTokens: 0,
  };
}
