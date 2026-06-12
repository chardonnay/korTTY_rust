import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { AiSkill, TerminalAgentExecutionTarget } from "../types/ai";
import type { SnippetEditorProfile } from "../types/snippet";
import type {
  TerminalRecordingFormat,
  TerminalRecordingScope,
} from "../types/terminalRecording";

export type TeamworkSourceType = "Git" | "SharedFile";
export type TerminalAgentPanelDock = "bottom" | "left" | "right";
export type LocalFileBrowserDock = "left" | "right" | "bottom";
export type AppDesign =
  | "normal"
  | "matrix-terminal"
  | "holographic-interface"
  | "klingon-tactical"
  | "elegant-dark";

export interface TeamworkSourceConfig {
  id: string;
  sourceType: TeamworkSourceType;
  location: string;
  checkIntervalMinutes: number;
  readOnly: boolean;
  enabled: boolean;
}

export interface GlobalSettings {
  language: string;
  autoDetectLanguage: boolean;
  defaultFontFamily: string;
  defaultFontSize: number;
  defaultColumns: number;
  defaultRows: number;
  defaultScrollbackLines: number;
  defaultSshKeepaliveEnabled: boolean;
  defaultSshKeepaliveInterval: number;
  defaultConnectionTimeout: number;
  defaultRetryCount: number;
  showMenuBar: boolean;
  storeWindowGeometry: boolean;
  storeDashboardState: boolean;
  backupEncryptionType: "Password" | "GPG";
  backupCredentialId?: string;
  backupGpgKeyId?: string;
  maxBackups: number;
  translationProvider?: "Google" | "DeepL" | "LibreTranslate" | "Microsoft" | "Yandex";
  translationApiKey?: string;
  translationApiUrl?: string;
  teamworkSources: TeamworkSourceConfig[];
  teamworkDefaultCheckIntervalMinutes: number;
  teamworkDefaultCredentialId?: string;
  teamworkDefaultSshKeyId?: string;
  teamworkDefaultUsername?: string;
  teamworkUseTemporaryKey: boolean;
  defaultCommandTimestampsEnabled: boolean;
  defaultPromptHookEnabled: boolean;
  terminalAgentShowDebugMessages: boolean;
  terminalAgentShowRuntimeMessages: boolean;
  terminalAgentShowRunDialog: boolean;
  terminalAgentCommandName: string;
  terminalAgentCommandNameCaseInsensitive: boolean;
  terminalAgentExecutionTarget: TerminalAgentExecutionTarget;
  terminalAgentRememberPanelLayout: boolean;
  terminalAgentPanelDock: TerminalAgentPanelDock;
  terminalAgentPanelHeight?: number;
  terminalAgentPanelSideWidth?: number;
  terminalAgentPanelFontSize?: number;
  defaultAiProfileId?: string;
  aiTavilyApiKey?: string;
  aiBrightDataApiToken?: string;
  aiBraveSearchApiKey?: string;
  aiSearxngUrl?: string;
  aiTavilyMcpServerLabel: string;
  aiBrightDataMcpServerLabel: string;
  aiBraveSearchMcpPluginId?: string;
  aiSearxngMcpPluginId?: string;
  aiLmStudioToolpackMcpPluginId?: string;
  aiSkills: AiSkill[];
  jobSchedulerJournalRetentionDays: number;
  jobSchedulerShowMenuBarStatus: boolean;
  jobSchedulerRsyncPath?: string;
  defaultTerminalEffectPluginId?: string;
  defaultTerminalEffectAnimationSpeed: number;
  lastQuickConnectTerminalEffectPluginId?: string;
  lastQuickConnectTerminalEffectAnimationSpeed?: number;
  terminalRecordingEnabled: boolean;
  terminalRecordingIdleAutoPause: boolean;
  terminalRecordingDirectory?: string;
  localFileBrowserDock: LocalFileBrowserDock;
  localFileBrowserVisible: boolean;
  updateChecksEnabled: boolean;
  updateCheckIntervalDays: number;
  lastSuccessfulUpdateCheckMillis: number;
  ignoredUpdateVersion?: string;
  snoozedUpdateVersion?: string;
  updateSnoozedUntilLocalDate?: string;
  lastAutomaticUpdatePromptVersion?: string;
  lastAutomaticUpdatePromptLocalDate?: string;
  logDirectoryPath?: string;
  logRetentionDays: number;
  appDesign: AppDesign;
  hideTerminalScrollbarsInFullscreen: boolean;
  terminalAgentExecutionEnabled: boolean;
  terminalAgentConfirmMutatingCommandSets: boolean;
  terminalRecordingFormat: TerminalRecordingFormat;
  terminalRecordingDefaultScope: TerminalRecordingScope;
  terminalRecordingIdlePauseSeconds: number;
  terminalRecordingFfmpegPath?: string;
  terminalRecordingCaptureColorsEnabled: boolean;
  snippetDiagramBackgroundColor?: string;
  selectedSnippetEditorProfileId?: string;
  snippetEditorProfiles: SnippetEditorProfile[];
  snippetManagerPreviewDividerPosition?: number;
  snippetHistoryMaxSize: number;
  snippetFontFamily?: string;
  snippetFontSize?: number;
  snippetForegroundColor?: string;
  snippetBackgroundColor?: string;
  snippetCursorColor?: string;
  snippetCursorStyle?: string;
  snippetWordWrap: boolean;
  snippetLineNumbers: boolean;
  aiSnippetEditorAdditionalInstructionsEnabled: boolean;
  aiSnippetAlternativeSolutionCount: number;
}

interface SettingsStore {
  settings: GlobalSettings;
  loading: boolean;
  loadSettings: () => Promise<void>;
  saveSettings: (settings: GlobalSettings) => Promise<void>;
}

export const useSettingsStore = create<SettingsStore>((set) => ({
  settings: {
    language: "en",
    autoDetectLanguage: true,
    defaultFontFamily: "JetBrains Mono",
    defaultFontSize: 14,
    defaultColumns: 80,
    defaultRows: 24,
    defaultScrollbackLines: 10000,
    defaultSshKeepaliveEnabled: true,
    defaultSshKeepaliveInterval: 60,
    defaultConnectionTimeout: 15,
    defaultRetryCount: 4,
    showMenuBar: true,
    storeWindowGeometry: true,
    storeDashboardState: true,
    backupEncryptionType: "Password",
    maxBackups: 10,
    teamworkSources: [],
    teamworkDefaultCheckIntervalMinutes: 15,
    teamworkUseTemporaryKey: false,
    defaultCommandTimestampsEnabled: false,
    defaultPromptHookEnabled: true,
    terminalAgentShowDebugMessages: false,
    terminalAgentShowRuntimeMessages: false,
    terminalAgentShowRunDialog: true,
    terminalAgentCommandName: "agent",
    terminalAgentCommandNameCaseInsensitive: false,
    terminalAgentExecutionTarget: "TerminalWindow",
    terminalAgentRememberPanelLayout: false,
    terminalAgentPanelDock: "bottom",
    terminalAgentPanelHeight: undefined,
    terminalAgentPanelSideWidth: undefined,
    terminalAgentPanelFontSize: undefined,
    defaultAiProfileId: undefined,
    aiTavilyApiKey: undefined,
    aiBrightDataApiToken: undefined,
    aiBraveSearchApiKey: undefined,
    aiSearxngUrl: undefined,
    aiTavilyMcpServerLabel: "tavily",
    aiBrightDataMcpServerLabel: "bright-data",
    aiBraveSearchMcpPluginId: undefined,
    aiSearxngMcpPluginId: undefined,
    aiLmStudioToolpackMcpPluginId: undefined,
    aiSkills: [],
    jobSchedulerJournalRetentionDays: 14,
    jobSchedulerShowMenuBarStatus: false,
    jobSchedulerRsyncPath: undefined,
    defaultTerminalEffectPluginId: undefined,
    defaultTerminalEffectAnimationSpeed: 1,
    lastQuickConnectTerminalEffectPluginId: undefined,
    lastQuickConnectTerminalEffectAnimationSpeed: undefined,
    terminalRecordingEnabled: false,
    terminalRecordingIdleAutoPause: true,
    terminalRecordingDirectory: undefined,
    localFileBrowserDock: "left",
    localFileBrowserVisible: false,
    updateChecksEnabled: true,
    updateCheckIntervalDays: 1,
    lastSuccessfulUpdateCheckMillis: 0,
    ignoredUpdateVersion: undefined,
    snoozedUpdateVersion: undefined,
    updateSnoozedUntilLocalDate: undefined,
    lastAutomaticUpdatePromptVersion: undefined,
    lastAutomaticUpdatePromptLocalDate: undefined,
    logDirectoryPath: undefined,
    logRetentionDays: 7,
    appDesign: "normal",
    hideTerminalScrollbarsInFullscreen: false,
    terminalAgentExecutionEnabled: true,
    terminalAgentConfirmMutatingCommandSets: false,
    terminalRecordingFormat: "KorttyReplay",
    terminalRecordingDefaultScope: "ActiveSplit",
    terminalRecordingIdlePauseSeconds: 20,
    terminalRecordingFfmpegPath: undefined,
    terminalRecordingCaptureColorsEnabled: false,
    snippetDiagramBackgroundColor: "#FFFFFF",
    selectedSnippetEditorProfileId: undefined,
    snippetEditorProfiles: [],
    snippetManagerPreviewDividerPosition: undefined,
    snippetHistoryMaxSize: 30,
    snippetFontFamily: undefined,
    snippetFontSize: undefined,
    snippetForegroundColor: undefined,
    snippetBackgroundColor: undefined,
    snippetCursorColor: undefined,
    snippetCursorStyle: undefined,
    snippetWordWrap: false,
    snippetLineNumbers: false,
    aiSnippetEditorAdditionalInstructionsEnabled: false,
    aiSnippetAlternativeSolutionCount: 3,
  },
  loading: false,

  loadSettings: async () => {
    set({ loading: true });
    try {
      const settings = await invoke<GlobalSettings>("get_settings");
      set({ settings, loading: false });
    } catch (err) {
      console.error("Failed to load settings:", err);
      set({ loading: false });
    }
  },

  saveSettings: async (settings) => {
    try {
      await invoke("save_settings", { settings });
      set({ settings });
    } catch (err) {
      console.error("Failed to save settings:", err);
    }
  },
}));
