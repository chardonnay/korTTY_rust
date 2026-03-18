export type AiAction = "Summarize" | "SolveProblem" | "Ask" | "GenerateChatTitle";

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
