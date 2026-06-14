import { lazy, Suspense, useState, useCallback, useEffect, useMemo, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emit, emitTo, listen } from "@tauri-apps/api/event";
import { WebviewWindow, getAllWebviewWindows, getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open as openFileDialog, save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { MenuBar } from "./common/MenuBar";
import { ResizableDivider } from "./common/ResizableDivider";
import { TabBar, Tab } from "./common/TabBar";
import { StatusBar } from "./common/StatusBar";
import {
  TerminalSplitPane,
  serializeSplitTree,
  getLeavesInOrder,
  deserializeSplitTreeWithMapping,
  type SplitNode,
  type SplitTreeTransferNode,
} from "./terminal/TerminalSplitPane";
import { QuickConnect } from "./dialogs/QuickConnect";
import { ConnectionManager } from "./dialogs/ConnectionManager";
import { ConnectionEditor } from "./dialogs/ConnectionEditor";
import { SettingsDialog } from "./dialogs/SettingsDialog";
import { CredentialManager } from "./dialogs/CredentialManager";
import { SSHKeyManager } from "./dialogs/SSHKeyManager";
import { GPGKeyManager } from "./dialogs/GPGKeyManager";
import type { SnippetFileDraft } from "./dialogs/SnippetManager";
import { normalizeSelectedFileName, resolveRemoteFilePath } from "../utils/remoteTextFileSelection";
import { AsciiArtBanner } from "./dialogs/AsciiArtBanner";
import { BackupDialog } from "./dialogs/BackupDialog";
import { ImportDialog } from "./dialogs/ImportDialog";
import { ThemeEditor } from "./dialogs/ThemeEditor";
import { GuiThemeEditor } from "./dialogs/GuiThemeEditor";
import { TeamworkSettingsDialog } from "./dialogs/TeamworkSettingsDialog";
import { ConnectionExportDialog } from "./dialogs/ConnectionExportDialog";
import { ProjectPreviewDialog } from "./dialogs/ProjectPreviewDialog";
import { ProjectSettingsDialog } from "./dialogs/ProjectSettingsDialog";
import { AiActionDialog } from "./dialogs/AiActionDialog";
import { AiAgentDialog } from "./dialogs/AiAgentDialog";
import { AiAgentPlanDialog } from "./dialogs/AiAgentPlanDialog";
import { AiManagerDialog } from "./dialogs/AiManagerDialog";
import { AiChatTab } from "./ai/AiChatTab";
import { AiAgentRunTab } from "./ai/AiAgentRunTab";
import { AiAgentPlanTab } from "./ai/AiAgentPlanTab";
import { LocalFileBrowser } from "./files/LocalFileBrowser";
import { useConnectionStore, ConnectionSettings } from "../store/connectionStore";
import { useProjectStore, type Project } from "../store/projectStore";
import type { GlobalSettings, LocalFileBrowserDock, TerminalAgentPanelDock } from "../store/settingsStore";
import { useThemeStore } from "../store/themeStore";
import { useGuiThemeStore } from "../store/guiThemeStore";
import {
  buildTerminalAgentAskPattern,
  buildTerminalAgentAskPrefixPattern,
  buildTerminalAgentCommandPattern,
  buildTerminalAgentInlinePlanPattern,
  buildTerminalAgentInlinePlanPrefixPattern,
  buildTerminalAgentPlanPattern,
  buildTerminalAgentPlanPrefixPattern,
  buildTerminalAgentUsageText,
  getTerminalAgentAskCommandName,
  getTerminalAgentPlanCommandName,
  normalizeTerminalAgentCommandName,
} from "../utils/terminalAgentCommand";
import { resolvePreferredAiProfileId } from "../utils/aiProfiles";
import { normalizeTerminalEffectSpeed, type TerminalEffectPluginEntry } from "../types/terminalEffects";
import type {
  TerminalRecordingScope,
  TerminalRecordingStartResponse,
} from "../types/terminalRecording";
import {
  UPDATE_AVAILABLE_EVENT,
  type AvailableUpdate,
  type UpdateCheckResult,
} from "../types/update";
import { UpdateAvailableDialog } from "./dialogs/UpdateAvailableDialog";
import { UpdateDownloadDialog } from "./dialogs/UpdateDownloadDialog";
import { TerminalRecordingScopeDialog } from "./dialogs/TerminalRecordingScopeDialog";
import korttyLogo from "../assets/kortty_logo.png";

const JobSchedulerDialog = lazy(() =>
  import("./dialogs/JobSchedulerDialog").then((module) => ({ default: module.JobSchedulerDialog })),
);
const TerminalEffectManagerDialog = lazy(() =>
  import("./dialogs/TerminalEffectManagerDialog").then((module) => ({ default: module.TerminalEffectManagerDialog })),
);
const TerminalRecordingManagerDialog = lazy(() =>
  import("./dialogs/TerminalRecordingManagerDialog").then((module) => ({ default: module.TerminalRecordingManagerDialog })),
);
const SnippetManager = lazy(() =>
  import("./dialogs/SnippetManager").then((module) => ({ default: module.SnippetManager })),
);
const SFTPManager = lazy(() =>
  import("./sftp/SFTPManager").then((module) => ({ default: module.SFTPManager })),
);
import type {
  AiAction,
  AiExecutionResult,
  AiProfile,
  AiRequestPayload,
  TerminalAgentPasswordRequest,
  SavedAiChat,
  TerminalAgentApproval,
  TerminalAgentExecutionTarget,
  TerminalAgentEvent,
  TerminalAgentPlanExecutionResponse,
  TerminalAgentPlanRequest,
  TerminalAgentPlanRunState,
  TerminalAgentPlanStartResponse,
  TerminalAgentRequest,
  TerminalAgentRunState,
  TerminalAgentStartResponse,
} from "../types/ai";

type DialogId =
  | null
  | "quickConnect"
  | "connectionManager"
  | "connectionEditor"
  | "settings"
  | "credentialManager"
  | "sshKeyManager"
  | "gpgKeyManager"
  | "snippetManager"
  | "asciiArt"
  | "backupCreate"
  | "backupImport"
  | "teamworkSettings"
  | "importDialog"
  | "connectionExport"
  | "aiAction"
  | "aiAgent"
  | "aiAgentPlan"
  | "aiManager"
  | "jobScheduler"
  | "terminalEffects"
  | "terminalRecordings"
  | "terminalThemeEditor"
  | "guiThemeEditor"
  | "sftpManager"
  | "projectPreview"
  | "projectSettings"
  | "about";

const MIN_FONT_SIZE = 8;
const MAX_FONT_SIZE = 36;
const DEFAULT_FONT_SIZE = 14;

const FILE_BROWSER_PANEL_WIDTH_KEY = "kortty.localFileBrowser.panelWidth";
const FILE_BROWSER_PANEL_HEIGHT_KEY = "kortty.localFileBrowser.panelHeight";
const FILE_BROWSER_MIN_WIDTH = 180;
const FILE_BROWSER_MAX_WIDTH = 640;
const FILE_BROWSER_MIN_HEIGHT = 120;
const FILE_BROWSER_MAX_HEIGHT = 560;

function readStoredPanelSize(key: string, fallback: number): number {
  try {
    const raw = window.localStorage.getItem(key);
    const parsed = raw != null ? Number.parseInt(raw, 10) : Number.NaN;
    return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
  } catch {
    return fallback;
  }
}

function storePanelSize(key: string, value: number) {
  try {
    window.localStorage.setItem(key, String(Math.round(value)));
  } catch {
    // localStorage unavailable
  }
}

function clampPanelSize(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/**
 * Queries the real geometry (cols/rows + pixel size) of a mounted terminal
 * via a synchronous DOM event answered by TerminalTab (WP3.8).
 */
function getTerminalGeometry(
  sessionId: string,
): { columns: number; rows: number; pixelWidth: number; pixelHeight: number } | null {
  const detail: {
    sessionId: string;
    geometry: { columns: number; rows: number; pixelWidth: number; pixelHeight: number } | null;
  } = { sessionId, geometry: null };
  window.dispatchEvent(new CustomEvent("kortty-terminal-geometry-request", { detail }));
  return detail.geometry;
}

type SessionConnectInfo = {
  host: string;
  port: number;
  username: string;
  authMethod: "Password" | "PrivateKey";
  password?: string;
  credentialId?: string;
  sshKeyId?: string;
  privateKeyPath?: string;
  privateKeyPassphrase?: string;
  temporaryKeyContent?: string;
  temporaryKeyExpirationMinutes?: number;
  temporaryKeyPermanent?: boolean;
  connectionProtocol: "TcpIp" | "Mosh";
  terminalEffectPluginId?: string;
  terminalEffectAnimationSpeed?: number;
  terminalEmulationType?: string;
  terminalColorsEnabled?: boolean;
  /** Fixed AI profile for this session's connection ("" / undefined = default). */
  aiProfileId?: string;
  /** AI skills assigned to this session's connection (always pinned). */
  aiSkillIds?: string[];
};

type DashboardConnectionEntry = {
  kind: "tab" | "split";
  sessionId: string;
  tabId: string;
  label: string;
  status: "connected" | "connecting" | "disconnected";
  config?: SessionConnectInfo;
  themeId?: string;
  fontFamily?: string;
  fontSize?: number;
  foregroundColor?: string;
  backgroundColor?: string;
  cursorColor?: string;
  ansiColors?: string[];
  terminalEffectPluginId?: string;
  terminalEffectAnimationSpeed?: number;
};

type WindowStateSnapshot = {
  label: string;
  name: string;
  updatedAt: number;
  connections: DashboardConnectionEntry[];
};

type SplitTransferEntry = {
  sessionId: string;
  config: SessionConnectInfo;
};

type CrossWindowTransferPayload = {
  sourceWindowLabel: string;
  entry: DashboardConnectionEntry;
  splitEntries?: SplitTransferEntry[];
  /** Nested split layout (horizontal/vertical). If present, splitEntries order should match getLeavesInOrder(splitTree). */
  splitTree?: SplitTreeTransferNode;
  /** If true, target creates tab/sessions but source keeps tab (copy); otherwise source removes tab after consumed (move). */
  copyMode?: boolean;
};

type GlobalSettingsView = {
  defaultCommandTimestampsEnabled?: boolean;
  defaultPromptHookEnabled?: boolean;
  showMenuBar?: boolean;
  terminalAgentCommandName?: string;
  terminalAgentCommandNameCaseInsensitive?: boolean;
  terminalAgentExecutionTarget?: TerminalAgentExecutionTarget;
  terminalAgentShowRunDialog?: boolean;
  terminalAgentRememberPanelLayout?: boolean;
  terminalAgentPanelDock?: TerminalAgentPanelDock;
  terminalAgentPanelHeight?: number;
  terminalAgentPanelSideWidth?: number;
  terminalAgentPanelFontSize?: number;
  defaultAiProfileId?: string;
  localFileBrowserDock?: LocalFileBrowserDock;
  localFileBrowserVisible?: boolean;
  hideTerminalScrollbarsInFullscreen?: boolean;
  terminalRecordingEnabled?: boolean;
  terminalRecordingDefaultScope?: TerminalRecordingScope;
};

type TerminalAgentPanelLayoutSnapshot = {
  terminalAgentPanelDock: TerminalAgentPanelDock;
  terminalAgentPanelHeight?: number;
  terminalAgentPanelSideWidth?: number;
  terminalAgentPanelFontSize?: number;
};

type PendingAiAction = {
  action: AiAction;
  sessionId: string;
  selectedText: string;
  connectionDisplayName?: string;
  connectionAiProfileId?: string;
  connectionAiSkillIds?: string[];
};

type PendingTerminalAgentAction = {
  sessionId: string;
  connectionDisplayName?: string;
  initialPrompt?: string;
  initialProfileId?: string;
  initialExecutionTarget?: TerminalAgentExecutionTarget;
  initialAskConfirmationBeforeEveryCommand?: boolean;
  initialAutoApproveRootCommands?: boolean;
  connectionAiProfileId?: string;
  connectionAiSkillIds?: string[];
};

type ActiveTabTransfer = {
  tabId: string;
  payload: CrossWindowTransferPayload;
};

type TransferConsumedPayload = {
  kind: "tab" | "split";
  tabId: string;
  sessionId: string;
  sourceWindowLabel: string;
  acceptedByWindowLabel: string;
};

type TerminalAppearanceSnapshot = Pick<
  Tab,
  "themeId" | "fontFamily" | "fontSize" | "foregroundColor" | "backgroundColor" | "cursorColor" | "ansiColors"
>;

const CROSS_WINDOW_TRANSFER_MIME = "application/x-kortty-transfer";
const CROSS_WINDOW_TRANSFER_URI_MIME = "text/uri-list";
const CROSS_WINDOW_TRANSFER_PREFIX = "kortty-transfer:";
const CROSS_WINDOW_TRANSFER_STORAGE_PREFIX = "kortty-transfer-payload:";
const TERMINAL_FONT_FAMILY_FALLBACK = "JetBrains Mono, Cascadia Code, Fira Code, Menlo, monospace";

function removeSessionFromTransferTree(
  node: SplitTreeTransferNode,
  sessionId: string,
): SplitTreeTransferNode | null {
  if (node.type === "leaf") {
    return node.sessionId === sessionId ? null : node;
  }

  const children = node.children
    .map((child) => removeSessionFromTransferTree(child, sessionId))
    .filter((child): child is SplitTreeTransferNode => child !== null);

  if (children.length === 0) return null;
  if (children.length === 1) return children[0];
  return { ...node, children };
}

function buildInitialSplitTree(transferTree: SplitTreeTransferNode): SplitNode {
  const identityMap = Object.fromEntries(
    getLeavesInOrder(transferTree).map((sessionId) => [sessionId, sessionId]),
  );
  return deserializeSplitTreeWithMapping(transferTree, identityMap);
}

function buildAiTabLabel(action: AiAction) {
  switch (action) {
    case "Summarize":
      return "AI Summary";
    case "SolveProblem":
      return "AI Problem Analysis";
    case "Ask":
      return "AI Chat";
    case "GenerateChatTitle":
      return "AI Title";
    default:
      return "AI";
  }
}

type TerminalAgentShortcutInvocation = {
  mode: "agent";
  userPrompt: string;
  profileLookup?: string;
  askConfirmationBeforeEveryCommand: boolean;
  autoApproveRootCommands: boolean;
};

type TerminalAskShortcutInvocation = {
  mode: "ask";
  userPrompt: string;
};

type TerminalAgentPlanShortcutInvocation = {
  mode: "plan";
  userPrompt: string;
  profileLookup?: string;
};

function stripMatchingQuotes(value: string) {
  const trimmed = value.trim();
  if (trimmed.length >= 2) {
    const first = trimmed[0];
    const last = trimmed[trimmed.length - 1];
    if ((first === "\"" || first === "'") && first === last) {
      return trimmed.slice(1, -1);
    }
  }
  return trimmed;
}

function splitTerminalAgentOptions(value: string) {
  const parts: string[] = [];
  let current = "";
  let quote: "'" | "\"" | null = null;

  for (const char of value) {
    if ((char === "\"" || char === "'") && (quote === null || quote === char)) {
      quote = quote === char ? null : char;
      current += char;
      continue;
    }
    if (char === "," && quote === null) {
      if (current.trim()) {
        parts.push(current.trim());
      }
      current = "";
      continue;
    }
    current += char;
  }

  if (quote !== null) {
    return { ok: false as const, error: "Unclosed quote in agent options." };
  }
  if (current.trim()) {
    parts.push(current.trim());
  }
  return { ok: true as const, parts };
}

function parseTerminalAgentBooleanOption(name: string, value: string) {
  const normalized = stripMatchingQuotes(value).trim().toLowerCase();
  if (normalized === "true") return true;
  if (normalized === "false") return false;
  throw new Error(`Option \`${name}\` expects \`true\` or \`false\`.`);
}

function parseTerminalAgentShortcut(
  rawCommand: string,
  agentCommandName: string,
  caseInsensitive = false,
):
  | {
      ok: true;
      invocation:
        | TerminalAgentShortcutInvocation
        | TerminalAskShortcutInvocation
        | TerminalAgentPlanShortcutInvocation;
    }
  | { ok: false; error: string } {
  const trimmed = rawCommand.trim();
  const normalizedAgentCommandName = normalizeTerminalAgentCommandName(agentCommandName);
  const askCommandName = getTerminalAgentAskCommandName(normalizedAgentCommandName);
  const planCommandName = getTerminalAgentPlanCommandName(normalizedAgentCommandName);
  const askMatch = trimmed.match(buildTerminalAgentAskPattern(normalizedAgentCommandName, caseInsensitive));
  if (askMatch) {
    const [, rawPrompt = ""] = askMatch;
    const userPrompt = rawPrompt.trim();
    if (!userPrompt) {
      return { ok: false, error: `The ${askCommandName} command is missing the question.` };
    }
    return {
      ok: true,
      invocation: {
        mode: "ask",
        userPrompt,
      },
    };
  }
  if (buildTerminalAgentAskPrefixPattern(normalizedAgentCommandName, caseInsensitive).test(trimmed)) {
    return {
      ok: false,
      error: `Invalid ${askCommandName} command. Use \`${askCommandName} <question>\` or \`${askCommandName}: <question>\`.`,
    };
  }
  const planMatch =
    trimmed.match(buildTerminalAgentPlanPattern(normalizedAgentCommandName, caseInsensitive)) ??
    trimmed.match(buildTerminalAgentInlinePlanPattern(normalizedAgentCommandName, caseInsensitive));
  if (planMatch) {
    const [, rawOptions = "", rawPrompt = ""] = planMatch;
    const userPrompt = rawPrompt.trim();
    if (!userPrompt) {
      return {
        ok: false,
        error: `The ${planCommandName} command is missing the planning task.`,
      };
    }

    let profileLookup: string | undefined;

    if (rawOptions.trim()) {
      const splitOptions = splitTerminalAgentOptions(rawOptions);
      if (!splitOptions.ok) {
        return splitOptions;
      }
      for (const option of splitOptions.parts) {
        const separatorIndex = option.indexOf("=");
        if (separatorIndex <= 0) {
          return {
            ok: false,
            error: `Invalid ${planCommandName} option \`${option}\`. Use \`key=value\`.`,
          };
        }
        const key = option.slice(0, separatorIndex).trim().toLowerCase();
        const value = option.slice(separatorIndex + 1).trim();
        if (!value) {
          return {
            ok: false,
            error: `${planCommandName} option \`${key}\` is missing a value.`,
          };
        }
        switch (key) {
          case "profile":
            profileLookup = stripMatchingQuotes(value);
            if (!profileLookup) {
              return {
                ok: false,
                error: `${planCommandName} option \`profile\` must not be empty.`,
              };
            }
            break;
          default:
            return {
              ok: false,
              error: `Unknown ${planCommandName} option \`${key}\`. Supported option: profile.`,
            };
        }
      }
    }

    return {
      ok: true,
      invocation: {
        mode: "plan",
        userPrompt,
        profileLookup,
      },
    };
  }
  if (
    buildTerminalAgentPlanPrefixPattern(normalizedAgentCommandName, caseInsensitive).test(trimmed) ||
    buildTerminalAgentInlinePlanPrefixPattern(normalizedAgentCommandName, caseInsensitive).test(trimmed)
  ) {
    return {
      ok: false,
      error: `Invalid ${planCommandName} command. Use \`${planCommandName} <prompt>\`, \`${planCommandName}: <prompt>\`, or \`${planCommandName}(profile=name) <prompt>\`.`,
    };
  }
  const match = trimmed.match(buildTerminalAgentCommandPattern(normalizedAgentCommandName, caseInsensitive));
  if (!match) {
    return {
      ok: false,
      error: `Invalid agent command. ${buildTerminalAgentUsageText(normalizedAgentCommandName)}`,
    };
  }

  const [, rawOptions = "", rawPrompt = ""] = match;
  const userPrompt = rawPrompt.trim();
  if (!userPrompt) {
    return {
      ok: false,
      error: `The agent command is missing the prompt after \`${normalizedAgentCommandName}\`.`,
    };
  }

  let profileLookup: string | undefined;
  let askConfirmationBeforeEveryCommand = false;
  let autoApproveRootCommands = false;

  if (rawOptions.trim()) {
    const splitOptions = splitTerminalAgentOptions(rawOptions);
    if (!splitOptions.ok) {
      return splitOptions;
    }
    for (const option of splitOptions.parts) {
      const separatorIndex = option.indexOf("=");
      if (separatorIndex <= 0) {
        return {
          ok: false,
          error: `Invalid agent option \`${option}\`. Use \`key=value\`.`,
        };
      }
      const key = option.slice(0, separatorIndex).trim().toLowerCase();
      const value = option.slice(separatorIndex + 1).trim();
      if (!value) {
        return {
          ok: false,
          error: `Agent option \`${key}\` is missing a value.`,
        };
      }
      try {
        switch (key) {
          case "profile":
            profileLookup = stripMatchingQuotes(value);
            if (!profileLookup) {
              return {
                ok: false,
                error: "Agent option `profile` must not be empty.",
              };
            }
            break;
          case "ask":
            askConfirmationBeforeEveryCommand = parseTerminalAgentBooleanOption(key, value);
            break;
          case "root":
            autoApproveRootCommands = parseTerminalAgentBooleanOption(key, value);
            break;
          default:
            return {
              ok: false,
              error: `Unknown agent option \`${key}\`. Supported options: profile, root, ask.`,
            };
        }
      } catch (error) {
        return {
          ok: false,
          error: error instanceof Error ? error.message : String(error),
        };
      }
    }
  }

  if (askConfirmationBeforeEveryCommand) {
    autoApproveRootCommands = false;
  }

  return {
    ok: true,
    invocation: {
      mode: "agent",
      userPrompt,
      profileLookup,
      askConfirmationBeforeEveryCommand,
      autoApproveRootCommands,
    },
  };
}

function storeCrossWindowTransferPayload(payload: CrossWindowTransferPayload): string {
  const now = Date.now();
  for (let index = localStorage.length - 1; index >= 0; index -= 1) {
    const key = localStorage.key(index);
    if (!key?.startsWith(CROSS_WINDOW_TRANSFER_STORAGE_PREFIX)) {
      continue;
    }

    try {
      const parsed = JSON.parse(localStorage.getItem(key) ?? "null") as { createdAt?: number } | null;
      if (!parsed?.createdAt || now - parsed.createdAt > 10 * 60 * 1000) {
        localStorage.removeItem(key);
      }
    } catch {
      localStorage.removeItem(key);
    }
  }

  const transferId = crypto.randomUUID();
  const storageKey = `${CROSS_WINDOW_TRANSFER_STORAGE_PREFIX}${transferId}`;
  const serialized = JSON.stringify({
    createdAt: now,
    payload,
  });
  localStorage.setItem(storageKey, serialized);
  return `${CROSS_WINDOW_TRANSFER_PREFIX}${transferId}`;
}

function readCrossWindowTransferPayload(token: string): CrossWindowTransferPayload | null {
  if (!token.startsWith(CROSS_WINDOW_TRANSFER_PREFIX)) {
    return null;
  }

  const transferId = token.slice(CROSS_WINDOW_TRANSFER_PREFIX.length);
  if (!transferId) {
    return null;
  }

  const storageKey = `${CROSS_WINDOW_TRANSFER_STORAGE_PREFIX}${transferId}`;
  const raw = localStorage.getItem(storageKey);
  if (!raw) {
    return null;
  }

  try {
    const parsed = JSON.parse(raw) as { payload?: CrossWindowTransferPayload };
    return parsed.payload ?? null;
  } catch {
    return null;
  }
}

function clearCrossWindowTransferPayload(token: string): void {
  if (!token.startsWith(CROSS_WINDOW_TRANSFER_PREFIX)) {
    return;
  }

  const transferId = token.slice(CROSS_WINDOW_TRANSFER_PREFIX.length);
  if (!transferId) {
    return;
  }

  localStorage.removeItem(`${CROSS_WINDOW_TRANSFER_STORAGE_PREFIX}${transferId}`);
}

function buildFontFamilyStack(fontFamily?: string): string {
  return fontFamily ? `${fontFamily}, ${TERMINAL_FONT_FAMILY_FALLBACK}` : TERMINAL_FONT_FAMILY_FALLBACK;
}

/**
 * True when a key event originates from a terminal/xterm view or an editable
 * element, i.e. a target that owns paste handling itself. Used to avoid
 * hijacking Ctrl/Cmd+Shift+V (the terminal paste binding) with global
 * shortcuts.
 */
function eventTargetConsumesPaste(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) {
    return false;
  }
  if (target.closest(".xterm, .xterm-helper-textarea")) {
    return true;
  }
  if (target instanceof HTMLElement && target.isContentEditable) {
    return true;
  }
  const tagName = target.tagName;
  return tagName === "INPUT" || tagName === "TEXTAREA" || tagName === "SELECT";
}

function copyAnsiColors(ansiColors?: string[]): string[] | undefined {
  if (!ansiColors || ansiColors.length === 0) {
    return undefined;
  }
  return [...ansiColors];
}

function buildTerminalAppearanceSnapshot(
  appearance: Pick<
    ConnectionSettings,
    "themeId" | "fontFamily" | "fontSize" | "foregroundColor" | "backgroundColor" | "cursorColor" | "ansiColors"
  >,
): TerminalAppearanceSnapshot {
  return {
    themeId: appearance.themeId,
    fontFamily: appearance.fontFamily,
    fontSize: appearance.fontSize,
    foregroundColor: appearance.foregroundColor,
    backgroundColor: appearance.backgroundColor,
    cursorColor: appearance.cursorColor,
    ansiColors: copyAnsiColors(appearance.ansiColors),
  };
}

function buildTerminalAppearanceFromTab(tab: Tab): TerminalAppearanceSnapshot {
  return {
    themeId: tab.themeId,
    fontFamily: tab.fontFamily,
    fontSize: tab.fontSize,
    foregroundColor: tab.foregroundColor,
    backgroundColor: tab.backgroundColor,
    cursorColor: tab.cursorColor,
    ansiColors: copyAnsiColors(tab.ansiColors),
  };
}

export function MainWindow() {
  const { t } = useTranslation();
  const currentWindowLabel = getCurrentWebviewWindow().label;
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTab, setActiveTab] = useState<string | null>(null);
  const [showDashboard, setShowDashboard] = useState(false);
  const [terminalOnlyFullscreen, setTerminalOnlyFullscreen] = useState(false);
  const [hideTerminalScrollbarsInFullscreen, setHideTerminalScrollbarsInFullscreen] = useState(false);
  const [localFileBrowserVisible, setLocalFileBrowserVisible] = useState(false);
  const [localFileBrowserDock, setLocalFileBrowserDock] = useState<LocalFileBrowserDock>("left");
  const [fileBrowserPanelWidth, setFileBrowserPanelWidth] = useState(() =>
    clampPanelSize(
      readStoredPanelSize(FILE_BROWSER_PANEL_WIDTH_KEY, 280),
      FILE_BROWSER_MIN_WIDTH,
      FILE_BROWSER_MAX_WIDTH,
    ),
  );
  const [fileBrowserPanelHeight, setFileBrowserPanelHeight] = useState(() =>
    clampPanelSize(
      readStoredPanelSize(FILE_BROWSER_PANEL_HEIGHT_KEY, 224),
      FILE_BROWSER_MIN_HEIGHT,
      FILE_BROWSER_MAX_HEIGHT,
    ),
  );
  const [recordingErrorMessage, setRecordingErrorMessage] = useState<string | null>(null);
  const [recordingScopeRequest, setRecordingScopeRequest] = useState<{
    tabId: string;
    targetSessionId: string;
    defaultScope: TerminalRecordingScope;
  } | null>(null);
  const [availableUpdate, setAvailableUpdate] = useState<AvailableUpdate | null>(null);
  const [updateDialogManual, setUpdateDialogManual] = useState(false);
  const [updateDownload, setUpdateDownload] = useState<AvailableUpdate | null>(null);
  const [updateCheckBusy, setUpdateCheckBusy] = useState(false);
  const [updateCheckStatus, setUpdateCheckStatus] = useState("");
  const [connectionCount, setConnectionCount] = useState(0);
  const [openDialog, setOpenDialog] = useState<DialogId>(null);
  const [editingConnection, setEditingConnection] = useState<ConnectionSettings | null>(null);
  const [projectPreview, setProjectPreview] = useState<Project | null>(null);
  const [projectSettingsDraft, setProjectSettingsDraft] = useState<Project | null>(null);
  const [pendingAiAction, setPendingAiAction] = useState<PendingAiAction | null>(null);
  const [pendingTerminalAgentAction, setPendingTerminalAgentAction] = useState<PendingTerminalAgentAction | null>(null);
  const [snippetFileDraft, setSnippetFileDraft] = useState<SnippetFileDraft | null>(null);
  /** WP1.3c: transient toast for the terminal "open selection in snippet editor" flow. */
  const [terminalFileLoadNotice, setTerminalFileLoadNotice] = useState<string | null>(null);
  const terminalFileLoadNoticeTimerRef = useRef<number | null>(null);
  const [pendingTerminalAgentMode, setPendingTerminalAgentMode] = useState<"run" | "plan" | null>(null);
  const [terminalAgentStates, setTerminalAgentStates] = useState<Record<string, TerminalAgentRunState>>({});
  const [globalFontSize, setGlobalFontSize] = useState(DEFAULT_FONT_SIZE);
  const [tabFontSizes, setTabFontSizes] = useState<Record<string, number>>({});
  const [paneFontSizes, setPaneFontSizes] = useState<Record<string, number>>({});
  const [showTimestamps, setShowTimestamps] = useState(false);
  const [recordingSessions, setRecordingSessions] = useState<Record<string, TerminalRecordingStartResponse | undefined>>({});
  const [promptHookEnabled, setPromptHookEnabled] = useState(true);
  const [showMenuBar, setShowMenuBar] = useState(true);
  const [terminalAgentCommandName, setTerminalAgentCommandName] = useState("agent");
  const [terminalAgentCommandNameCaseInsensitive, setTerminalAgentCommandNameCaseInsensitive] = useState(false);
  const [terminalAgentShowRunDialog, setTerminalAgentShowRunDialog] = useState(true);
  const [terminalAgentRememberPanelLayout, setTerminalAgentRememberPanelLayout] = useState(false);
  const [terminalAgentPanelDock, setTerminalAgentPanelDock] = useState<TerminalAgentPanelDock>("bottom");
  const [terminalAgentPanelHeight, setTerminalAgentPanelHeight] = useState<number | undefined>(undefined);
  const [terminalAgentPanelSideWidth, setTerminalAgentPanelSideWidth] = useState<number | undefined>(undefined);
  const [terminalAgentPanelFontSize, setTerminalAgentPanelFontSize] = useState<number | undefined>(undefined);
  const [terminalAgentExecutionTarget, setTerminalAgentExecutionTarget] =
    useState<TerminalAgentExecutionTarget>("TerminalWindow");
  const [defaultAiProfileId, setDefaultAiProfileId] = useState("");
  const [hasConfiguredAiProfiles, setHasConfiguredAiProfiles] = useState(false);
  const [settingsReady, setSettingsReady] = useState(false);
  const globalFontSizeRef = useRef(globalFontSize);
  globalFontSizeRef.current = globalFontSize;
  const activeTabRef = useRef<string | null>(activeTab);
  activeTabRef.current = activeTab;
  const focusedPaneSessionRef = useRef<string | null>(null);
  const { loadConnections } = useConnectionStore();
  const { currentProject, setCurrentProject, setRecentProjects, addRecentProject } = useProjectStore();
  const { theme: activeTheme, loadActiveTheme } = useThemeStore();
  const { loadActiveGuiTheme } = useGuiThemeStore();
  const [allThemes, setAllThemes] = useState<import("../store/themeStore").ThemeData[]>([]);
  const [terminalEffects, setTerminalEffects] = useState<TerminalEffectPluginEntry[]>([]);
  const [tabSplitSessions, setTabSplitSessions] = useState<Record<string, string[]>>({});
  const [splitSessionConfigs, setSplitSessionConfigs] = useState<Record<string, SessionConnectInfo>>({});
  /** Stored split tree per tab (for transfer). Updated by TerminalSplitPane onTreeChange. */
  const [tabSplitTrees, setTabSplitTrees] = useState<Record<string, SplitTreeTransferNode>>({});
  /** One-time initial tree when tab was created from transfer (so layout is restored). */
  const [tabInitialSplitTree, setTabInitialSplitTree] = useState<Record<string, SplitNode>>({});
  const [windowName, setWindowName] = useState(`Window ${currentWindowLabel.slice(-4)}`);
  const [workspaceWindows, setWorkspaceWindows] = useState<Record<string, WindowStateSnapshot>>({});
  const [dragOverWindowLabel, setDragOverWindowLabel] = useState<string | null>(null);
  const splitResolveRef = useRef<((sessionId: string | null) => void) | null>(null);
  const splitTabRef = useRef<string | null>(null);
  const projectSaveModeRef = useRef<"save" | "saveAs" | "edit">("save");
  const activeTabTransferRef = useRef<ActiveTabTransfer | null>(null);
  const tabsRef = useRef<Tab[]>([]);
  const splitSessionsRef = useRef<Record<string, string[]>>({});
  const localSnapshotRef = useRef<WindowStateSnapshot | null>(null);
  const localWindowSnapshotRef = useRef<WindowStateSnapshot | null>(null);
  const createSshSessionRef = useRef<(sessionId: string, info: SessionConnectInfo) => Promise<boolean>>(async () => false);
  const transferDropProcessedRef = useRef<Set<string>>(new Set());
  const processTransferPayloadRef = useRef<(payload: CrossWindowTransferPayload) => Promise<void>>(async () => {});
  const pollingPendingTransferRef = useRef(false);
  const terminalAgentPanelLayoutRef = useRef<TerminalAgentPanelLayoutSnapshot>({
    terminalAgentPanelDock: "bottom",
    terminalAgentPanelHeight: undefined,
    terminalAgentPanelSideWidth: undefined,
    terminalAgentPanelFontSize: undefined,
  });
  const terminalAgentPanelLayoutSaveTimerRef = useRef<number | null>(null);
  const activeTabEntry = useMemo(
    () => tabs.find((tab) => tab.id === activeTab) ?? null,
    [tabs, activeTab],
  );
  const activeTerminalSessionId = (activeTabEntry?.kind ?? "terminal") === "terminal" ? activeTabEntry?.id ?? "" : "";

  async function emitTransferWithRetry(
    targetLabel: string,
    payload: CrossWindowTransferPayload,
  ): Promise<void> {
    const payloadJson = JSON.stringify(payload);
    try {
      await invoke("store_pending_transfer", { targetLabel, payloadJson });
    } catch {}
    await emitTo(targetLabel, "kortty-transfer-drop", payload);
    for (let i = 0; i < 3; i++) {
      await new Promise((r) => setTimeout(r, 500));
      await emitTo(targetLabel, "kortty-transfer-drop", payload);
    }
  }

  tabsRef.current = tabs;
  splitSessionsRef.current = tabSplitSessions;

  const applyGlobalSettingsView = useCallback((settings: GlobalSettingsView) => {
    setShowTimestamps(!!settings.defaultCommandTimestampsEnabled);
    setPromptHookEnabled(settings.defaultPromptHookEnabled !== false);
    setShowMenuBar(settings.showMenuBar !== false);
    setTerminalAgentCommandName(
      normalizeTerminalAgentCommandName(settings.terminalAgentCommandName),
    );
    setTerminalAgentCommandNameCaseInsensitive(!!settings.terminalAgentCommandNameCaseInsensitive);
    setTerminalAgentShowRunDialog(settings.terminalAgentShowRunDialog !== false);
    setTerminalAgentRememberPanelLayout(!!settings.terminalAgentRememberPanelLayout);
    const nextAgentPanelLayout = {
      terminalAgentPanelDock: settings.terminalAgentPanelDock ?? "bottom",
      terminalAgentPanelHeight: settings.terminalAgentPanelHeight,
      terminalAgentPanelSideWidth: settings.terminalAgentPanelSideWidth,
      terminalAgentPanelFontSize: settings.terminalAgentPanelFontSize,
    };
    terminalAgentPanelLayoutRef.current = nextAgentPanelLayout;
    setTerminalAgentPanelDock(nextAgentPanelLayout.terminalAgentPanelDock);
    setTerminalAgentPanelHeight(nextAgentPanelLayout.terminalAgentPanelHeight);
    setTerminalAgentPanelSideWidth(nextAgentPanelLayout.terminalAgentPanelSideWidth);
    setTerminalAgentPanelFontSize(nextAgentPanelLayout.terminalAgentPanelFontSize);
    setTerminalAgentExecutionTarget(settings.terminalAgentExecutionTarget ?? "TerminalWindow");
    setDefaultAiProfileId(settings.defaultAiProfileId ?? "");
    setLocalFileBrowserDock(settings.localFileBrowserDock ?? "left");
    setLocalFileBrowserVisible(!!settings.localFileBrowserVisible);
    setHideTerminalScrollbarsInFullscreen(!!settings.hideTerminalScrollbarsInFullscreen);
  }, []);

  const loadAiProfileAvailability = useCallback(async () => {
    try {
      const profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
      return profiles;
    } catch (error) {
      console.error("Failed to load AI profiles:", error);
      setHasConfiguredAiProfiles(false);
      return [];
    }
  }, []);

  useEffect(() => {
    loadConnections();
    loadActiveTheme();
    loadActiveGuiTheme();
    invoke<import("../store/themeStore").ThemeData[]>("get_themes")
      .then(setAllThemes)
      .catch(console.error);
    invoke<TerminalEffectPluginEntry[]>("list_terminal_effect_plugins")
      .then(setTerminalEffects)
      .catch(console.error);
    invoke<GlobalSettingsView>("get_settings")
      .then((settings) => {
        applyGlobalSettingsView(settings);
      })
      .catch(console.error)
      .finally(() => setSettingsReady(true));
    void loadAiProfileAvailability();
    invoke<string[]>("get_recent_projects")
      .then(setRecentProjects)
      .catch(console.error);
  }, [applyGlobalSettingsView, loadAiProfileAvailability, loadConnections, loadActiveTheme, loadActiveGuiTheme, setRecentProjects]);

  const reloadTerminalEffects = useCallback(async () => {
    try {
      setTerminalEffects(await invoke<TerminalEffectPluginEntry[]>("list_terminal_effect_plugins"));
    } catch (error) {
      console.error("Failed to load terminal effect plugins:", error);
    }
  }, []);

  useEffect(() => {
    let offSettingsUpdated: (() => void) | null = null;
    listen<GlobalSettingsView>("kortty-settings-updated", (event) => {
      applyGlobalSettingsView(event.payload);
      setSettingsReady(true);
    }).then((fn) => {
      offSettingsUpdated = fn;
    }).catch(console.error);

    return () => {
      offSettingsUpdated?.();
    };
  }, [applyGlobalSettingsView]);

  useEffect(() => {
    return () => {
      if (terminalAgentPanelLayoutSaveTimerRef.current != null) {
        window.clearTimeout(terminalAgentPanelLayoutSaveTimerRef.current);
      }
    };
  }, []);

  const persistTerminalAgentPanelLayout = useCallback((patch: Partial<TerminalAgentPanelLayoutSnapshot>) => {
    const nextLayout = {
      ...terminalAgentPanelLayoutRef.current,
      ...patch,
    };
    terminalAgentPanelLayoutRef.current = nextLayout;

    if (patch.terminalAgentPanelDock !== undefined) {
      setTerminalAgentPanelDock(patch.terminalAgentPanelDock);
    }
    if (patch.terminalAgentPanelHeight !== undefined) {
      setTerminalAgentPanelHeight(patch.terminalAgentPanelHeight);
    }
    if (patch.terminalAgentPanelSideWidth !== undefined) {
      setTerminalAgentPanelSideWidth(patch.terminalAgentPanelSideWidth);
    }
    if (patch.terminalAgentPanelFontSize !== undefined) {
      setTerminalAgentPanelFontSize(patch.terminalAgentPanelFontSize);
    }

    if (terminalAgentPanelLayoutSaveTimerRef.current != null) {
      window.clearTimeout(terminalAgentPanelLayoutSaveTimerRef.current);
    }

    if (!terminalAgentRememberPanelLayout) {
      return;
    }

    terminalAgentPanelLayoutSaveTimerRef.current = window.setTimeout(() => {
      terminalAgentPanelLayoutSaveTimerRef.current = null;
      if (!terminalAgentRememberPanelLayout) {
        return;
      }
      void invoke<GlobalSettings>("get_settings")
        .then((settings) =>
          invoke("save_settings", {
            settings: {
              ...settings,
              terminalAgentPanelDock: nextLayout.terminalAgentPanelDock,
              terminalAgentPanelHeight: nextLayout.terminalAgentPanelHeight,
              terminalAgentPanelSideWidth: nextLayout.terminalAgentPanelSideWidth,
              terminalAgentPanelFontSize: nextLayout.terminalAgentPanelFontSize,
            },
          }),
        )
        .catch((error) => {
          console.error("Failed to save terminal agent panel layout:", error);
        });
    }, 250);
  }, [terminalAgentRememberPanelLayout]);

  useEffect(() => {
    getCurrentWindow().setTitle(`KorTTY - ${windowName}`).catch(console.error);
  }, [windowName]);

  useEffect(() => {
    let offSetName: (() => void) | null = null;
    listen<{ name: string }>("kortty-set-window-name", (event) => {
      if (event.payload?.name) {
        setWindowName(event.payload.name);
      }
    }).then((fn) => {
      offSetName = fn;
    });
    return () => {
      offSetName?.();
    };
  }, []);

  const defaultTerminalTheme = useMemo(() => ({
    foreground: activeTheme.foregroundColor,
    background: activeTheme.backgroundColor,
    cursor: activeTheme.cursorColor,
    selectionBackground: activeTheme.selectionColor + "80",
    ansiColors: activeTheme.ansiColors,
  }), [activeTheme]);

  const defaultTerminalFontFamily = useMemo(
    () => buildFontFamilyStack(activeTheme.fontFamily),
    [activeTheme.fontFamily],
  );

  const getTabTheme = useCallback(
    (tab: Tab) => {
      const selectedTheme = tab.themeId
        ? allThemes.find((theme) => theme.id === tab.themeId)
        : undefined;
      if (selectedTheme) {
        return {
          theme: {
            foreground: selectedTheme.foregroundColor,
            background: selectedTheme.backgroundColor,
            cursor: selectedTheme.cursorColor,
            selectionBackground: selectedTheme.selectionColor + "80",
            ansiColors: selectedTheme.ansiColors,
          },
          fontFamily: buildFontFamilyStack(selectedTheme.fontFamily),
          fontSize: selectedTheme.fontSize,
        };
      }

      return {
        theme: {
          foreground: tab.foregroundColor ?? defaultTerminalTheme.foreground,
          background: tab.backgroundColor ?? defaultTerminalTheme.background,
          cursor: tab.cursorColor ?? defaultTerminalTheme.cursor,
          selectionBackground: defaultTerminalTheme.selectionBackground,
          ansiColors: copyAnsiColors(tab.ansiColors) ?? defaultTerminalTheme.ansiColors,
        },
        fontFamily: buildFontFamilyStack(tab.fontFamily) || defaultTerminalFontFamily,
        fontSize: tab.fontSize ?? DEFAULT_FONT_SIZE,
      };
    },
    [allThemes, defaultTerminalTheme, defaultTerminalFontFamily],
  );

  useEffect(() => {
    setConnectionCount(tabs.filter((t) => (t.kind ?? "terminal") === "terminal" && t.status === "connected").length);
  }, [tabs]);

  // Content-based key so we only recompute the snapshot when data actually changes,
  // not on every render or when only reference identity changes (e.g. other state updates).
  const localWindowSnapshotKey = useMemo(
    () =>
      JSON.stringify({
        l: currentWindowLabel,
        n: windowName,
        t: tabs
          .filter((tab) => (tab.kind ?? "terminal") === "terminal")
          .map((tab) => ({
            i: tab.id,
            lb: tab.label,
            s: tab.status,
            h: tab.host,
            u: tab.username,
            p: tab.port,
            am: tab.authMethod,
            cid: tab.credentialId,
            sk: tab.sshKeyId,
            pk: tab.privateKeyPath,
            cp: tab.connectionProtocol,
            ep: tab.terminalEffectPluginId,
            es: tab.terminalEffectAnimationSpeed,
            th: tab.themeId,
            ff: tab.fontFamily,
            fs: tab.fontSize,
            fg: tab.foregroundColor,
            bg: tab.backgroundColor,
            cur: tab.cursorColor,
            ansi: tab.ansiColors,
          })),
        s: tabSplitSessions,
        c: splitSessionConfigs,
      }),
    [currentWindowLabel, windowName, tabs, tabSplitSessions, splitSessionConfigs]
  );

  const localWindowSnapshot = useMemo<WindowStateSnapshot>(() => {
    const connections: DashboardConnectionEntry[] = [];

    for (const tab of tabs) {
      if ((tab.kind ?? "terminal") === "ai") {
        continue;
      }
      const tabConfig =
        tab.host && tab.username
          ? {
              host: tab.host,
              port: tab.port || 22,
              username: tab.username,
              authMethod: tab.authMethod || "Password",
              password: tab.password,
              credentialId: tab.credentialId,
              sshKeyId: tab.sshKeyId,
              privateKeyPath: tab.privateKeyPath,
              privateKeyPassphrase: tab.privateKeyPassphrase,
              temporaryKeyContent: tab.temporaryKeyContent,
              temporaryKeyExpirationMinutes: tab.temporaryKeyExpirationMinutes,
              temporaryKeyPermanent: tab.temporaryKeyPermanent,
              connectionProtocol: tab.connectionProtocol || "TcpIp",
              terminalEffectPluginId: tab.terminalEffectPluginId,
              terminalEffectAnimationSpeed: tab.terminalEffectAnimationSpeed,
            }
          : undefined;
      connections.push({
        kind: "tab",
        sessionId: tab.id,
        tabId: tab.id,
        label: tab.label,
        status: tab.status,
        config: tabConfig,
        ...buildTerminalAppearanceFromTab(tab),
        terminalEffectPluginId: tab.terminalEffectPluginId,
        terminalEffectAnimationSpeed: tab.terminalEffectAnimationSpeed,
      });

      const splitIds = tabSplitSessions[tab.id] || [];
      for (const splitId of splitIds) {
        const splitCfg = splitSessionConfigs[splitId];
        connections.push({
          kind: "split",
          sessionId: splitId,
          tabId: tab.id,
          label: `Split: ${splitCfg?.username || "user"}@${splitCfg?.host || "host"}`,
          status: "connected",
          config: splitCfg,
        });
      }
    }

    return {
      label: currentWindowLabel,
      name: windowName,
      updatedAt: Date.now(),
      connections,
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- key is content-based; tabs/splits/config read from closure when key changes
  }, [localWindowSnapshotKey]);
  localWindowSnapshotRef.current = localWindowSnapshot;

  const workspaceWindowList = useMemo(
    () =>
      Object.values(workspaceWindows).sort((a, b) => {
        if (a.label === currentWindowLabel) return -1;
        if (b.label === currentWindowLabel) return 1;
        return a.name.localeCompare(b.name) || a.label.localeCompare(b.label);
      }),
    [workspaceWindows, currentWindowLabel],
  );
  const otherWorkspaceWindows = useMemo(
    () =>
      workspaceWindowList
        .filter((win) => win.label !== currentWindowLabel)
        .map((win) => ({ label: win.label, name: win.name })),
    [workspaceWindowList, currentWindowLabel],
  );

  const publishWindowState = useCallback(
    (updatedAt?: number) => {
      const snapshot: WindowStateSnapshot = {
        ...localWindowSnapshot,
        updatedAt: updatedAt ?? Date.now(),
      };
      localSnapshotRef.current = snapshot;
      setWorkspaceWindows((prev) => ({
        ...prev,
        [currentWindowLabel]: snapshot,
      }));
      emit("kortty-window-state", snapshot).catch(console.error);
    },
    [currentWindowLabel, localWindowSnapshot],
  );

  useEffect(() => {
    publishWindowState();
  }, [publishWindowState]);

  useEffect(() => {
    emit("kortty-window-state-request", { requester: currentWindowLabel }).catch(console.error);
  }, [currentWindowLabel, showDashboard]);

  useEffect(() => {
    const interval = setInterval(async () => {
      publishWindowState();
      emit("kortty-window-state-request", { requester: currentWindowLabel }).catch(console.error);
      try {
        const windows = await getAllWebviewWindows();
        const labels = new Set(windows.map((w) => w.label));
        labels.add(currentWindowLabel);
        setWorkspaceWindows((prev) => {
          const next: Record<string, WindowStateSnapshot> = {};
          for (const label of labels) {
            const existing = prev[label];
            if (existing) {
              next[label] = existing;
            } else {
              next[label] = {
                label,
                name: `Window ${label.slice(-4)}`,
                updatedAt: Date.now(),
                connections: [],
              };
            }
          }
          return next;
        });
      } catch (err) {
        console.error("Failed to reconcile workspace windows:", err);
      }
    }, 5000);
    return () => clearInterval(interval);
  }, [currentWindowLabel, publishWindowState]);

  useEffect(() => {
    let offState: (() => void) | null = null;
    let offReq: (() => void) | null = null;
    let offTransferConsumed: (() => void) | null = null;
    let offFocusConnection: (() => void) | null = null;

    listen<WindowStateSnapshot>("kortty-window-state", (event) => {
      const snapshot = event.payload;
      setWorkspaceWindows((prev) => ({
        ...prev,
        [snapshot.label]:
          !prev[snapshot.label] || snapshot.updatedAt >= prev[snapshot.label].updatedAt
            ? snapshot
            : prev[snapshot.label],
      }));
    }).then((fn) => {
      offState = fn;
    });

    listen<{ requester: string }>("kortty-window-state-request", (event) => {
      if (event.payload.requester === currentWindowLabel) return;
      const latest = localSnapshotRef.current ?? localWindowSnapshotRef.current;
      if (!latest) {
        return;
      }
      emitTo(event.payload.requester, "kortty-window-state", {
        ...latest,
        updatedAt: Date.now(),
      }).catch(console.error);
    }).then((fn) => {
      offReq = fn;
    });

    listen<TransferConsumedPayload>(
      "kortty-transfer-consumed",
      (event) => {
        const payload = event.payload;
        if (payload.sourceWindowLabel !== currentWindowLabel) {
          return;
        }
        if (payload.kind === "tab") {
          // Move only removes local UI ownership; session stays alive for target window.
          const splitSessions = splitSessionsRef.current[payload.tabId] || [];
          for (const splitId of splitSessions) {
            setSplitSessionConfigs((prev) => {
              const next = { ...prev };
              delete next[splitId];
              return next;
            });
          }
          setTabSplitSessions((prev) => {
            const next = { ...prev };
            delete next[payload.tabId];
            return next;
          });
          setTabSplitTrees((prev) => {
            const next = { ...prev };
            delete next[payload.tabId];
            return next;
          });
          setTabInitialSplitTree((prev) => {
            const next = { ...prev };
            delete next[payload.tabId];
            return next;
          });
          setTabs((prev) => {
            const remaining = prev.filter((t) => t.id !== payload.tabId);
            if (activeTabRef.current === payload.tabId) {
              setActiveTab(remaining.length > 0 ? remaining[remaining.length - 1].id : null);
            }
            return remaining;
          });
          return;
        }
        window.dispatchEvent(
          new CustomEvent("kortty-remove-split-session", {
            detail: { sessionId: payload.sessionId },
          }),
        );
      },
    ).then((fn) => {
      offTransferConsumed = fn;
    });

    listen<{ tabId: string }>("kortty-focus-connection", (event) => {
      setActiveTab(event.payload.tabId);
      getCurrentWindow().setFocus().catch(console.error);
    }).then((fn) => {
      offFocusConnection = fn;
    });

    return () => {
      offState?.();
      offReq?.();
      offTransferConsumed?.();
      offFocusConnection?.();
    };
  }, [currentWindowLabel]);

  useEffect(() => {
    let offTransferDrop: (() => void) | null = null;
    listen<CrossWindowTransferPayload>("kortty-transfer-drop", async (event) => {
      await processTransferPayloadRef.current(event.payload);
    }).then((fn) => {
      offTransferDrop = fn;
    });
    return () => {
      offTransferDrop?.();
    };
  }, [currentWindowLabel]);

  const hasCheckedPendingTransferRef = useRef(false);
  useEffect(() => {
    if (hasCheckedPendingTransferRef.current) return;
    hasCheckedPendingTransferRef.current = true;
    const timer = setTimeout(async () => {
      try {
        const raw = await invoke<string | null>("take_pending_transfer", {
          windowLabel: currentWindowLabel,
        });
        if (!raw) return;
        const payload = JSON.parse(raw) as CrossWindowTransferPayload;
        if (payload.sourceWindowLabel === currentWindowLabel) return;
        await processTransferPayloadRef.current(payload);
      } catch {
        // ignore
      }
    }, 150);
    return () => clearTimeout(timer);
  }, [currentWindowLabel]);

  useEffect(() => {
    let disposed = false;

    const pollPendingTransfer = async () => {
      if (disposed || pollingPendingTransferRef.current) {
        return;
      }

      pollingPendingTransferRef.current = true;
      try {
        const raw = await invoke<string | null>("take_pending_transfer", {
          windowLabel: currentWindowLabel,
        });
        if (!raw) {
          return;
        }

        const payload = JSON.parse(raw) as CrossWindowTransferPayload;
        if (payload.sourceWindowLabel === currentWindowLabel) {
          return;
        }

        await processTransferPayloadRef.current(payload);
      } catch {
        // ignore polling failures
      } finally {
        pollingPendingTransferRef.current = false;
      }
    };

    const interval = window.setInterval(() => {
      void pollPendingTransfer();
    }, 750);

    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") {
        void pollPendingTransfer();
      }
    };

    window.addEventListener("focus", onVisibilityChange);
    document.addEventListener("visibilitychange", onVisibilityChange);

    return () => {
      disposed = true;
      window.clearInterval(interval);
      window.removeEventListener("focus", onVisibilityChange);
      document.removeEventListener("visibilitychange", onVisibilityChange);
    };
  }, [currentWindowLabel]);

  const createSshSession = useCallback(
    async (sessionId: string, info: SessionConnectInfo): Promise<boolean> => {
      try {
        await invoke("ssh_connect", {
          sessionId,
          settings: {
            id: sessionId,
            name: `${info.username}@${info.host}`,
            host: info.host,
            port: info.port,
            username: info.username,
            connectionProtocol: info.connectionProtocol,
            authMethod: info.authMethod,
            password: info.password,
            credentialId: info.credentialId,
            sshKeyId: info.sshKeyId,
            privateKeyPath: info.privateKeyPath,
            privateKeyPassphrase: info.privateKeyPassphrase,
            temporaryKeyContent: info.temporaryKeyContent,
            temporaryKeyExpirationMinutes: info.temporaryKeyExpirationMinutes,
            temporaryKeyPermanent: info.temporaryKeyPermanent ?? false,
            fontFamily: "JetBrains Mono",
            fontSize: 14.0,
            columns: 80,
            rows: 24,
            scrollbackLines: 10000,
            foregroundColor: "#cdd6f4",
            backgroundColor: "#11111b",
            cursorColor: "#89b4fa",
            cursorStyle: "Block",
            ansiColors: [
              "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
              "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
              "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
              "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
            ],
            sshKeepaliveEnabled: true,
            sshKeepaliveInterval: 60,
            connectionTimeout: 15,
            retryCount: 4,
            terminalLogging: false,
            commandTimestamps: false,
            promptHookEnabled,
            terminalAgentCommandName,
            terminalEffectPluginId: info.terminalEffectPluginId,
            terminalEffectAnimationSpeed: info.terminalEffectAnimationSpeed ?? 1,
            terminalEmulationType: info.terminalEmulationType,
            terminalColorsEnabled: info.terminalColorsEnabled ?? true,
            tunnels: [],
            usageCount: 0,
          },
        });
        return true;
      } catch (err) {
        console.error("SSH connect failed:", err);
        return false;
      }
    },
    [promptHookEnabled, terminalAgentCommandName],
  );
  createSshSessionRef.current = createSshSession;

  const processTransferPayload = useCallback(
    async (payload: CrossWindowTransferPayload) => {
      if (payload.sourceWindowLabel === currentWindowLabel) {
        return;
      }
      const cfg = payload.entry.config;
      if (!cfg) {
        return;
      }

      const dropKey = `${payload.sourceWindowLabel}:${payload.entry.tabId}:${payload.entry.sessionId}`;
      if (transferDropProcessedRef.current.has(dropKey)) {
        return;
      }
      transferDropProcessedRef.current.add(dropKey);
      setTimeout(() => transferDropProcessedRef.current.delete(dropKey), 5000);

      const reusedTabId = payload.entry.sessionId;
      if (tabsRef.current.some((t) => t.id === reusedTabId)) {
        setActiveTab(reusedTabId);
        if (!payload.copyMode) {
          await emitTo(payload.sourceWindowLabel, "kortty-transfer-consumed", {
            kind: payload.entry.kind,
            tabId: payload.entry.tabId,
            sessionId: payload.entry.sessionId,
            sourceWindowLabel: payload.sourceWindowLabel,
            acceptedByWindowLabel: currentWindowLabel,
          });
        }
        return;
      }

      const newTab: Tab = {
        id: reusedTabId,
        kind: "terminal",
        label: payload.entry.label,
        status: payload.entry.status,
        readOnlyMirror: !!payload.copyMode,
        host: cfg.host,
        port: cfg.port,
        username: cfg.username,
        authMethod: cfg.authMethod,
        password: cfg.password,
        credentialId: cfg.credentialId,
        sshKeyId: cfg.sshKeyId,
        privateKeyPath: cfg.privateKeyPath,
        privateKeyPassphrase: cfg.privateKeyPassphrase,
        temporaryKeyContent: cfg.temporaryKeyContent,
        temporaryKeyExpirationMinutes: cfg.temporaryKeyExpirationMinutes,
        temporaryKeyPermanent: cfg.temporaryKeyPermanent,
        connectionProtocol: cfg.connectionProtocol,
        terminalEffectPluginId: cfg.terminalEffectPluginId ?? payload.entry.terminalEffectPluginId,
        terminalEffectAnimationSpeed: cfg.terminalEffectAnimationSpeed ?? payload.entry.terminalEffectAnimationSpeed,
        themeId: payload.entry.themeId,
        fontFamily: payload.entry.fontFamily,
        fontSize: payload.entry.fontSize,
        foregroundColor: payload.entry.foregroundColor,
        backgroundColor: payload.entry.backgroundColor,
        cursorColor: payload.entry.cursorColor,
        ansiColors: copyAnsiColors(payload.entry.ansiColors),
      };
      setTabs((prev) => [...prev, newTab]);
      setActiveTab(reusedTabId);

      const splitTree = payload.splitTree;
      const splitEntries = payload.splitEntries || [];
      const splitIds = splitEntries.map((e) => e.sessionId);
      if (splitIds.length > 0) {
        setTabSplitSessions((prev) => ({ ...prev, [reusedTabId]: splitIds }));
        setSplitSessionConfigs((prev) => ({
          ...prev,
          ...Object.fromEntries(splitEntries.map((e) => [e.sessionId, e.config])),
        }));
      }
      if (splitTree) {
        setTabSplitTrees((prev) => ({ ...prev, [reusedTabId]: splitTree }));
        const identityMap: Record<string, string> = {
          [payload.entry.sessionId]: payload.entry.sessionId,
          ...Object.fromEntries(splitIds.map((sid) => [sid, sid])),
        };
        const initialTree = deserializeSplitTreeWithMapping(splitTree, identityMap);
        setTabInitialSplitTree((prev) => ({ ...prev, [reusedTabId]: initialTree }));
      }

      if (!payload.copyMode) {
        await emitTo(payload.sourceWindowLabel, "kortty-transfer-consumed", {
          kind: payload.entry.kind,
          tabId: payload.entry.tabId,
          sessionId: payload.entry.sessionId,
          sourceWindowLabel: payload.sourceWindowLabel,
          acceptedByWindowLabel: currentWindowLabel,
        });
      }
    },
    [currentWindowLabel],
  );
  processTransferPayloadRef.current = processTransferPayload;

  const handleConnect = useCallback(
    async (
      tabId: string,
      host: string,
      port: number,
      username: string,
      password: string,
      connectionProtocol: "TcpIp" | "Mosh" = "TcpIp",
      terminalEffectPluginId?: string,
      terminalEffectAnimationSpeed?: number,
      terminalEmulationType?: string,
      terminalColorsEnabled?: boolean,
    ) => {
      setTabs((prev) =>
        prev.map((t) =>
          t.id === tabId
            ? {
                ...t,
                host,
                port,
                username,
                authMethod: "Password",
                password,
                credentialId: t.credentialId,
                connectionProtocol,
                terminalEffectPluginId: terminalEffectPluginId ?? t.terminalEffectPluginId,
                terminalEffectAnimationSpeed: terminalEffectAnimationSpeed ?? t.terminalEffectAnimationSpeed,
                status: "connecting" as const,
              }
            : t,
        ),
      );
      const ok = await createSshSession(tabId, {
        host,
        port,
        username,
        authMethod: "Password",
        password,
        credentialId: undefined,
        connectionProtocol,
        terminalEffectPluginId,
        terminalEffectAnimationSpeed,
        terminalEmulationType,
        terminalColorsEnabled,
      });
      if (ok) {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId
              ? { ...t, label: `${username}@${host}`, status: "connected" as const }
              : t,
          ),
        );
      } else {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId ? { ...t, status: "disconnected" as const } : t,
          ),
        );
      }
    },
    [createSshSession],
  );

  const connectFromSettings = useCallback(
    async (conn: ConnectionSettings) => {
      const latestConnection =
        useConnectionStore.getState().connections.find((connection) => connection.id === conn.id) ?? conn;
      if (splitResolveRef.current && splitTabRef.current) {
        const tabId = splitTabRef.current;
        const splitSessionId = crypto.randomUUID();
        const ok = await createSshSession(splitSessionId, {
          host: latestConnection.host,
          port: latestConnection.port,
          username: latestConnection.username,
          authMethod: latestConnection.authMethod,
          password: latestConnection.password,
          credentialId: latestConnection.credentialId,
          sshKeyId: latestConnection.sshKeyId,
          privateKeyPath: latestConnection.privateKeyPath,
          privateKeyPassphrase: latestConnection.privateKeyPassphrase,
          temporaryKeyContent: latestConnection.temporaryKeyContent,
          temporaryKeyExpirationMinutes: latestConnection.temporaryKeyExpirationMinutes,
          temporaryKeyPermanent: latestConnection.temporaryKeyPermanent,
          connectionProtocol: latestConnection.connectionProtocol || "TcpIp",
          terminalEffectPluginId: latestConnection.terminalEffectPluginId,
          terminalEffectAnimationSpeed: latestConnection.terminalEffectAnimationSpeed,
          terminalEmulationType: latestConnection.terminalEmulationType,
          terminalColorsEnabled: latestConnection.terminalColorsEnabled ?? true,
        });
        if (ok) {
          setTabSplitSessions((prev) => ({
            ...prev,
            [tabId]: [...(prev[tabId] || []), splitSessionId],
          }));
          setSplitSessionConfigs((prev) => ({
            ...prev,
            [splitSessionId]: {
              host: latestConnection.host,
              port: latestConnection.port,
              username: latestConnection.username,
              authMethod: latestConnection.authMethod,
              password: latestConnection.password,
              credentialId: latestConnection.credentialId,
              sshKeyId: latestConnection.sshKeyId,
              privateKeyPath: latestConnection.privateKeyPath,
              privateKeyPassphrase: latestConnection.privateKeyPassphrase,
              temporaryKeyContent: latestConnection.temporaryKeyContent,
              temporaryKeyExpirationMinutes: latestConnection.temporaryKeyExpirationMinutes,
              temporaryKeyPermanent: latestConnection.temporaryKeyPermanent,
              connectionProtocol: latestConnection.connectionProtocol || "TcpIp",
              terminalEffectPluginId: latestConnection.terminalEffectPluginId,
              terminalEffectAnimationSpeed: latestConnection.terminalEffectAnimationSpeed,
              terminalEmulationType: latestConnection.terminalEmulationType,
              terminalColorsEnabled: latestConnection.terminalColorsEnabled ?? true,
              aiProfileId: latestConnection.aiProfileId,
              aiSkillIds: latestConnection.aiSkillIds,
            },
          }));
          splitResolveRef.current(splitSessionId);
        } else {
          splitResolveRef.current(null);
        }
        splitResolveRef.current = null;
        splitTabRef.current = null;
        setOpenDialog(null);
        return;
      }
      const id = crypto.randomUUID();
      const newTab: Tab = {
        id,
        kind: "terminal",
        label: latestConnection.name || `${latestConnection.username}@${latestConnection.host}`,
        status: "disconnected",
        host: latestConnection.host,
        port: latestConnection.port,
        username: latestConnection.username,
        connectionId: latestConnection.id,
        authMethod: latestConnection.authMethod,
        password: latestConnection.password,
        credentialId: latestConnection.credentialId,
        sshKeyId: latestConnection.sshKeyId,
        privateKeyPath: latestConnection.privateKeyPath,
        privateKeyPassphrase: latestConnection.privateKeyPassphrase,
        temporaryKeyContent: latestConnection.temporaryKeyContent,
        temporaryKeyExpirationMinutes: latestConnection.temporaryKeyExpirationMinutes,
        temporaryKeyPermanent: latestConnection.temporaryKeyPermanent,
        connectionProtocol: latestConnection.connectionProtocol || "TcpIp",
        terminalEffectPluginId: latestConnection.terminalEffectPluginId,
        terminalEffectAnimationSpeed: latestConnection.terminalEffectAnimationSpeed,
        ...buildTerminalAppearanceSnapshot(latestConnection),
      };
      setTabs((prev) => [...prev, newTab]);
      setActiveTab(id);
      setOpenDialog(null);
      if (latestConnection.authMethod === "Password") {
        handleConnect(
          id,
          latestConnection.host,
          latestConnection.port,
          latestConnection.username,
          latestConnection.password || "",
          latestConnection.connectionProtocol || "TcpIp",
          latestConnection.terminalEffectPluginId,
          latestConnection.terminalEffectAnimationSpeed,
          latestConnection.terminalEmulationType,
          latestConnection.terminalColorsEnabled ?? true,
        );
      } else {
        createSshSession(id, {
          host: latestConnection.host,
          port: latestConnection.port,
          username: latestConnection.username,
          authMethod: latestConnection.authMethod,
          password: latestConnection.password,
          credentialId: latestConnection.credentialId,
          sshKeyId: latestConnection.sshKeyId,
          privateKeyPath: latestConnection.privateKeyPath,
          privateKeyPassphrase: latestConnection.privateKeyPassphrase,
          temporaryKeyContent: latestConnection.temporaryKeyContent,
          temporaryKeyExpirationMinutes: latestConnection.temporaryKeyExpirationMinutes,
          temporaryKeyPermanent: latestConnection.temporaryKeyPermanent,
          connectionProtocol: latestConnection.connectionProtocol || "TcpIp",
          terminalEffectPluginId: latestConnection.terminalEffectPluginId,
          terminalEffectAnimationSpeed: latestConnection.terminalEffectAnimationSpeed,
          terminalEmulationType: latestConnection.terminalEmulationType,
          terminalColorsEnabled: latestConnection.terminalColorsEnabled ?? true,
        }).then((ok) => {
          setTabs((prev) =>
            prev.map((t) =>
              t.id === id
                ? { ...t, status: ok ? ("connected" as const) : ("disconnected" as const) }
                : t,
            ),
          );
        });
      }
    },
    [handleConnect, createSshSession],
  );

  const buildProjectSnapshot = useCallback(
    (base?: Project | null): Project => ({
      name: base?.name || currentProject?.name || "KorTTY Project",
      description: base?.description || currentProject?.description,
      filePath: base?.filePath || currentProject?.filePath,
      connectionIds: Array.from(
        new Set(
          tabs
            .map((tab) => tab.connectionId)
            .filter((connectionId): connectionId is string => !!connectionId),
        ),
      ),
      dashboardOpen: showDashboard,
      autoReconnect: base?.autoReconnect ?? currentProject?.autoReconnect ?? true,
      createdAt: base?.createdAt || currentProject?.createdAt,
      lastModified: base?.lastModified || currentProject?.lastModified,
    }),
    [currentProject, showDashboard, tabs],
  );

  const clearWorkspaceForProject = useCallback(async () => {
    const currentTabs = tabsRef.current;
    const currentSplitSessions = splitSessionsRef.current;

    for (const tab of currentTabs) {
      if (tab.status === "connected") {
        try {
          await invoke("ssh_disconnect", { sessionId: tab.id });
        } catch (error) {
          console.error("Project switch disconnect failed:", error);
        }
      }
      for (const splitId of currentSplitSessions[tab.id] || []) {
        try {
          await invoke("ssh_disconnect", { sessionId: splitId });
        } catch (error) {
          console.error("Project switch split disconnect failed:", error);
        }
      }
    }

    setTabs([]);
    setActiveTab(null);
    setTabSplitSessions({});
    setSplitSessionConfigs({});
    setTabSplitTrees({});
    setTabInitialSplitTree({});
  }, []);

  const saveProjectToPath = useCallback(
    async (draft: Project, forcePathPicker: boolean) => {
      let path = draft.filePath;
      if (forcePathPicker || !path) {
        const selectedPath = await saveFileDialog({
          defaultPath: draft.filePath || `${draft.name || "kortty-project"}.json`,
          filters: [
            { name: "KorTTY Project", extensions: ["json"] },
            { name: "All files", extensions: ["*"] },
          ],
        });
        if (!selectedPath || typeof selectedPath !== "string") {
          return;
        }
        path = selectedPath;
      }

      const saved = await invoke<Project>("save_project", {
        project: { ...buildProjectSnapshot(draft), filePath: path },
        path,
      });
      setCurrentProject(saved);
      addRecentProject(path);
      const refreshed = await invoke<string[]>("get_recent_projects");
      setRecentProjects(refreshed);
    },
    [addRecentProject, buildProjectSnapshot, setCurrentProject, setRecentProjects],
  );

  const applyProjectToWorkspace = useCallback(
    async (project: Project, autoReconnect: boolean) => {
      await clearWorkspaceForProject();
      await loadConnections();

      const allConnections = useConnectionStore.getState().connections;
      const matchingConnections = project.connectionIds
        .map((connectionId) => allConnections.find((connection) => connection.id === connectionId))
        .filter((connection): connection is ConnectionSettings => !!connection);

      const restoredTabs: Tab[] = matchingConnections.map((connection) => ({
        id: crypto.randomUUID(),
        kind: "terminal",
        label: connection.name || `${connection.username}@${connection.host}`,
        status: autoReconnect ? "connecting" : "disconnected",
        host: connection.host,
        port: connection.port,
        username: connection.username,
        connectionId: connection.id,
        authMethod: connection.authMethod,
        password: connection.password,
        credentialId: connection.credentialId,
        sshKeyId: connection.sshKeyId,
        privateKeyPath: connection.privateKeyPath,
        privateKeyPassphrase: connection.privateKeyPassphrase,
        temporaryKeyContent: connection.temporaryKeyContent,
        temporaryKeyExpirationMinutes: connection.temporaryKeyExpirationMinutes,
        temporaryKeyPermanent: connection.temporaryKeyPermanent,
        connectionProtocol: connection.connectionProtocol || "TcpIp",
        terminalEffectPluginId: connection.terminalEffectPluginId,
        terminalEffectAnimationSpeed: connection.terminalEffectAnimationSpeed,
        ...buildTerminalAppearanceSnapshot(connection),
      }));

      setTabs(restoredTabs);
      setActiveTab(restoredTabs[0]?.id ?? null);
      setShowDashboard(project.dashboardOpen);

      const resolvedProject: Project = {
        ...project,
        autoReconnect,
      };
      setCurrentProject(resolvedProject);
      if (resolvedProject.filePath) {
        addRecentProject(resolvedProject.filePath);
        const refreshed = await invoke<string[]>("get_recent_projects");
        setRecentProjects(refreshed);
      }

      if (!autoReconnect) {
        return;
      }

      for (const [index, connection] of matchingConnections.entries()) {
        const tabId = restoredTabs[index]?.id;
        if (!tabId) continue;

        if (connection.authMethod === "Password") {
          void handleConnect(
            tabId,
            connection.host,
            connection.port,
            connection.username,
            connection.password || "",
            connection.connectionProtocol || "TcpIp",
            connection.terminalEffectPluginId,
            connection.terminalEffectAnimationSpeed,
          );
          continue;
        }

        const ok = await createSshSession(tabId, {
          host: connection.host,
          port: connection.port,
          username: connection.username,
          authMethod: connection.authMethod,
          password: connection.password,
          credentialId: connection.credentialId,
          sshKeyId: connection.sshKeyId,
          privateKeyPath: connection.privateKeyPath,
          privateKeyPassphrase: connection.privateKeyPassphrase,
          temporaryKeyContent: connection.temporaryKeyContent,
          temporaryKeyExpirationMinutes: connection.temporaryKeyExpirationMinutes,
          temporaryKeyPermanent: connection.temporaryKeyPermanent,
          connectionProtocol: connection.connectionProtocol || "TcpIp",
          terminalEffectPluginId: connection.terminalEffectPluginId,
          terminalEffectAnimationSpeed: connection.terminalEffectAnimationSpeed,
        });
        setTabs((previous) =>
          previous.map((tab) =>
            tab.id === tabId
              ? { ...tab, status: ok ? ("connected" as const) : ("disconnected" as const) }
              : tab,
          ),
        );
      }
    },
    [
      addRecentProject,
      clearWorkspaceForProject,
      createSshSession,
      handleConnect,
      loadConnections,
      setCurrentProject,
      setRecentProjects,
    ],
  );

  const handleDisconnect = useCallback(async (tabId: string) => {
    try {
      await invoke("ssh_disconnect", { sessionId: tabId });
    } catch (err) {
      console.error("Disconnect failed:", err);
    }
    setTabs((prev) =>
      prev.map((t) =>
        t.id === tabId ? { ...t, status: "disconnected" as const } : t,
      ),
    );
  }, []);

  const handleReconnect = useCallback(
    async (sessionId: string | null) => {
      if (!sessionId) return;

      const primaryTab = tabs.find((t) => t.id === sessionId);
      let info: SessionConnectInfo | null = null;

      if (primaryTab?.host && primaryTab?.username) {
        info = {
          host: primaryTab.host,
          port: primaryTab.port || 22,
          username: primaryTab.username,
          authMethod: primaryTab.authMethod || "Password",
          password: primaryTab.password,
          credentialId: primaryTab.credentialId,
          sshKeyId: primaryTab.sshKeyId,
          privateKeyPath: primaryTab.privateKeyPath,
          privateKeyPassphrase: primaryTab.privateKeyPassphrase,
          temporaryKeyContent: primaryTab.temporaryKeyContent,
          temporaryKeyExpirationMinutes: primaryTab.temporaryKeyExpirationMinutes,
          temporaryKeyPermanent: primaryTab.temporaryKeyPermanent,
          connectionProtocol: primaryTab.connectionProtocol || "TcpIp",
          terminalEffectPluginId: primaryTab.terminalEffectPluginId,
          terminalEffectAnimationSpeed: primaryTab.terminalEffectAnimationSpeed,
        };
      } else {
        info = splitSessionConfigs[sessionId] || null;
      }

      if (!info) return;

      try {
        await invoke("ssh_disconnect", { sessionId });
      } catch (err) {
        console.error("Reconnect disconnect failed:", err);
      }

      await new Promise((r) => setTimeout(r, 200));
      const ok = await createSshSession(sessionId, info);
      if (!ok) {
        console.error(`Reconnect failed for session ${sessionId}`);
      }
    },
    [tabs, splitSessionConfigs, createSshSession],
  );

  const handleReconnectTabAll = useCallback(
    async (tabId: string | null) => {
      if (!tabId) return;
      await handleReconnect(tabId);
      const splitIds = tabSplitSessions[tabId] || [];
      for (const splitId of splitIds) {
        await handleReconnect(splitId);
      }
    },
    [handleReconnect, tabSplitSessions],
  );

  const addTab = useCallback(() => {
    const id = crypto.randomUUID();
    const newTab: Tab = { id, kind: "terminal", label: "New Connection", status: "disconnected" };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
  }, []);

  const duplicateTab = useCallback(
    (tabId: string) => {
      const source = tabs.find((t) => t.id === tabId);
      if (!source || (source.kind ?? "terminal") === "ai") return;
      const id = crypto.randomUUID();
      const newTab: Tab = {
        id,
        kind: "terminal",
        label: source.label,
        status: "disconnected",
        host: source.host,
        port: source.port,
        username: source.username,
        connectionId: source.connectionId,
        authMethod: source.authMethod,
        password: source.password,
        credentialId: source.credentialId,
        sshKeyId: source.sshKeyId,
        privateKeyPath: source.privateKeyPath,
        privateKeyPassphrase: source.privateKeyPassphrase,
        temporaryKeyContent: source.temporaryKeyContent,
        temporaryKeyExpirationMinutes: source.temporaryKeyExpirationMinutes,
        temporaryKeyPermanent: source.temporaryKeyPermanent,
        connectionProtocol: source.connectionProtocol,
        terminalEffectPluginId: source.terminalEffectPluginId,
        terminalEffectAnimationSpeed: source.terminalEffectAnimationSpeed,
        ...buildTerminalAppearanceFromTab(source),
      };
      setTabs((prev) => [...prev, newTab]);
      setActiveTab(id);
      if (source.host && source.username) {
        if ((source.authMethod || "Password") === "Password") {
          handleConnect(
            id,
            source.host,
            source.port || 22,
            source.username,
            source.password || "",
            source.connectionProtocol || "TcpIp",
            source.terminalEffectPluginId,
            source.terminalEffectAnimationSpeed,
          );
        } else {
          createSshSession(id, {
            host: source.host,
            port: source.port || 22,
            username: source.username,
            authMethod: source.authMethod || "PrivateKey",
            password: source.password,
            credentialId: source.credentialId,
            sshKeyId: source.sshKeyId,
            privateKeyPath: source.privateKeyPath,
            privateKeyPassphrase: source.privateKeyPassphrase,
            temporaryKeyContent: source.temporaryKeyContent,
            temporaryKeyExpirationMinutes: source.temporaryKeyExpirationMinutes,
            temporaryKeyPermanent: source.temporaryKeyPermanent,
            connectionProtocol: source.connectionProtocol || "TcpIp",
            terminalEffectPluginId: source.terminalEffectPluginId,
            terminalEffectAnimationSpeed: source.terminalEffectAnimationSpeed,
          }).then((ok) => {
            setTabs((prev) =>
              prev.map((t) =>
                t.id === id
                  ? { ...t, status: ok ? ("connected" as const) : ("disconnected" as const) }
                  : t,
              ),
            );
          });
        }
      }
    },
    [tabs, handleConnect, createSshSession],
  );

  const handleSplitSameServer = useCallback(
    async (tabId: string): Promise<string | null> => {
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab?.host || !tab?.username) return null;
      const host = tab.host;
      const username = tab.username;
      const port = tab.port || 22;
      const authMethod = tab.authMethod || "Password";
      const password = tab.password;
      const credentialId = tab.credentialId;
      const sshKeyId = tab.sshKeyId;
      const privateKeyPath = tab.privateKeyPath;
      const privateKeyPassphrase = tab.privateKeyPassphrase;
      const temporaryKeyContent = tab.temporaryKeyContent;
      const temporaryKeyExpirationMinutes = tab.temporaryKeyExpirationMinutes;
      const temporaryKeyPermanent = tab.temporaryKeyPermanent;
      const connectionProtocol = tab.connectionProtocol || "TcpIp";
      const terminalEffectPluginId = tab.terminalEffectPluginId;
      const terminalEffectAnimationSpeed = tab.terminalEffectAnimationSpeed;
      const splitSessionId = crypto.randomUUID();
      const ok = await createSshSession(splitSessionId, {
        host,
        port,
        username,
        authMethod,
        password,
        credentialId,
        sshKeyId,
        privateKeyPath,
        privateKeyPassphrase,
        temporaryKeyContent,
        temporaryKeyExpirationMinutes,
        temporaryKeyPermanent,
        connectionProtocol,
        terminalEffectPluginId,
        terminalEffectAnimationSpeed,
      });
      if (ok) {
        setTabSplitSessions((prev) => ({
          ...prev,
          [tabId]: [...(prev[tabId] || []), splitSessionId],
        }));
        setSplitSessionConfigs((prev) => ({
          ...prev,
          [splitSessionId]: {
            host,
            port,
            username,
              authMethod,
            password,
              credentialId,
              sshKeyId,
              privateKeyPath,
              privateKeyPassphrase,
              temporaryKeyContent,
              temporaryKeyExpirationMinutes,
              temporaryKeyPermanent,
            connectionProtocol,
            terminalEffectPluginId,
            terminalEffectAnimationSpeed,
          },
        }));
        return splitSessionId;
      }
      return null;
    },
    [tabs, createSshSession],
  );

  const handleSplitNewServer = useCallback(
    (tabId: string): Promise<string | null> => {
      return new Promise((resolve) => {
        splitResolveRef.current = resolve;
        splitTabRef.current = tabId;
        setOpenDialog("connectionManager");
      });
    },
    [],
  );

  const handleDisconnectSplitSession = useCallback(
    (tabId: string, sessionId: string) => {
      handleDisconnect(sessionId);
      setTabSplitSessions((prev) => ({
        ...prev,
        [tabId]: (prev[tabId] || []).filter((id) => id !== sessionId),
      }));
      setSplitSessionConfigs((prev) => {
        const next = { ...prev };
        delete next[sessionId];
        return next;
      });
    },
    [handleDisconnect],
  );

  const reorderTabs = useCallback((draggedId: string, targetId: string) => {
    setTabs((prev) => {
      const draggedIdx = prev.findIndex((t) => t.id === draggedId);
      const targetIdx = prev.findIndex((t) => t.id === targetId);
      if (draggedIdx < 0 || targetIdx < 0) return prev;
      const result = [...prev];
      const [dragged] = result.splice(draggedIdx, 1);
      result.splice(targetIdx, 0, dragged);
      return result;
    });
  }, []);

  const closeTab = useCallback(
    (id: string) => {
      const splitSessions = tabSplitSessions[id] || [];
      const tab = tabs.find((t) => t.id === id);
      if (tab?.status === "connected") {
        handleDisconnect(id);
      }
      for (const splitId of splitSessions) {
        handleDisconnect(splitId);
      }
      setTabSplitSessions((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      setSplitSessionConfigs((prev) => {
        const next = { ...prev };
        for (const splitId of splitSessions) {
          delete next[splitId];
        }
        return next;
      });
      setTabs((prev) => {
        const remaining = prev.filter((t) => t.id !== id);
        if (activeTab === id) {
          setActiveTab(remaining.length > 0 ? remaining[remaining.length - 1].id : null);
        }
        return remaining;
      });
    },
    [tabs, activeTab, handleDisconnect, tabSplitSessions],
  );

  const handleClosePrimarySplit = useCallback(
    async (tabId: string) => {
      const splitSessions = tabSplitSessions[tabId] || [];
      if (splitSessions.length === 0) {
        closeTab(tabId);
        return;
      }

      const currentTree = tabSplitTrees[tabId];
      const remainingTree = currentTree
        ? removeSessionFromTransferTree(currentTree, tabId)
        : null;
      const promotedSessionId = remainingTree
        ? getLeavesInOrder(remainingTree)[0]
        : splitSessions[0];

      if (!promotedSessionId) {
        closeTab(tabId);
        return;
      }

      const promotedConfig = splitSessionConfigs[promotedSessionId];
      if (!promotedConfig) {
        handleDisconnectSplitSession(tabId, promotedSessionId);
        return;
      }

      try {
        await invoke("ssh_disconnect", { sessionId: tabId });
      } catch (error) {
        console.error("Primary split disconnect failed:", error);
      }

      const remainingSplitIds = splitSessions.filter((sessionId) => sessionId !== promotedSessionId);
      const nextTransferTree =
        remainingTree ??
        (remainingSplitIds.length === 0
          ? { type: "leaf" as const, sessionId: promotedSessionId }
          : {
              type: "container" as const,
              direction: "horizontal" as const,
              children: [
                { type: "leaf" as const, sessionId: promotedSessionId },
                ...remainingSplitIds.map((sessionId) => ({ type: "leaf" as const, sessionId })),
              ],
            });

      setTabs((prev) =>
        prev.map((tab) =>
          tab.id === tabId
            ? {
                ...tab,
                id: promotedSessionId,
                label: `${promotedConfig.username}@${promotedConfig.host}`,
                status: "connected",
                host: promotedConfig.host,
                port: promotedConfig.port,
                username: promotedConfig.username,
                authMethod: promotedConfig.authMethod,
                password: promotedConfig.password,
                credentialId: promotedConfig.credentialId,
                sshKeyId: promotedConfig.sshKeyId,
                privateKeyPath: promotedConfig.privateKeyPath,
                privateKeyPassphrase: promotedConfig.privateKeyPassphrase,
                temporaryKeyContent: promotedConfig.temporaryKeyContent,
                temporaryKeyExpirationMinutes: promotedConfig.temporaryKeyExpirationMinutes,
                temporaryKeyPermanent: promotedConfig.temporaryKeyPermanent,
                connectionProtocol: promotedConfig.connectionProtocol,
              }
            : tab,
        ),
      );

      if (activeTab === tabId) {
        setActiveTab(promotedSessionId);
      }
      focusedPaneSessionRef.current = promotedSessionId;

      setTabSplitSessions((prev) => {
        const next = { ...prev };
        delete next[tabId];
        next[promotedSessionId] = remainingSplitIds;
        return next;
      });

      setSplitSessionConfigs((prev) => {
        const next = { ...prev };
        delete next[promotedSessionId];
        return next;
      });

      setTabSplitTrees((prev) => {
        const next = { ...prev };
        delete next[tabId];
        next[promotedSessionId] = nextTransferTree;
        return next;
      });

      setTabInitialSplitTree((prev) => {
        const next = { ...prev };
        delete next[tabId];
        next[promotedSessionId] = buildInitialSplitTree(nextTransferTree);
        return next;
      });

      setTabFontSizes((prev) => {
        const next = { ...prev };
        if (next[tabId] != null) {
          next[promotedSessionId] = next[tabId];
          delete next[tabId];
        }
        return next;
      });

      setPaneFontSizes((prev) => {
        const next: Record<string, number> = {};
        for (const [key, value] of Object.entries(prev)) {
          if (key === `${tabId}:${promotedSessionId}`) {
            continue;
          }
          if (key.startsWith(`${tabId}:`)) {
            next[`${promotedSessionId}:${key.slice(tabId.length + 1)}`] = value;
          } else {
            next[key] = value;
          }
        }
        return next;
      });
    },
    [
      activeTab,
      closeTab,
      handleDisconnectSplitSession,
      splitSessionConfigs,
      tabSplitSessions,
      tabSplitTrees,
    ],
  );

  const nextTab = useCallback(() => {
    if (tabs.length < 2) return;
    const idx = tabs.findIndex((t) => t.id === activeTab);
    const nextIdx = (idx + 1) % tabs.length;
    setActiveTab(tabs[nextIdx].id);
  }, [tabs, activeTab]);

  const prevTab = useCallback(() => {
    if (tabs.length < 2) return;
    const idx = tabs.findIndex((t) => t.id === activeTab);
    const prevIdx = (idx - 1 + tabs.length) % tabs.length;
    setActiveTab(tabs[prevIdx].id);
  }, [tabs, activeTab]);

  const handleQuickConnect = useCallback(
    (info: SessionConnectInfo) => {
      const id = crypto.randomUUID();
      const newTab: Tab = {
        id,
        kind: "terminal",
        label: `${info.username}@${info.host}`,
        status: "disconnected",
        host: info.host,
        port: info.port,
        username: info.username,
        authMethod: info.authMethod,
        password: info.password,
        credentialId: info.credentialId,
        sshKeyId: info.sshKeyId,
        privateKeyPath: info.privateKeyPath,
        privateKeyPassphrase: info.privateKeyPassphrase,
        temporaryKeyContent: info.temporaryKeyContent,
        temporaryKeyExpirationMinutes: info.temporaryKeyExpirationMinutes,
        temporaryKeyPermanent: info.temporaryKeyPermanent,
        connectionProtocol: info.connectionProtocol,
        terminalEffectPluginId: info.terminalEffectPluginId,
        terminalEffectAnimationSpeed: info.terminalEffectAnimationSpeed,
        aiProfileId: info.aiProfileId,
        aiSkillIds: info.aiSkillIds,
      };
      setTabs((prev) => [...prev, newTab]);
      setActiveTab(id);
      if (info.authMethod === "Password") {
        handleConnect(
          id,
          info.host,
          info.port,
          info.username,
          info.password || "",
          info.connectionProtocol,
          info.terminalEffectPluginId,
          info.terminalEffectAnimationSpeed,
          info.terminalEmulationType,
          info.terminalColorsEnabled,
        );
      } else {
        createSshSession(id, info).then((ok) => {
          setTabs((prev) =>
            prev.map((t) =>
              t.id === id
                ? { ...t, status: ok ? ("connected" as const) : ("disconnected" as const) }
                : t,
            ),
          );
        });
      }
    },
    [handleConnect, createSshSession],
  );

  const createAdditionalWindow = useCallback(async () => {
    const label = `window-${crypto.randomUUID()}`;
    const name = `Window ${label.slice(-4)}`;
    setWorkspaceWindows((prev) => ({
      ...prev,
      [label]: {
        label,
        name,
        updatedAt: Date.now(),
        connections: [],
      },
    }));
    try {
      await invoke("create_workspace_window", {
        label,
        title: `KorTTY - ${name}`,
      });
      const webview = await WebviewWindow.getByLabel(label);
      if (webview) {
        await webview.setFocus();
        emitTo(label, "kortty-set-window-name", { name }).catch(console.error);
      }
    } catch (err) {
      console.error("Failed to create window:", err);
    }
  }, []);

  const focusWorkspaceWindow = useCallback(async (label: string) => {
    if (label === currentWindowLabel) return;
    const win = await WebviewWindow.getByLabel(label);
    if (win) {
      await win.setFocus();
    }
  }, [currentWindowLabel]);

  const findWindowLabelAtScreenPoint = useCallback(
    async (screenX: number, screenY: number) => {
      if (!Number.isFinite(screenX) || !Number.isFinite(screenY)) {
        return null;
      }

      const windows = await getAllWebviewWindows();
      for (const win of windows) {
        if (win.label === currentWindowLabel) {
          continue;
        }

        try {
          const [position, size, scaleFactor] = await Promise.all([
            win.outerPosition(),
            win.outerSize(),
            win.scaleFactor(),
          ]);
          const logicalPosition = position.toLogical(scaleFactor);
          const logicalSize = size.toLogical(scaleFactor);
          const isWithinHorizontalBounds =
            screenX >= logicalPosition.x &&
            screenX <= logicalPosition.x + logicalSize.width;
          const isWithinVerticalBounds =
            screenY >= logicalPosition.y &&
            screenY <= logicalPosition.y + logicalSize.height;

          if (isWithinHorizontalBounds && isWithinVerticalBounds) {
            return win.label;
          }
        } catch (error) {
          console.error("Failed to inspect target window geometry:", error);
        }
      }

      return null;
    },
    [currentWindowLabel],
  );

  const handleDashboardDragStart = useCallback(
    (
      entry: DashboardConnectionEntry,
      e: React.DragEvent<HTMLDivElement>,
      onStarted?: () => void,
      extraSplitEntries?: SplitTransferEntry[],
    ) => {
      if (!e.altKey || !e.shiftKey) {
        e.preventDefault();
        return;
      }
      const payload: CrossWindowTransferPayload = {
        sourceWindowLabel: currentWindowLabel,
        entry,
        splitEntries:
          extraSplitEntries && extraSplitEntries.length > 0 ? extraSplitEntries : undefined,
      };
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("application/x-kortty-transfer", JSON.stringify(payload));
      e.dataTransfer.setData("text/plain", `${entry.label}`);
      onStarted?.();
    },
    [currentWindowLabel],
  );

  const handleTabTransferDragStart = useCallback(
    (tab: Tab, e: React.DragEvent<HTMLDivElement>) => {
      if ((tab.kind ?? "terminal") === "ai") {
        return;
      }
      const config: SessionConnectInfo | undefined =
        tab.host && tab.username
          ? {
              host: tab.host,
              port: tab.port || 22,
              username: tab.username,
              authMethod: tab.authMethod || "Password",
              password: tab.password,
              credentialId: tab.credentialId,
              sshKeyId: tab.sshKeyId,
              privateKeyPath: tab.privateKeyPath,
              privateKeyPassphrase: tab.privateKeyPassphrase,
              temporaryKeyContent: tab.temporaryKeyContent,
              temporaryKeyExpirationMinutes: tab.temporaryKeyExpirationMinutes,
              temporaryKeyPermanent: tab.temporaryKeyPermanent,
              connectionProtocol: tab.connectionProtocol || "TcpIp",
            }
          : undefined;
      const entry: DashboardConnectionEntry = {
        kind: "tab",
        sessionId: tab.id,
        tabId: tab.id,
        label: tab.label,
        status: tab.status,
        config,
        ...buildTerminalAppearanceFromTab(tab),
      };
      const splitTree = tabSplitTrees[tab.id];
      const order = splitTree ? getLeavesInOrder(splitTree) : [tab.id, ...(tabSplitSessions[tab.id] || [])];
      const splitEntries: SplitTransferEntry[] = order
        .slice(1)
        .map((sessionId) => {
          const splitCfg = splitSessionConfigs[sessionId];
          return splitCfg ? { sessionId, config: splitCfg } : null;
        })
        .filter((e): e is SplitTransferEntry => e != null);
      const payload: CrossWindowTransferPayload = {
        sourceWindowLabel: currentWindowLabel,
        entry,
        splitEntries: splitEntries.length > 0 ? splitEntries : undefined,
        splitTree: splitTree ?? undefined,
      };
      activeTabTransferRef.current = {
        tabId: tab.id,
        payload,
      };
      const transferToken = storeCrossWindowTransferPayload(payload);
      e.dataTransfer.effectAllowed = "copyMove";
      e.dataTransfer.setData(CROSS_WINDOW_TRANSFER_MIME, JSON.stringify(payload));
      e.dataTransfer.setData(CROSS_WINDOW_TRANSFER_URI_MIME, transferToken);
      e.dataTransfer.setData("text/plain", transferToken);
    },
    [currentWindowLabel, tabSplitSessions, splitSessionConfigs, tabSplitTrees],
  );

  const handleTabTransferDragEnd = useCallback(
    async (tab: Tab, e: React.DragEvent<HTMLDivElement>) => {
      setDragOverWindowLabel(null);

      const activeTransfer = activeTabTransferRef.current;
      activeTabTransferRef.current = null;

      if (!activeTransfer || activeTransfer.tabId !== tab.id) {
        return;
      }

      const targetWindowLabel = await findWindowLabelAtScreenPoint(e.screenX, e.screenY);
      if (!targetWindowLabel) {
        return;
      }

      await emitTransferWithRetry(targetWindowLabel, {
        ...activeTransfer.payload,
        copyMode: e.altKey,
      });
    },
    [findWindowLabelAtScreenPoint],
  );

  const handleMoveTabToWindow = useCallback(
    async (tabId: string, targetWindowLabel: string) => {
      if (targetWindowLabel === currentWindowLabel) return;
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab || (tab.kind ?? "terminal") === "ai") return;
      const config: SessionConnectInfo | undefined =
        tab.host && tab.username
          ? {
              host: tab.host,
              port: tab.port || 22,
              username: tab.username,
              authMethod: tab.authMethod || "Password",
              password: tab.password,
              credentialId: tab.credentialId,
              sshKeyId: tab.sshKeyId,
              privateKeyPath: tab.privateKeyPath,
              privateKeyPassphrase: tab.privateKeyPassphrase,
              temporaryKeyContent: tab.temporaryKeyContent,
              temporaryKeyExpirationMinutes: tab.temporaryKeyExpirationMinutes,
              temporaryKeyPermanent: tab.temporaryKeyPermanent,
              connectionProtocol: tab.connectionProtocol || "TcpIp",
            }
          : undefined;
      const entry: DashboardConnectionEntry = {
        kind: "tab",
        sessionId: tab.id,
        tabId: tab.id,
        label: tab.label,
        status: tab.status,
        config,
        ...buildTerminalAppearanceFromTab(tab),
      };
      if (!config) return;
      const splitTree = tabSplitTrees[tabId];
      const order = splitTree ? getLeavesInOrder(splitTree) : [tabId, ...(tabSplitSessions[tabId] || [])];
      const splitEntries: SplitTransferEntry[] = order
        .slice(1)
        .map((sessionId) => {
          const splitCfg = splitSessionConfigs[sessionId];
          return splitCfg ? { sessionId, config: splitCfg } : null;
        })
        .filter((e): e is SplitTransferEntry => e != null);
      const payload: CrossWindowTransferPayload = {
        sourceWindowLabel: currentWindowLabel,
        entry,
        splitEntries: splitEntries.length > 0 ? splitEntries : undefined,
        splitTree: splitTree ?? undefined,
      };
      await emitTransferWithRetry(targetWindowLabel, payload);
    },
    [currentWindowLabel, tabs, tabSplitSessions, splitSessionConfigs, tabSplitTrees],
  );

  const handleCopyTabToWindow = useCallback(
    async (tabId: string, targetWindowLabel: string) => {
      if (targetWindowLabel === currentWindowLabel) return;
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab || (tab.kind ?? "terminal") === "ai") return;
      const config: SessionConnectInfo | undefined =
        tab.host && tab.username
          ? {
              host: tab.host,
              port: tab.port || 22,
              username: tab.username,
              authMethod: tab.authMethod || "Password",
              password: tab.password,
              credentialId: tab.credentialId,
              sshKeyId: tab.sshKeyId,
              privateKeyPath: tab.privateKeyPath,
              privateKeyPassphrase: tab.privateKeyPassphrase,
              temporaryKeyContent: tab.temporaryKeyContent,
              temporaryKeyExpirationMinutes: tab.temporaryKeyExpirationMinutes,
              temporaryKeyPermanent: tab.temporaryKeyPermanent,
              connectionProtocol: tab.connectionProtocol || "TcpIp",
            }
          : undefined;
      const entry: DashboardConnectionEntry = {
        kind: "tab",
        sessionId: tab.id,
        tabId: tab.id,
        label: tab.label,
        status: tab.status,
        config,
        ...buildTerminalAppearanceFromTab(tab),
      };
      if (!config) return;
      const splitTree = tabSplitTrees[tabId];
      const order = splitTree ? getLeavesInOrder(splitTree) : [tabId, ...(tabSplitSessions[tabId] || [])];
      const splitEntries: SplitTransferEntry[] = order
        .slice(1)
        .map((sessionId) => {
          const splitCfg = splitSessionConfigs[sessionId];
          return splitCfg ? { sessionId, config: splitCfg } : null;
        })
        .filter((e): e is SplitTransferEntry => e != null);
      const payload: CrossWindowTransferPayload = {
        sourceWindowLabel: currentWindowLabel,
        entry,
        splitEntries: splitEntries.length > 0 ? splitEntries : undefined,
        splitTree: splitTree ?? undefined,
        copyMode: true,
      };
      await emitTransferWithRetry(targetWindowLabel, payload);
    },
    [currentWindowLabel, tabs, tabSplitSessions, splitSessionConfigs, tabSplitTrees],
  );

  const handleWindowDragOver = useCallback(
    (targetWindowLabel: string, e: React.DragEvent<HTMLDivElement>) => {
      const dataTypes = Array.from(e.dataTransfer.types);
      if (
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_MIME) &&
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_URI_MIME) &&
        !dataTypes.includes("text/plain")
      ) {
        return;
      }
      e.preventDefault();
      e.stopPropagation();
      e.dataTransfer.dropEffect = "move";
      setDragOverWindowLabel(targetWindowLabel);
    },
    [],
  );

  const handleWindowDragLeave = useCallback((e: React.DragEvent<HTMLDivElement>) => {
    const related = e.relatedTarget as Node | null;
    if (!related || !e.currentTarget.contains(related)) {
      setDragOverWindowLabel(null);
    }
  }, []);

  const lastProcessedTransferKey = useRef<string | null>(null);

  const handleWindowDrop = useCallback(
    async (targetWindowLabel: string, e: React.DragEvent<HTMLDivElement>) => {
      setDragOverWindowLabel(null);
      e.preventDefault();
      e.stopPropagation();
      let payload: CrossWindowTransferPayload;
      const rawCustom = e.dataTransfer.getData(CROSS_WINDOW_TRANSFER_MIME);
      const rawUri = e.dataTransfer.getData(CROSS_WINDOW_TRANSFER_URI_MIME);
      const rawPlainText = e.dataTransfer.getData("text/plain");

      if (rawCustom) {
        try {
          payload = JSON.parse(rawCustom) as CrossWindowTransferPayload;
        } catch {
          return;
        }
      } else {
        const transferToken = rawUri || rawPlainText;
        const storedPayload = transferToken ? readCrossWindowTransferPayload(transferToken) : null;
        if (!storedPayload) {
          return;
        }
        payload = storedPayload;
      }
      const cfg = payload.entry.config;
      if (!cfg) {
        return;
      }

      const transferKey = `${payload.sourceWindowLabel}:${payload.entry.tabId}:${payload.entry.sessionId}:${targetWindowLabel}`;
      if (lastProcessedTransferKey.current === transferKey) {
        return;
      }
      lastProcessedTransferKey.current = transferKey;
      setTimeout(() => {
        lastProcessedTransferKey.current = null;
      }, 3000);

      if (targetWindowLabel !== currentWindowLabel) {
        const payloadToSend = { ...payload, copyMode: e.altKey };
        await emitTransferWithRetry(targetWindowLabel, payloadToSend);
        if (rawUri || rawPlainText) {
          clearCrossWindowTransferPayload(rawUri || rawPlainText);
        }
        return;
      }

      if (payload.sourceWindowLabel === currentWindowLabel) {
        return;
      }

      await processTransferPayloadRef.current({
        ...payload,
        copyMode: e.altKey || payload.copyMode,
      });
      if (rawUri || rawPlainText) {
        clearCrossWindowTransferPayload(rawUri || rawPlainText);
      }
    },
    [currentWindowLabel],
  );

  const handleWindowRootDragOver = useCallback(
    (e: React.DragEvent<HTMLDivElement>) => {
      handleWindowDragOver(currentWindowLabel, e);
    },
    [currentWindowLabel, handleWindowDragOver],
  );

  const handleWindowRootDragLeave = useCallback(
    (e: React.DragEvent<HTMLDivElement>) => {
      const dataTypes = Array.from(e.dataTransfer.types);
      if (
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_MIME) &&
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_URI_MIME) &&
        !dataTypes.includes("text/plain")
      ) {
        return;
      }
      handleWindowDragLeave(e);
    },
    [handleWindowDragLeave],
  );

  const handleWindowRootDrop = useCallback(
    async (e: React.DragEvent<HTMLDivElement>) => {
      const dataTypes = Array.from(e.dataTransfer.types);
      if (
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_MIME) &&
        !dataTypes.includes(CROSS_WINDOW_TRANSFER_URI_MIME) &&
        !dataTypes.includes("text/plain")
      ) {
        return;
      }
      await handleWindowDrop(currentWindowLabel, e);
    },
    [currentWindowLabel, handleWindowDrop],
  );

  const getConfiguredFontSizeForTab = useCallback(
    (tabId: string) => {
      const tab = tabs.find((entry) => entry.id === tabId);
      if (!tab) {
        return globalFontSizeRef.current;
      }
      return getTabTheme(tab).fontSize ?? globalFontSizeRef.current;
    },
    [getTabTheme, tabs],
  );

  useEffect(() => {
    focusedPaneSessionRef.current = null;
  }, [activeTab]);

  const zoomIn = useCallback(() => {
    if (!activeTab) return;
    const hasMultiplePanes = (tabSplitSessions[activeTab]?.length ?? 0) > 0;
    const focusedSession = focusedPaneSessionRef.current;
    const paneKey = focusedSession ? `${activeTab}:${focusedSession}` : null;
    const configuredFontSize = getConfiguredFontSizeForTab(activeTab);
    if (paneKey) {
      setPaneFontSizes((prev) => ({
        ...prev,
        [paneKey]: Math.min(
          MAX_FONT_SIZE,
          (prev[paneKey] ?? tabFontSizes[activeTab] ?? configuredFontSize) + 1
        ),
      }));
    } else if (!hasMultiplePanes) {
      setTabFontSizes((prev) => ({
        ...prev,
        [activeTab]: Math.min(
          MAX_FONT_SIZE,
          (prev[activeTab] ?? configuredFontSize) + 1
        ),
      }));
    }
  }, [activeTab, getConfiguredFontSizeForTab, tabFontSizes, tabSplitSessions]);

  const zoomOut = useCallback(() => {
    if (!activeTab) return;
    const hasMultiplePanes = (tabSplitSessions[activeTab]?.length ?? 0) > 0;
    const focusedSession = focusedPaneSessionRef.current;
    const paneKey = focusedSession ? `${activeTab}:${focusedSession}` : null;
    const configuredFontSize = getConfiguredFontSizeForTab(activeTab);
    if (paneKey) {
      setPaneFontSizes((prev) => ({
        ...prev,
        [paneKey]: Math.max(
          MIN_FONT_SIZE,
          (prev[paneKey] ?? tabFontSizes[activeTab] ?? configuredFontSize) - 1
        ),
      }));
    } else if (!hasMultiplePanes) {
      setTabFontSizes((prev) => ({
        ...prev,
        [activeTab]: Math.max(
          MIN_FONT_SIZE,
          (prev[activeTab] ?? configuredFontSize) - 1
        ),
      }));
    }
  }, [activeTab, getConfiguredFontSizeForTab, tabFontSizes, tabSplitSessions]);

  const resetZoom = useCallback(() => {
    if (!activeTab) return;
    const hasMultiplePanes = (tabSplitSessions[activeTab]?.length ?? 0) > 0;
    const focusedSession = focusedPaneSessionRef.current;
    const paneKey = focusedSession ? `${activeTab}:${focusedSession}` : null;
    if (paneKey) {
      setPaneFontSizes((prev) => {
        const next = { ...prev };
        delete next[paneKey];
        return next;
      });
    } else if (!hasMultiplePanes) {
      setTabFontSizes((prev) => {
        const next = { ...prev };
        delete next[activeTab];
        return next;
      });
    }
  }, [activeTab, tabSplitSessions]);

  const zoomAllInTabIn = useCallback(() => {
    if (!activeTab) return;
    const configuredFontSize = getConfiguredFontSizeForTab(activeTab);
    setTabFontSizes((prev) => ({
      ...prev,
      [activeTab]: Math.min(
        MAX_FONT_SIZE,
        (prev[activeTab] ?? configuredFontSize) + 1
      ),
    }));
    setPaneFontSizes((prev) => {
      const next = { ...prev };
      const prefix = `${activeTab}:`;
      for (const key of Object.keys(next)) {
        if (key.startsWith(prefix)) delete next[key];
      }
      return next;
    });
  }, [activeTab, getConfiguredFontSizeForTab]);

  const zoomAllInTabOut = useCallback(() => {
    if (!activeTab) return;
    const configuredFontSize = getConfiguredFontSizeForTab(activeTab);
    setTabFontSizes((prev) => ({
      ...prev,
      [activeTab]: Math.max(
        MIN_FONT_SIZE,
        (prev[activeTab] ?? configuredFontSize) - 1
      ),
    }));
    setPaneFontSizes((prev) => {
      const next = { ...prev };
      const prefix = `${activeTab}:`;
      for (const key of Object.keys(next)) {
        if (key.startsWith(prefix)) delete next[key];
      }
      return next;
    });
  }, [activeTab, getConfiguredFontSizeForTab]);

  const resetZoomAllInTab = useCallback(() => {
    if (!activeTab) return;
    setTabFontSizes((prev) => {
      const next = { ...prev };
      delete next[activeTab];
      return next;
    });
    setPaneFontSizes((prev) => {
      const next = { ...prev };
      const prefix = `${activeTab}:`;
      for (const key of Object.keys(next)) {
        if (key.startsWith(prefix)) delete next[key];
      }
      return next;
    });
  }, [activeTab]);

  const handleFullscreen = useCallback(async () => {
    const win = getCurrentWindow();
    const isFull = await win.isFullscreen();
    await win.setFullscreen(!isFull);
  }, []);

  const terminalOnlyPreviousFullscreenRef = useRef(false);

  const toggleTerminalOnlyFullscreen = useCallback(() => {
    setTerminalOnlyFullscreen((current) => !current);
    window.setTimeout(() => window.dispatchEvent(new Event("kortty-refit")), 0);
    window.setTimeout(() => window.dispatchEvent(new Event("kortty-refit")), 120);
  }, []);

  // Java setTerminalOnlyFullscreen also enters/leaves native fullscreen.
  const terminalOnlyFullscreenInitializedRef = useRef(false);
  useEffect(() => {
    if (!terminalOnlyFullscreenInitializedRef.current) {
      terminalOnlyFullscreenInitializedRef.current = true;
      return;
    }
    const win = getCurrentWindow();
    if (terminalOnlyFullscreen) {
      win
        .isFullscreen()
        .then((isFull) => {
          terminalOnlyPreviousFullscreenRef.current = isFull;
          if (!isFull) {
            return win.setFullscreen(true);
          }
        })
        .catch(console.error);
    } else {
      win.setFullscreen(terminalOnlyPreviousFullscreenRef.current).catch(console.error);
    }
  }, [terminalOnlyFullscreen]);

  const toggleHideTerminalScrollbarsInFullscreen = useCallback(() => {
    setHideTerminalScrollbarsInFullscreen((current) => {
      const next = !current;
      invoke<GlobalSettings>("get_settings")
        .then((settings) =>
          invoke("save_settings", {
            settings: { ...settings, hideTerminalScrollbarsInFullscreen: next },
          }),
        )
        .catch(console.error);
      return next;
    });
  }, []);

  const saveSnippetFileDraft = useCallback(async (content: string) => {
    const draft = snippetFileDraft;
    if (!draft) return;
    if (draft.source === "local") {
      await invoke("write_local_text_file", { path: draft.path, content });
    } else {
      await invoke("write_remote_text_file", {
        sessionId: draft.sessionId,
        remotePath: draft.path,
        content,
      });
    }
    setSnippetFileDraft((current) => (current?.id === draft.id ? { ...current, content } : current));
  }, [snippetFileDraft]);

  const showTerminalFileLoadNotice = useCallback((message: string | null) => {
    if (terminalFileLoadNoticeTimerRef.current !== null) {
      window.clearTimeout(terminalFileLoadNoticeTimerRef.current);
      terminalFileLoadNoticeTimerRef.current = null;
    }
    setTerminalFileLoadNotice(message);
    if (message) {
      terminalFileLoadNoticeTimerRef.current = window.setTimeout(() => {
        terminalFileLoadNoticeTimerRef.current = null;
        setTerminalFileLoadNotice(null);
      }, 6000);
    }
  }, []);

  /**
   * WP1.3c: opens a terminal selection as a remote text file in the snippet
   * editor (port of MainWindow.loadTerminalSelectionAsTextFile): the selection
   * is validated as a single file name, resolved against the tracked remote
   * working directory (home hint as "~" fallback) and loaded via
   * read_remote_text_file. Binary/non-UTF-8 files are rejected with a
   * localized message.
   */
  const handleOpenTerminalSelectionInSnippetEditor = useCallback(
    async (sessionId: string, selectedText: string) => {
      let fileName: string;
      try {
        fileName = normalizeSelectedFileName(selectedText);
      } catch {
        showTerminalFileLoadNotice(t("terminal.loadTextFile.invalidSelection"));
        return;
      }
      showTerminalFileLoadNotice(t("terminal.loadTextFile.loading", { name: fileName }));
      try {
        const hints = await invoke<{ current?: string; home?: string }>(
          "get_remote_directory_hints",
          { sessionId },
        );
        const remotePath = resolveRemoteFilePath(hints.current, fileName, hints.home);
        const content = await invoke<string>("read_remote_text_file", { sessionId, remotePath });
        setSnippetFileDraft({
          id: crypto.randomUUID(),
          source: "remote",
          path: remotePath,
          sessionId,
          content,
        });
        setOpenDialog("snippetManager");
        showTerminalFileLoadNotice(t("terminal.loadTextFile.loaded", { path: remotePath }));
      } catch (error) {
        const message = String(error);
        if (message.includes("BINARY_OR_NON_TEXT")) {
          showTerminalFileLoadNotice(t("terminal.loadTextFile.binary", { path: fileName }));
        } else if (message.includes("Session not found")) {
          showTerminalFileLoadNotice(t("terminal.loadTextFile.notConnected"));
        } else {
          showTerminalFileLoadNotice(t("terminal.loadTextFile.failed", { name: fileName, error: message }));
        }
      }
    },
    [showTerminalFileLoadNotice, t],
  );

  const handleQuit = useCallback(async () => {
    for (const tab of tabs) {
      if ((tab.kind ?? "terminal") === "terminal" && tab.status === "connected") {
        await handleDisconnect(tab.id);
      }
    }
    const win = getCurrentWindow();
    await win.close();
  }, [tabs, handleDisconnect]);

  const toggleMenuBarPreference = useCallback((visible: boolean) => {
    setShowMenuBar(visible);
    invoke<GlobalSettings>("get_settings")
      .then((settings) => invoke("save_settings", { settings: { ...settings, showMenuBar: visible } }))
      .catch(console.error);
  }, [showMenuBar]);

  const setLocalFileBrowserVisiblePreference = useCallback((visible: boolean) => {
    setLocalFileBrowserVisible(visible);
    invoke<GlobalSettings>("get_settings")
      .then((settings) =>
        invoke("save_settings", { settings: { ...settings, localFileBrowserVisible: visible } }),
      )
      .catch(console.error);
  }, []);

  const toggleLocalFileBrowserPreference = useCallback(() => {
    setLocalFileBrowserVisible((prev) => {
      const next = !prev;
      invoke<GlobalSettings>("get_settings")
        .then((settings) =>
          invoke("save_settings", { settings: { ...settings, localFileBrowserVisible: next } }),
        )
        .catch(console.error);
      return next;
    });
  }, []);

  const setTabTerminalEffect = useCallback((tabId: string, pluginId: string | undefined) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId && (tab.kind ?? "terminal") === "terminal"
          ? { ...tab, terminalEffectPluginId: pluginId }
          : tab,
      ),
    );
  }, []);

  const setTabTerminalEffectSpeed = useCallback((tabId: string, speed: number) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId && (tab.kind ?? "terminal") === "terminal"
          ? { ...tab, terminalEffectAnimationSpeed: normalizeTerminalEffectSpeed(speed) }
          : tab,
      ),
    );
  }, []);

  const handleOpenProjectDialog = useCallback(async () => {
    const path = await openFileDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "KorTTY Project", extensions: ["json"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (!path || typeof path !== "string") return;

    try {
      const project = await invoke<Project>("peek_project", { path });
      setProjectPreview(project);
      setOpenDialog("projectPreview");
    } catch (error) {
      console.error("Failed to preview project:", error);
    }
  }, []);

  const handleSaveProject = useCallback(async () => {
    const draft = buildProjectSnapshot(currentProject);
    if (!currentProject?.filePath) {
      projectSaveModeRef.current = "save";
      setProjectSettingsDraft(draft);
      setOpenDialog("projectSettings");
      return;
    }
    try {
      await saveProjectToPath(draft, false);
    } catch (error) {
      console.error("Failed to save project:", error);
    }
  }, [buildProjectSnapshot, currentProject, saveProjectToPath]);

  const handleSaveProjectAs = useCallback(() => {
    projectSaveModeRef.current = "saveAs";
    setProjectSettingsDraft(buildProjectSnapshot(currentProject));
    setOpenDialog("projectSettings");
  }, [buildProjectSnapshot, currentProject]);

  const handleEditProjectSettings = useCallback(() => {
    projectSaveModeRef.current = "edit";
    setProjectSettingsDraft(buildProjectSnapshot(currentProject));
    setOpenDialog("projectSettings");
  }, [buildProjectSnapshot, currentProject]);

  const resolveConnectionDisplayName = useCallback((sessionId: string) => {
    const directTab = tabs.find((tab) => tab.id === sessionId);
    if (directTab) {
      return directTab.label;
    }

    const splitConfig = splitSessionConfigs[sessionId];
    if (splitConfig) {
      return `${splitConfig.username}@${splitConfig.host}`;
    }

    const parentTabId = Object.entries(tabSplitSessions).find(([, sessionIds]) => sessionIds.includes(sessionId))?.[0];
    return parentTabId ? tabs.find((tab) => tab.id === parentTabId)?.label : undefined;
  }, [splitSessionConfigs, tabSplitSessions, tabs]);

  /**
   * Resolves the connection-scoped AI settings (fixed profile + pinned skills)
   * for a session so chat/agent requests can carry them (WP4.6).
   */
  const resolveConnectionAiSettings = useCallback((sessionId: string): {
    connectionAiProfileId?: string;
    connectionAiSkillIds?: string[];
  } => {
    const splitConfig = splitSessionConfigs[sessionId];
    const parentTabId = Object.entries(tabSplitSessions)
      .find(([, sessionIds]) => sessionIds.includes(sessionId))?.[0];
    const tab =
      tabs.find((candidate) => candidate.id === sessionId) ??
      (parentTabId ? tabs.find((candidate) => candidate.id === parentTabId) : undefined);

    // Split sessions opened for a different server carry their own AI settings.
    if (splitConfig && (splitConfig.aiProfileId || splitConfig.aiSkillIds?.length)) {
      return {
        connectionAiProfileId: splitConfig.aiProfileId || undefined,
        connectionAiSkillIds: splitConfig.aiSkillIds?.length ? splitConfig.aiSkillIds : undefined,
      };
    }

    if (tab?.connectionId) {
      const connection = useConnectionStore
        .getState()
        .connections.find((candidate) => candidate.id === tab.connectionId);
      if (connection) {
        return {
          connectionAiProfileId: connection.aiProfileId || undefined,
          connectionAiSkillIds: connection.aiSkillIds?.length ? connection.aiSkillIds : undefined,
        };
      }
    }

    // Ad-hoc sessions (quick connect) keep their AI selection on the tab itself.
    if (tab && (tab.aiProfileId || tab.aiSkillIds?.length)) {
      return {
        connectionAiProfileId: tab.aiProfileId || undefined,
        connectionAiSkillIds: tab.aiSkillIds?.length ? tab.aiSkillIds : undefined,
      };
    }
    return {};
  }, [splitSessionConfigs, tabSplitSessions, tabs]);

  /** Starts a recording for one split or the whole tab with real geometry (WP3.8). */
  const startTerminalRecordingWithScope = useCallback(
    async (tabId: string, targetSessionId: string, scope: TerminalRecordingScope) => {
      const splitIds = tabSplitSessions[tabId] ?? [];
      const geometrySessionId = scope === "WholeTab" ? tabId : targetSessionId;
      const geometry =
        getTerminalGeometry(geometrySessionId) ??
        getTerminalGeometry(tabId) ?? {
          columns: 80,
          rows: 24,
          pixelWidth: 0,
          pixelHeight: 0,
        };
      try {
        const response = await invoke<TerminalRecordingStartResponse>("start_terminal_recording", {
          request: {
            tabId,
            splitId: scope === "ActiveSplit" && targetSessionId !== tabId ? targetSessionId : undefined,
            connectionName: resolveConnectionDisplayName(
              scope === "ActiveSplit" ? targetSessionId : tabId,
            ),
            scope,
            columns: geometry.columns,
            rows: geometry.rows,
          },
        });
        setRecordingSessions((prev) => {
          const next = { ...prev };
          const sessionIds = scope === "WholeTab" ? [tabId, ...splitIds] : [targetSessionId];
          for (const id of sessionIds) {
            next[id] = response;
          }
          return next;
        });
      } catch (error) {
        setRecordingErrorMessage(t("terminal.recording.error.start", { message: String(error) }));
      }
    },
    [resolveConnectionDisplayName, t, tabSplitSessions],
  );

  /** Stops a recording and removes all session mappings sharing it (WholeTab). */
  const stopTerminalRecordingSession = useCallback(
    async (recording: TerminalRecordingStartResponse) => {
      try {
        await invoke("stop_terminal_recording", { sessionId: recording.sessionId });
      } catch (error) {
        setRecordingErrorMessage(t("terminal.recording.error.stop", { message: String(error) }));
      } finally {
        setRecordingSessions((prev) => {
          const next: typeof prev = {};
          for (const [key, value] of Object.entries(prev)) {
            if (value?.sessionId !== recording.sessionId) {
              next[key] = value;
            }
          }
          return next;
        });
      }
    },
    [t],
  );

  const toggleTerminalRecording = useCallback(async (sessionId: string) => {
    const current = recordingSessions[sessionId];
    if (current) {
      await stopTerminalRecordingSession(current);
      return;
    }

    const parentTabId = tabs.some((tab) => tab.id === sessionId)
      ? sessionId
      : Object.entries(tabSplitSessions).find(([, sessionIds]) => sessionIds.includes(sessionId))?.[0] ?? sessionId;
    const tab = tabs.find((entry) => entry.id === parentTabId);
    if (!tab || (tab.kind ?? "terminal") !== "terminal") {
      setRecordingErrorMessage(t("terminal.recording.error.noTerminal"));
      return;
    }
    if (tab.status !== "connected") {
      setRecordingErrorMessage(t("terminal.recording.error.notConnected"));
      return;
    }

    let settings: GlobalSettingsView | null = null;
    try {
      settings = await invoke<GlobalSettingsView>("get_settings");
    } catch (error) {
      console.error("Failed to load settings for recording:", error);
    }
    if (!settings?.terminalRecordingEnabled) {
      setRecordingErrorMessage(t("terminal.recording.error.disabled"));
      return;
    }
    const defaultScope: TerminalRecordingScope =
      settings.terminalRecordingDefaultScope ?? "ActiveSplit";
    const splitIds = tabSplitSessions[parentTabId] ?? [];
    if (splitIds.length > 0) {
      // Only ask for the scope when the tab actually has split terminals
      // (Java TerminalTab.chooseRecordingScope).
      setRecordingScopeRequest({ tabId: parentTabId, targetSessionId: sessionId, defaultScope });
      return;
    }
    await startTerminalRecordingWithScope(parentTabId, sessionId, defaultScope);
  }, [
    recordingSessions,
    startTerminalRecordingWithScope,
    stopTerminalRecordingSession,
    t,
    tabSplitSessions,
    tabs,
  ]);

  /** Ctrl/Cmd+Shift+E and Tools menu entry (Java MainWindow.toggleTerminalRecording). */
  const toggleRecordingForActiveTab = useCallback(() => {
    const tab = activeTabEntry;
    if (!tab || (tab.kind ?? "terminal") !== "terminal") {
      setRecordingErrorMessage(t("terminal.recording.error.noTerminal"));
      return;
    }
    const sessionIds = [tab.id, ...(tabSplitSessions[tab.id] ?? [])];
    const activeRecording = sessionIds
      .map((id) => recordingSessions[id])
      .find((entry) => entry != null);
    if (activeRecording) {
      void stopTerminalRecordingSession(activeRecording);
      return;
    }
    const focused = focusedPaneSessionRef.current;
    const targetSessionId = focused && sessionIds.includes(focused) ? focused : tab.id;
    void toggleTerminalRecording(targetSessionId);
  }, [
    activeTabEntry,
    recordingSessions,
    stopTerminalRecordingSession,
    t,
    tabSplitSessions,
    toggleTerminalRecording,
  ]);

  const handleRequestAiAction = useCallback(async (pendingAction: PendingAiAction) => {
    if (!pendingAction.selectedText.trim()) {
      return;
    }
    const nextAction: PendingAiAction = {
      ...pendingAction,
      ...resolveConnectionAiSettings(pendingAction.sessionId),
    };
    try {
      const profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
      if (profiles.length === 0) {
        setPendingAiAction(nextAction);
        setOpenDialog("aiManager");
        return;
      }
      setPendingAiAction(nextAction);
      setOpenDialog("aiAction");
    } catch (error) {
      console.error("Failed to load AI profiles:", error);
      setPendingAiAction(nextAction);
      setOpenDialog("aiManager");
    }
  }, [resolveConnectionAiSettings]);

  const handleRequestTerminalAgent = useCallback(async (pendingAction: PendingTerminalAgentAction) => {
    const nextAction: PendingTerminalAgentAction = {
      ...pendingAction,
      ...resolveConnectionAiSettings(pendingAction.sessionId),
    };
    try {
      const profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
      if (profiles.length === 0) {
        setPendingTerminalAgentAction(nextAction);
        setPendingTerminalAgentMode("run");
        setOpenDialog("aiManager");
        return;
      }
      setPendingTerminalAgentAction(nextAction);
      setPendingTerminalAgentMode("run");
      setOpenDialog("aiAgent");
    } catch (error) {
      console.error("Failed to load AI profiles:", error);
      setPendingTerminalAgentAction(nextAction);
      setPendingTerminalAgentMode("run");
      setOpenDialog("aiManager");
    }
  }, [resolveConnectionAiSettings]);

  const handleRequestTerminalAgentPlan = useCallback(async (pendingAction: PendingTerminalAgentAction) => {
    const nextAction: PendingTerminalAgentAction = {
      ...pendingAction,
      ...resolveConnectionAiSettings(pendingAction.sessionId),
    };
    try {
      const profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
      if (profiles.length === 0) {
        setPendingTerminalAgentAction(nextAction);
        setPendingTerminalAgentMode("plan");
        setOpenDialog("aiManager");
        return;
      }
      setPendingTerminalAgentAction(nextAction);
      setPendingTerminalAgentMode("plan");
      setOpenDialog("aiAgentPlan");
    } catch (error) {
      console.error("Failed to load AI profiles:", error);
      setPendingTerminalAgentAction(nextAction);
      setPendingTerminalAgentMode("plan");
      setOpenDialog("aiManager");
    }
  }, [resolveConnectionAiSettings]);

  const isReadOnlyMirrorSession = useCallback((sessionId: string) => {
    const directTab = tabs.find((tab) => tab.id === sessionId);
    if (directTab) {
      return !!directTab.readOnlyMirror;
    }

    const parentTabId = Object.entries(tabSplitSessions).find(([, sessionIds]) => sessionIds.includes(sessionId))?.[0];
    const parentTab = parentTabId ? tabs.find((tab) => tab.id === parentTabId) : undefined;
    return !!parentTab?.readOnlyMirror;
  }, [tabSplitSessions, tabs]);

  const loadTerminalAgentVisibilitySettings = useCallback(async () => {
    try {
      const settings = await invoke<GlobalSettings>("get_settings");
      return {
        showDebugMessages: settings.terminalAgentShowDebugMessages ?? false,
        showRuntimeMessages: settings.terminalAgentShowRuntimeMessages ?? false,
      };
    } catch {
      return {
        showDebugMessages: false,
        showRuntimeMessages: false,
      };
    }
  }, []);

  const handleStartTerminalAgent = useCallback(async (request: TerminalAgentRequest) => {
    let effectiveRequest = request;
    if (request.confirmMutatingCommandSets === undefined) {
      const settings = await invoke<GlobalSettings>("get_settings").catch(() => null);
      effectiveRequest = {
        ...request,
        confirmMutatingCommandSets: settings?.terminalAgentConfirmMutatingCommandSets ?? false,
      };
    }
    const response = await invoke<TerminalAgentStartResponse>("start_terminal_agent", {
      request: effectiveRequest,
    });
    if (request.executionTarget === "TerminalWindow") {
      setTerminalAgentStates((prev) => ({
        ...prev,
        [request.sessionId]: {
          runId: response.runId,
          sessionId: request.sessionId,
          executionTarget: request.executionTarget,
          phase: "Starting",
          summary: "Starting terminal agent run.",
          userMessage: request.userPrompt,
          turn: 0,
        },
      }));
    }
    setPendingTerminalAgentAction(null);
    setPendingTerminalAgentMode(null);
    setOpenDialog(null);
    return response;
  }, []);

  const handleStartTerminalAgentPlan = useCallback(async (request: TerminalAgentPlanRequest) => {
    const response = await invoke<TerminalAgentPlanStartResponse>("start_terminal_agent_plan", { request });
    return response;
  }, []);

  const emitTerminalAgentNote = useCallback(async (sessionId: string, message: string) => {
    const normalized = message.replace(/\r/g, "");
    const lines = normalized.split("\n");
    const payloadText = lines.map((line) => `[KorTTY Agent] ${line}\r\n`).join("");
    const payload = Array.from(new TextEncoder().encode(payloadText));
    await emit(`terminal-output-${sessionId}`, payload);
  }, []);

  const redrawTerminalPrompt = useCallback(async (sessionId: string) => {
    await invoke("ssh_send_input", {
      sessionId,
      data: [21, 13],
    });
  }, []);

  const createAiTab = useCallback((request: AiRequestPayload) => {
    const id = crypto.randomUUID();
    const newTab: Tab = {
      id,
      kind: "ai",
      label: buildAiTabLabel(request.action),
      status: "disconnected",
      aiInitialRequest: request,
    };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
    setPendingAiAction(null);
    setOpenDialog(null);
  }, []);

  const createAiAgentTab = useCallback((request: TerminalAgentRequest, runId: string) => {
    const id = crypto.randomUUID();
    const newTab: Tab = {
      id,
      kind: "ai",
      label: "AI Agent",
      status: "disconnected",
      aiAgentRequest: request,
      aiAgentRunId: runId,
    };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
    setPendingTerminalAgentAction(null);
    setPendingTerminalAgentMode(null);
    setOpenDialog(null);
  }, []);

  const createAiAgentPlanTab = useCallback((
    request: TerminalAgentPlanRequest,
    runId = "",
    initialState?: TerminalAgentPlanRunState,
  ) => {
    const id = crypto.randomUUID();
    const newTab: Tab = {
      id,
      kind: "ai",
      label: "AI Agent Plan",
      status: "disconnected",
      aiAgentPlanRequest: request,
      aiAgentPlanRunId: runId,
      aiAgentPlanInitialState: initialState,
    };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
    setPendingTerminalAgentAction(null);
    setPendingTerminalAgentMode(null);
    setOpenDialog(null);
    return id;
  }, []);

  const updateAiAgentPlanTab = useCallback((
    tabId: string,
    runId: string,
    initialState: TerminalAgentPlanRunState,
  ) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId
          ? {
              ...tab,
              aiAgentPlanRunId: runId,
              aiAgentPlanInitialState: initialState,
            }
          : tab,
      ),
    );
  }, []);

  const markAiAgentPlanTabFailed = useCallback((
    tabId: string,
    request: TerminalAgentPlanRequest,
    error: unknown,
  ) => {
    const message = `Planning start failed: ${String(error)}`;
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId
          ? {
              ...tab,
              aiAgentPlanInitialState: {
                runId: "",
                sessionId: request.sessionId,
                phase: "Failed",
                summary: message,
                userMessage: message,
              },
            }
          : tab,
      ),
    );
  }, []);

  const handleLaunchTerminalAgentPlanTask = useCallback(async (request: TerminalAgentPlanRequest) => {
    const placeholderState: TerminalAgentPlanRunState = {
      runId: "",
      sessionId: request.sessionId,
      phase: "Starting",
      summary: "Collecting remote server facts.",
      userMessage: "Preparing the planning run and collecting remote server facts.",
    };
    const tabId = createAiAgentPlanTab(request, "", placeholderState);
    try {
      const response = await handleStartTerminalAgentPlan(request);
      updateAiAgentPlanTab(tabId, response.runId, response.initialState);
    } catch (error) {
      markAiAgentPlanTabFailed(tabId, request, error);
      throw error;
    }
  }, [createAiAgentPlanTab, handleStartTerminalAgentPlan, markAiAgentPlanTabFailed, updateAiAgentPlanTab]);

  const handleLaunchTerminalAgentTask = useCallback(async (request: TerminalAgentRequest) => {
    if (request.executionTarget === "ChatWindow") {
      const response = await handleStartTerminalAgent({
        ...request,
        executionTarget: "ChatWindow",
      });
      createAiAgentTab(
        {
          ...request,
          executionTarget: "ChatWindow",
        },
        response.runId,
      );
      return;
    }

    await handleStartTerminalAgent({
      ...request,
      executionTarget: "TerminalWindow",
    });
  }, [createAiAgentTab, handleStartTerminalAgent]);

  const handleStartExecutionFromPlan = useCallback(async (runId: string) => {
    const visibility = await loadTerminalAgentVisibilitySettings();
    const response = await invoke<TerminalAgentPlanExecutionResponse>("start_terminal_agent_from_plan", {
      runId,
      executionTarget: terminalAgentExecutionTarget,
      showDebugMessages: visibility.showDebugMessages,
      showRuntimeMessages: visibility.showRuntimeMessages,
    });

    if (response.request.executionTarget === "ChatWindow") {
      createAiAgentTab(response.request, response.runId);
    }

    return response;
  }, [createAiAgentTab, loadTerminalAgentVisibilitySettings, terminalAgentExecutionTarget]);

  const handleTerminalAgentShortcut = useCallback(async (sessionId: string, rawCommand: string) => {
    const parsed = parseTerminalAgentShortcut(
      rawCommand,
      terminalAgentCommandName,
      terminalAgentCommandNameCaseInsensitive,
    );
    if (!parsed.ok) {
      await emitTerminalAgentNote(sessionId, parsed.error);
      return;
    }

    let profiles: AiProfile[] = [];
    try {
      profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
    } catch (error) {
      await emitTerminalAgentNote(sessionId, `Failed to load AI profiles: ${String(error)}`);
      return;
    }
    if (profiles.length === 0) {
      await emitTerminalAgentNote(sessionId, "No AI profile is configured. Open AI Manager first.");
      return;
    }

    const profileLookup = parsed.invocation.mode !== "ask"
      ? parsed.invocation.profileLookup?.trim()
      : undefined;
    const normalizedLookup = profileLookup?.toLowerCase();
    const connectionAi = resolveConnectionAiSettings(sessionId);
    // The connection-fixed profile wins over the default profile (WP4.6).
    const preferredProfileId = resolvePreferredAiProfileId(
      profiles,
      connectionAi.connectionAiProfileId || defaultAiProfileId,
    );
    const profile = profileLookup
      ? profiles.find((candidate) => {
          const candidateName = candidate.name.trim();
          const candidateId = candidate.id.trim();
          return (
            candidateName === profileLookup
            || candidateId === profileLookup
            || candidateName.toLowerCase() === normalizedLookup
            || candidateId.toLowerCase() === normalizedLookup
          );
        })
      : profiles.find((candidate) => candidate.id === preferredProfileId) ?? profiles[0];

    if (!profile) {
      await emitTerminalAgentNote(
        sessionId,
        `The AI profile "${profileLookup}" was not found.`,
      );
      return;
    }

    if (parsed.invocation.mode === "ask") {
      const requestId = crypto.randomUUID();
      await emitTerminalAgentNote(
        sessionId,
        `Starting question: ${parsed.invocation.userPrompt}`,
      );
      await emitTerminalAgentNote(sessionId, "Waiting for AI response...");
      try {
        const result = await invoke<AiExecutionResult>("execute_ai_action", {
          request: {
            action: "Ask",
            profileId: profile.id,
            selectedText: parsed.invocation.userPrompt,
            connectionDisplayName: resolveConnectionDisplayName(sessionId),
            userPrompt: parsed.invocation.userPrompt,
            connectionAiProfileId: connectionAi.connectionAiProfileId,
            connectionAiSkillIds: connectionAi.connectionAiSkillIds,
          },
          requestId,
        });
        await emitTerminalAgentNote(sessionId, result.content);
      } catch (error) {
        await emitTerminalAgentNote(sessionId, `Agent ask failed: ${String(error)}`);
      }
      try {
        await redrawTerminalPrompt(sessionId);
      } catch (error) {
        console.error("Failed to redraw terminal prompt after agent-ask:", error);
      }
      return;
    }

    if (parsed.invocation.mode === "plan") {
      try {
        await handleLaunchTerminalAgentPlanTask({
          sessionId,
          profileId: profile.id,
          userPrompt: parsed.invocation.userPrompt,
          connectionDisplayName: resolveConnectionDisplayName(sessionId),
          // An explicit profile lookup overrides the connection profile.
          connectionAiProfileId: profileLookup ? undefined : connectionAi.connectionAiProfileId,
          connectionAiSkillIds: connectionAi.connectionAiSkillIds,
        });
      } catch (error) {
        await emitTerminalAgentNote(sessionId, `Agent planning start failed: ${String(error)}`);
      }
      return;
    }

    const visibility = await loadTerminalAgentVisibilitySettings();

    try {
      if (terminalAgentShowRunDialog) {
        setPendingTerminalAgentAction({
          sessionId,
          connectionDisplayName: resolveConnectionDisplayName(sessionId),
          initialPrompt: parsed.invocation.userPrompt,
          initialProfileId: profile.id,
          initialExecutionTarget: "TerminalWindow",
          initialAskConfirmationBeforeEveryCommand:
            parsed.invocation.askConfirmationBeforeEveryCommand,
          initialAutoApproveRootCommands: parsed.invocation.autoApproveRootCommands,
          connectionAiProfileId: profileLookup ? undefined : connectionAi.connectionAiProfileId,
          connectionAiSkillIds: connectionAi.connectionAiSkillIds,
        });
        setPendingTerminalAgentMode("run");
        setOpenDialog("aiAgent");
        return;
      }

      await handleLaunchTerminalAgentTask({
        sessionId,
        profileId: profile.id,
        userPrompt: parsed.invocation.userPrompt,
        connectionDisplayName: resolveConnectionDisplayName(sessionId),
        executionTarget: "TerminalWindow",
        showDebugMessages: visibility.showDebugMessages,
        showRuntimeMessages: visibility.showRuntimeMessages,
        askConfirmationBeforeEveryCommand: parsed.invocation.askConfirmationBeforeEveryCommand,
        autoApproveRootCommands: parsed.invocation.autoApproveRootCommands,
        // An explicit profile lookup overrides the connection profile.
        connectionAiProfileId: profileLookup ? undefined : connectionAi.connectionAiProfileId,
        connectionAiSkillIds: connectionAi.connectionAiSkillIds,
      });
    } catch (error) {
      await emitTerminalAgentNote(sessionId, `Agent start failed: ${String(error)}`);
    }
  }, [
    emitTerminalAgentNote,
    handleLaunchTerminalAgentTask,
    handleLaunchTerminalAgentPlanTask,
    loadTerminalAgentVisibilitySettings,
    redrawTerminalPrompt,
    resolveConnectionAiSettings,
    resolveConnectionDisplayName,
    terminalAgentCommandName,
    terminalAgentCommandNameCaseInsensitive,
    terminalAgentShowRunDialog,
    defaultAiProfileId,
  ]);

  const handleApproveTerminalAgent = useCallback(async (approval: TerminalAgentApproval) => {
    await invoke("approve_terminal_agent", { runId: approval.runId });
  }, []);

  const handleApproveTerminalAgentAlways = useCallback(async (approval: TerminalAgentApproval) => {
    await invoke("approve_terminal_agent_always", { runId: approval.runId });
  }, []);

  const handleStopTerminalAgent = useCallback(async (runId: string) => {
    await invoke("cancel_terminal_agent", { runId });
  }, []);

  const handleSubmitTerminalAgentPassword = useCallback(
    async (request: TerminalAgentPasswordRequest, password: string) => {
      await invoke("submit_terminal_agent_sudo_password", {
        runId: request.runId,
        password,
      });
    },
    [],
  );

  const handleOpenSavedAiChat = useCallback((chat: SavedAiChat) => {
    setPendingAiAction(null);
    const existingTab = tabs.find((tab) => (tab.kind ?? "terminal") === "ai" && tab.aiChatId === chat.id);
    if (existingTab) {
      setActiveTab(existingTab.id);
      setOpenDialog(null);
      return;
    }

    const id = crypto.randomUUID();
    const newTab: Tab = {
      id,
      kind: "ai",
      label: chat.title || "AI Chat",
      status: "disconnected",
      aiChatId: chat.id,
      aiSavedChat: chat,
    };
    setTabs((prev) => [...prev, newTab]);
    setActiveTab(id);
    setOpenDialog(null);
  }, [tabs]);

  const handleCloseAiManager = useCallback(async () => {
    try {
      const profiles = await invoke<AiProfile[]>("get_ai_profiles");
      setHasConfiguredAiProfiles(profiles.length > 0);
      if (pendingAiAction) {
        setOpenDialog(profiles.length > 0 ? "aiAction" : null);
      } else if (pendingTerminalAgentAction) {
        setOpenDialog(
          profiles.length > 0
            ? (pendingTerminalAgentMode === "plan" ? "aiAgentPlan" : "aiAgent")
            : null,
        );
      } else {
        setOpenDialog(null);
      }
      if (profiles.length === 0 && pendingAiAction) {
        setPendingAiAction(null);
      }
      if (profiles.length === 0 && pendingTerminalAgentAction) {
        setPendingTerminalAgentAction(null);
        setPendingTerminalAgentMode(null);
      }
    } catch (error) {
      console.error("Failed to reload AI profiles:", error);
      setHasConfiguredAiProfiles(false);
      setOpenDialog(null);
      setPendingAiAction(null);
      setPendingTerminalAgentAction(null);
      setPendingTerminalAgentMode(null);
    }
  }, [pendingAiAction, pendingTerminalAgentAction, pendingTerminalAgentMode]);

  const handleOpenAiAgentForFocusedSession = useCallback(() => {
    if ((activeTabEntry?.kind ?? "terminal") !== "terminal" || activeTabEntry?.status !== "connected") {
      // Java MainWindow.showAiAgent surfaces an error instead of silently
      // doing nothing when no connected terminal is active.
      showTerminalFileLoadNotice(t("ai.agent.error.noTerminal"));
      return;
    }

    const activeSessionIds = new Set([
      activeTabEntry.id,
      ...(tabSplitSessions[activeTabEntry.id] ?? []),
    ]);
    const focusedSessionId = focusedPaneSessionRef.current;
    const targetSessionId =
      focusedSessionId && activeSessionIds.has(focusedSessionId)
        ? focusedSessionId
        : activeTabEntry.id;
    if (!targetSessionId || isReadOnlyMirrorSession(targetSessionId)) {
      showTerminalFileLoadNotice(t("ai.agent.error.noTerminal"));
      return;
    }

    void handleRequestTerminalAgent({
      sessionId: targetSessionId,
      connectionDisplayName: resolveConnectionDisplayName(targetSessionId),
    });
  }, [activeTabEntry, handleRequestTerminalAgent, isReadOnlyMirrorSession, resolveConnectionDisplayName, showTerminalFileLoadNotice, t, tabSplitSessions]);

  /** Ctrl/Cmd+Alt+P and Tools menu entry (Java MainWindow.showAiPlanning). */
  const handleOpenAiAgentPlanForFocusedSession = useCallback(() => {
    if ((activeTabEntry?.kind ?? "terminal") !== "terminal" || activeTabEntry?.status !== "connected") {
      showTerminalFileLoadNotice(t("ai.agent.error.noTerminal"));
      return;
    }

    const activeSessionIds = new Set([
      activeTabEntry.id,
      ...(tabSplitSessions[activeTabEntry.id] ?? []),
    ]);
    const focusedSessionId = focusedPaneSessionRef.current;
    const targetSessionId =
      focusedSessionId && activeSessionIds.has(focusedSessionId)
        ? focusedSessionId
        : activeTabEntry.id;
    if (!targetSessionId || isReadOnlyMirrorSession(targetSessionId)) {
      showTerminalFileLoadNotice(t("ai.agent.error.noTerminal"));
      return;
    }

    void handleRequestTerminalAgentPlan({
      sessionId: targetSessionId,
      connectionDisplayName: resolveConnectionDisplayName(targetSessionId),
    });
  }, [activeTabEntry, handleRequestTerminalAgentPlan, isReadOnlyMirrorSession, resolveConnectionDisplayName, showTerminalFileLoadNotice, t, tabSplitSessions]);

  // Automatic update notifications from the background checker (WP7.2).
  useEffect(() => {
    if (currentWindowLabel !== "main") {
      return;
    }
    let disposed = false;
    const unlisten = listen<AvailableUpdate>(UPDATE_AVAILABLE_EVENT, (event) => {
      if (disposed || !event.payload) {
        return;
      }
      setAvailableUpdate(event.payload);
      setUpdateDialogManual(false);
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
  }, [currentWindowLabel]);

  /** Manual update check from the About dialog (Java runManualUpdateCheck). */
  const runManualUpdateCheck = useCallback(async () => {
    setUpdateCheckBusy(true);
    setUpdateCheckStatus(t("updates.checking"));
    try {
      const result = await invoke<UpdateCheckResult>("check_for_updates_manually");
      switch (result?.status) {
        case "UpdateAvailable":
          if (result.update) {
            setUpdateCheckStatus(t("updates.available.short", { version: result.update.latestVersion }));
            setAvailableUpdate(result.update);
            setUpdateDialogManual(true);
          } else {
            setAvailableUpdate(null);
            setUpdateCheckStatus(t("updates.checkFailed"));
          }
          break;
        case "NoUpdate":
          setAvailableUpdate(null);
          setUpdateCheckStatus(t("updates.manual.current"));
          break;
        case "NoCompatibleAsset":
          setAvailableUpdate(null);
          setUpdateCheckStatus(t("updates.noCompatibleAsset"));
          break;
        default:
          setAvailableUpdate(null);
          setUpdateCheckStatus(
            result?.message
              ? t("updates.checkFailedDetail", { message: result.message })
              : t("updates.checkFailed"),
          );
          break;
      }
    } catch (error) {
      setAvailableUpdate(null);
      setUpdateCheckStatus(t("updates.checkFailedDetail", { message: String(error) }));
    } finally {
      setUpdateCheckBusy(false);
    }
  }, [t]);

  useEffect(() => {
    const unlistenStatus = listen<TerminalAgentRunState>("terminal-agent-status", (event) => {
      const nextState = event.payload;
      setTerminalAgentStates((prev) => {
        if (nextState.executionTarget === "ChatWindow") {
          if (!(nextState.sessionId in prev)) {
            return prev;
          }
          const { [nextState.sessionId]: _removed, ...rest } = prev;
          return rest;
        }
        return {
          ...prev,
          [nextState.sessionId]: nextState,
        };
      });
    });

    const unlistenApproval = listen<TerminalAgentApproval>("terminal-agent-approval", (event) => {
      const approval = event.payload;
      if (approval.executionTarget === "ChatWindow") {
        setTerminalAgentStates((prev) => {
          if (!(approval.sessionId in prev)) {
            return prev;
          }
          const { [approval.sessionId]: _removed, ...rest } = prev;
          return rest;
        });
        return;
      }
      setTerminalAgentStates((prev) => ({
        ...prev,
        [approval.sessionId]: {
          ...(prev[approval.sessionId] ?? {
            runId: approval.runId,
            sessionId: approval.sessionId,
            executionTarget: approval.executionTarget,
            phase: "AwaitingApproval",
            summary: approval.summary,
            turn: 0,
          }),
          runId: approval.runId,
          sessionId: approval.sessionId,
          executionTarget: approval.executionTarget,
          phase: "AwaitingApproval",
          summary: approval.summary,
          userMessage: approval.userMessage,
          pendingApproval: approval,
        },
      }));
    });

    const unlistenOutput = listen<TerminalAgentEvent>("terminal-agent-output", (event) => {
      if (event.payload.executionTarget === "ChatWindow") {
        return;
      }
      if (event.payload.kind !== "command_started") {
        return;
      }

      setTerminalAgentStates((prev) => {
        const current = prev[event.payload.sessionId];
        if (!current) {
          return prev;
        }
        return {
          ...prev,
          [event.payload.sessionId]: {
            ...current,
            currentCommand: event.payload.command,
          },
        };
      });
    });

    return () => {
      void unlistenStatus.then((fn) => fn());
      void unlistenApproval.then((fn) => fn());
      void unlistenOutput.then((fn) => fn());
    };
  }, []);

  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      const ctrl = e.ctrlKey || e.metaKey;
      const shift = e.shiftKey;

      if (ctrl && !shift && e.key === "t") {
        e.preventDefault();
        addTab();
      } else if (ctrl && shift && (e.key === "N" || e.key === "n")) {
        e.preventDefault();
        createAdditionalWindow();
      } else if (ctrl && !shift && e.key === "w") {
        e.preventDefault();
        if (activeTab) closeTab(activeTab);
      } else if (ctrl && shift && (e.key === "W" || e.key === "w")) {
        e.preventDefault();
        getCurrentWindow().close().catch(console.error);
      } else if (ctrl && shift && e.key === "D") {
        e.preventDefault();
        setShowDashboard((prev) => !prev);
      } else if (ctrl && shift && (e.key === "L" || e.key === "l")) {
        e.preventDefault();
        toggleMenuBarPreference(!showMenuBar);
      } else if (ctrl && !shift && e.key === "k") {
        e.preventDefault();
        setOpenDialog("quickConnect");
      } else if (ctrl && !shift && e.key === "o") {
        e.preventDefault();
        void handleOpenProjectDialog();
      } else if (ctrl && !shift && e.key === "s") {
        e.preventDefault();
        void handleSaveProject();
      } else if (ctrl && shift && e.key === "B") {
        e.preventDefault();
        setOpenDialog("backupCreate");
      } else if (ctrl && shift && (e.key === "Y" || e.key === "y")) {
        e.preventDefault();
        setOpenDialog("aiManager");
      } else if (ctrl && shift && (e.key === "E" || e.key === "e")) {
        e.preventDefault();
        toggleRecordingForActiveTab();
      } else if (ctrl && shift && (e.key === "J" || e.key === "j")) {
        e.preventDefault();
        setOpenDialog("jobScheduler");
      } else if (ctrl && shift && (e.key === "V" || e.key === "v")) {
        // Ctrl/Cmd+Shift+V is the terminal's paste binding. Don't hijack it
        // when focus is inside a terminal/xterm view or any editable element,
        // otherwise the user's paste is swallowed by the recordings dialog.
        if (eventTargetConsumesPaste(e.target)) {
          return;
        }
        e.preventDefault();
        setOpenDialog("terminalRecordings");
      } else if (ctrl && shift && (e.key === "S" || e.key === "s")) {
        e.preventDefault();
        setSnippetFileDraft(null);
        setOpenDialog("snippetManager");
      } else if (ctrl && e.altKey && !shift && e.code === "KeyA") {
        e.preventDefault();
        handleOpenAiAgentForFocusedSession();
      } else if (ctrl && e.altKey && !shift && e.code === "KeyP") {
        e.preventDefault();
        handleOpenAiAgentPlanForFocusedSession();
      } else if (ctrl && !shift && e.key === "q") {
        // Port of WindowCloseShortcutSupport: Ctrl+Q closes only secondary
        // windows; the primary main window ignores the shortcut.
        e.preventDefault();
        if (currentWindowLabel !== "main") {
          handleQuit();
        }
      } else if (ctrl && !shift && e.key === "Tab") {
        e.preventDefault();
        nextTab();
      } else if (ctrl && shift && e.key === "Tab") {
        e.preventDefault();
        prevTab();
      } else if (e.key === "F11") {
        e.preventDefault();
        handleFullscreen();
      } else if (e.key === "F12") {
        e.preventDefault();
        toggleTerminalOnlyFullscreen();
      } else if (
        ctrl &&
        shift &&
        (e.key === "=" || e.key === "+" || e.key === "Add" || e.code === "Equal" || e.code === "NumpadAdd")
      ) {
        e.preventDefault();
        e.stopPropagation();
        zoomAllInTabIn();
      } else if (
        ctrl &&
        !shift &&
        (e.key === "=" || e.key === "+" || e.key === "Add" || e.code === "Equal" || e.code === "NumpadAdd")
      ) {
        e.preventDefault();
        zoomIn();
      } else if (
        ctrl &&
        shift &&
        (e.key === "-" || e.key === "Subtract" || e.code === "Minus" || e.code === "NumpadSubtract")
      ) {
        e.preventDefault();
        e.stopPropagation();
        zoomAllInTabOut();
      } else if (
        ctrl &&
        !shift &&
        (e.key === "-" || e.key === "Subtract" || e.code === "Minus" || e.code === "NumpadSubtract")
      ) {
        e.preventDefault();
        zoomOut();
      } else if (ctrl && shift && (e.key === "0" || e.code === "Numpad0")) {
        e.preventDefault();
        e.stopPropagation();
        resetZoomAllInTab();
      } else if (ctrl && !shift && (e.key === "0" || e.code === "Numpad0")) {
        e.preventDefault();
        resetZoom();
      }
    }
    window.addEventListener("keydown", handleKeyDown, true);
    return () => window.removeEventListener("keydown", handleKeyDown, true);
  }, [
    addTab,
    createAdditionalWindow,
    closeTab,
    activeTab,
    currentWindowLabel,
    nextTab,
    prevTab,
    handleOpenProjectDialog,
    handleOpenAiAgentForFocusedSession,
    handleOpenAiAgentPlanForFocusedSession,
    handleSaveProject,
    showMenuBar,
    toggleMenuBarPreference,
    toggleRecordingForActiveTab,
    setOpenDialog,
    handleQuit,
    handleFullscreen,
    toggleTerminalOnlyFullscreen,
    zoomIn,
    zoomOut,
    resetZoom,
    zoomAllInTabIn,
    zoomAllInTabOut,
    resetZoomAllInTab,
  ]);

  const menuActions = {
    onNewWindow: createAdditionalWindow,
    onCloseWindow: () => {
      getCurrentWindow().close().catch(console.error);
    },
    onNewTab: addTab,
    onCloseTab: () => { if (activeTab) closeTab(activeTab); },
    onOpenProject: () => {
      void handleOpenProjectDialog();
    },
    onSaveProject: () => {
      void handleSaveProject();
    },
    onSaveProjectAs: () => {
      void handleSaveProjectAs();
    },
    onProjectSettings: handleEditProjectSettings,
    onToggleMenuBar: () => toggleMenuBarPreference(!showMenuBar),
    onToggleDashboard: () => setShowDashboard((prev) => !prev),
    onToggleLocalFileBrowser: toggleLocalFileBrowserPreference,
    onQuickConnect: () => setOpenDialog("quickConnect"),
    onManageConnections: () => setOpenDialog("connectionManager"),
    onImportConnections: () => setOpenDialog("importDialog"),
    onExportConnections: () => setOpenDialog("connectionExport"),
    onSettings: () => setOpenDialog("settings"),
    onManageCredentials: () => setOpenDialog("credentialManager"),
    onManageSSHKeys: () => setOpenDialog("sshKeyManager"),
    onManageGPGKeys: () => setOpenDialog("gpgKeyManager"),
    onAiManager: () => setOpenDialog("aiManager"),
    onAiAgent: handleOpenAiAgentForFocusedSession,
    onAiAgentPlan: handleOpenAiAgentPlanForFocusedSession,
    onSnippets: () => {
      setSnippetFileDraft(null);
      setOpenDialog("snippetManager");
    },
    onJobScheduler: () => setOpenDialog("jobScheduler"),
    onTerminalEffects: () => setOpenDialog("terminalEffects"),
    onTerminalRecordings: () => setOpenDialog("terminalRecordings"),
    onToggleRecording: toggleRecordingForActiveTab,
    onSFTPManager: () => setOpenDialog("sftpManager"),
    onAsciiArt: () => setOpenDialog("asciiArt"),
    onCreateBackup: () => setOpenDialog("backupCreate"),
    onImportBackup: () => setOpenDialog("backupImport"),
    onTeamworkSettings: () => setOpenDialog("teamworkSettings"),
    onTerminalThemeEditor: () => setOpenDialog("terminalThemeEditor"),
    onGuiThemeEditor: () => setOpenDialog("guiThemeEditor"),
    onFullscreen: handleFullscreen,
    onTerminalOnlyFullscreen: toggleTerminalOnlyFullscreen,
    hideTerminalScrollbarsInFullscreen,
    onToggleHideTerminalScrollbarsInFullscreen: toggleHideTerminalScrollbarsInFullscreen,
    onQuit: handleQuit,
    onAbout: () => setOpenDialog("about"),
  };

  if (!settingsReady) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-kortty-bg text-kortty-text">
        <div className="rounded-lg border border-kortty-border bg-kortty-surface px-6 py-5 text-sm shadow-2xl">
          Loading KorTTY settings...
        </div>
      </div>
    );
  }

  return (
    <div
      className={`flex flex-col h-screen w-screen bg-kortty-bg ${
        terminalOnlyFullscreen && hideTerminalScrollbarsInFullscreen
          ? "kortty-hide-terminal-scrollbars"
          : ""
      }`}
    >
      {showMenuBar && !terminalOnlyFullscreen && <MenuBar {...menuActions} />}
      <div className="flex flex-1 min-h-0">
        {showDashboard && !terminalOnlyFullscreen && (
          <div className="w-[300px] border-r border-kortty-border bg-kortty-surface flex-shrink-0 flex flex-col">
            <div className="px-3 py-2 text-xs font-semibold text-kortty-text-dim uppercase tracking-wider border-b border-kortty-border">
              Dashboard
            </div>
            <div className="p-3 border-b border-kortty-border space-y-2">
              <div className="flex items-center gap-2">
                <input
                  className="input-field"
                  value={windowName}
                  onChange={(e) => setWindowName(e.target.value)}
                  placeholder="Window name"
                />
              </div>
              <button
                className="w-full px-2 py-1.5 text-xs rounded bg-kortty-accent text-kortty-bg hover:bg-kortty-accent-hover transition-colors"
                onClick={createAdditionalWindow}
              >
                New Window
              </button>
            </div>
            <div className="flex-1 p-3 text-sm overflow-y-auto space-y-3">
              {workspaceWindowList.map((win) => (
                <div
                  key={win.label}
                  className={`border rounded transition-colors ${
                    dragOverWindowLabel === win.label
                      ? "border-kortty-accent bg-kortty-accent/5"
                      : "border-kortty-border"
                  }`}
                  onDragOver={(e) => handleWindowDragOver(win.label, e)}
                  onDragLeave={handleWindowDragLeave}
                  onDrop={(e) => {
                    void handleWindowDrop(win.label, e);
                  }}
                >
                  <div
                    className={`px-2 py-1.5 text-xs border-b border-kortty-border ${
                      win.label === currentWindowLabel
                        ? "bg-kortty-accent/10 text-kortty-accent"
                        : "text-kortty-text-dim"
                    }`}
                    onDoubleClick={() => focusWorkspaceWindow(win.label)}
                    title={win.label === currentWindowLabel ? "This window" : "Double-click to focus"}
                  >
                    {win.name}
                  </div>
                  <div className="p-1 space-y-1 min-h-[44px]">
                    {win.connections.length === 0 ? (
                      <div className="px-2 py-2 text-[11px] text-kortty-text-dim">No connections</div>
                    ) : (
                      win.connections.map((entry) => (
                        <div
                          key={`${win.label}-${entry.kind}-${entry.sessionId}`}
                          onDoubleClick={() => {
                            if (win.label === currentWindowLabel) {
                              setActiveTab(entry.tabId);
                              return;
                            }
                            focusWorkspaceWindow(win.label);
                            emitTo(win.label, "kortty-focus-connection", {
                              tabId: entry.tabId,
                            }).catch(console.error);
                          }}
                          className={`px-2 py-1.5 rounded text-[11px] ${
                            win.label === currentWindowLabel
                              ? "text-kortty-text hover:bg-kortty-panel"
                              : "text-kortty-text-dim"
                          }`}
                        >
                          <div className="flex items-center gap-2">
                            <span
                              className={`w-1.5 h-1.5 rounded-full ${
                                entry.status === "connected"
                                  ? "bg-kortty-success"
                                  : entry.status === "connecting"
                                    ? "bg-kortty-warning"
                                    : "bg-kortty-error"
                              }`}
                            />
                            <span className="truncate">{entry.label}</span>
                          </div>
                        </div>
                      ))
                    )}
                  </div>
                </div>
              ))}
            </div>
          </div>
        )}
        {localFileBrowserVisible && !terminalOnlyFullscreen && localFileBrowserDock === "left" && (
          <>
            <div
              className="kortty-resizable-filebrowser flex shrink-0 min-h-0"
              style={{ width: fileBrowserPanelWidth }}
            >
              <LocalFileBrowser
                dock="left"
                onClose={() => setLocalFileBrowserVisiblePreference(false)}
                onEditFile={(draft) => {
                  setSnippetFileDraft(draft);
                  setOpenDialog("snippetManager");
                }}
              />
            </div>
            <ResizableDivider
              orientation="vertical"
              onResize={(delta) =>
                setFileBrowserPanelWidth((width) =>
                  clampPanelSize(width + delta, FILE_BROWSER_MIN_WIDTH, FILE_BROWSER_MAX_WIDTH),
                )
              }
              onResizeEnd={() => storePanelSize(FILE_BROWSER_PANEL_WIDTH_KEY, fileBrowserPanelWidth)}
            />
          </>
        )}
        <div className="flex-1 flex flex-col min-w-0 min-h-0">
          {!terminalOnlyFullscreen && (
            <TabBar
              tabs={tabs}
              activeTab={activeTab}
              onTabChange={setActiveTab}
              onAddTab={addTab}
              onCloseTab={closeTab}
              onDuplicateTab={duplicateTab}
              onReconnectTab={(tabId) => handleReconnectTabAll(tabId)}
              onReorderTabs={reorderTabs}
              onTabTransferDragStart={handleTabTransferDragStart}
              onTabTransferDragEnd={handleTabTransferDragEnd}
              otherWindows={otherWorkspaceWindows}
              onMoveTabToWindow={handleMoveTabToWindow}
              onCopyTabToWindow={handleCopyTabToWindow}
              terminalEffects={terminalEffects}
              onSetTabTerminalEffect={setTabTerminalEffect}
              onSetTabTerminalEffectSpeed={setTabTerminalEffectSpeed}
            />
          )}
          <div
            className="flex-1 min-h-0 bg-kortty-terminal relative overflow-hidden"
            onDragOver={handleWindowRootDragOver}
            onDragLeave={handleWindowRootDragLeave}
            onDrop={handleWindowRootDrop}
          >
            {dragOverWindowLabel === currentWindowLabel && (
              <div className="pointer-events-none absolute inset-0 z-40 border-2 border-dashed border-kortty-accent bg-kortty-accent/5" />
            )}
            {tabs.map((tab) => {
              const terminalAppearance = getTabTheme(tab);
              const baseFontSize = tabFontSizes[tab.id] ?? terminalAppearance.fontSize ?? globalFontSize;

              return (
                <div
                  key={tab.id}
                  className="absolute inset-0"
                  style={{ display: tab.id === activeTab ? "block" : "none" }}
                >
                  {(tab.kind ?? "terminal") === "ai" ? (
                  tab.aiAgentPlanRequest ? (
                    <AiAgentPlanTab
                      tabId={tab.id}
                      initialRequest={tab.aiAgentPlanRequest}
                      initialRunId={tab.aiAgentPlanRunId || ""}
                      initialState={tab.aiAgentPlanInitialState}
                      onTitleChange={(title) => {
                        setTabs((prev) =>
                          prev.map((entry) => (entry.id === tab.id ? { ...entry, label: title || "AI Agent Plan" } : entry)),
                        );
                      }}
                      onStartExecution={async (runId) => {
                        await handleStartExecutionFromPlan(runId);
                      }}
                    />
                  ) : tab.aiAgentRequest ? (
                    <AiAgentRunTab
                      tabId={tab.id}
                      initialRequest={tab.aiAgentRequest}
                      initialRunId={tab.aiAgentRunId || ""}
                      onTitleChange={(title) => {
                        setTabs((prev) =>
                          prev.map((entry) => (entry.id === tab.id ? { ...entry, label: title || "AI Agent" } : entry)),
                        );
                      }}
                    />
                  ) : (
                    <AiChatTab
                      tabId={tab.id}
                      initialRequest={tab.aiInitialRequest}
                      initialChat={tab.aiSavedChat}
                      onTitleChange={(title) => {
                        setTabs((prev) =>
                          prev.map((entry) => (entry.id === tab.id ? { ...entry, label: title || "AI Chat" } : entry)),
                        );
                      }}
                      onSavedChatChange={(savedChat) => {
                        setTabs((prev) =>
                          prev.map((entry) =>
                            entry.id === tab.id
                              ? {
                                  ...entry,
                                  label: savedChat.title || "AI Chat",
                                  aiChatId: savedChat.id,
                                  aiSavedChat: savedChat,
                                }
                              : entry,
                          ),
                        );
                      }}
                    />
                  )
                  ) : tab.status === "connected" ? (
                    <TerminalSplitPane
                      primarySessionId={tab.id}
                      connected={true}
                      agentCommandName={terminalAgentCommandName}
                      agentCommandNameCaseInsensitive={terminalAgentCommandNameCaseInsensitive}
                      readOnly={!!tab.readOnlyMirror}
                      promptHookEnabled={promptHookEnabled}
                      agentPanelDock={terminalAgentPanelDock}
                      initialAgentPanelHeight={terminalAgentPanelHeight}
                      initialAgentPanelSideWidth={terminalAgentPanelSideWidth}
                      initialAgentPanelFontSize={terminalAgentPanelFontSize}
                      getAgentPanelLabel={(sessionId, splitIndex) => {
                        const displayName = resolveConnectionDisplayName(sessionId) ?? `Session ${sessionId.slice(0, 8)}`;
                        return `${displayName} · ${splitIndex === 0 ? "Main" : `Split ${splitIndex + 1}`}`;
                      }}
                      onAgentPanelLayoutChange={persistTerminalAgentPanelLayout}
                      initialSplitSessionIds={tabInitialSplitTree[tab.id] ? undefined : tabSplitSessions[tab.id]}
                      initialTree={tabInitialSplitTree[tab.id]}
                      onTreeChange={(tree) => setTabSplitTrees((prev) => ({ ...prev, [tab.id]: serializeSplitTree(tree) }))}
                      theme={terminalAppearance.theme}
                      fontFamily={terminalAppearance.fontFamily}
                      fontSize={baseFontSize}
                      getFontSizeForSession={(sessionId) =>
                        paneFontSizes[`${tab.id}:${sessionId}`] ??
                        tabFontSizes[tab.id] ??
                        terminalAppearance.fontSize ??
                        globalFontSize
                      }
                      getTerminalEffectPluginIdForSession={(sessionId) =>
                        sessionId === tab.id
                          ? tab.terminalEffectPluginId
                          : splitSessionConfigs[sessionId]?.terminalEffectPluginId
                      }
                      getTerminalEffectAnimationSpeedForSession={(sessionId) =>
                        sessionId === tab.id
                          ? tab.terminalEffectAnimationSpeed
                          : splitSessionConfigs[sessionId]?.terminalEffectAnimationSpeed
                      }
                      onFocusSession={(sessionId) => {
                        focusedPaneSessionRef.current = sessionId;
                      }}
                      onZoomIn={(sessionId) => {
                        const key = `${tab.id}:${sessionId}`;
                        const base = tabFontSizes[tab.id] ?? terminalAppearance.fontSize ?? globalFontSizeRef.current;
                        setPaneFontSizes((prev) => ({
                          ...prev,
                          [key]: Math.min(MAX_FONT_SIZE, (prev[key] ?? base) + 1),
                        }));
                      }}
                      onZoomOut={(sessionId) => {
                        const key = `${tab.id}:${sessionId}`;
                        const base = tabFontSizes[tab.id] ?? terminalAppearance.fontSize ?? globalFontSizeRef.current;
                        setPaneFontSizes((prev) => ({
                          ...prev,
                          [key]: Math.max(MIN_FONT_SIZE, (prev[key] ?? base) - 1),
                        }));
                      }}
                      onResetZoom={(sessionId) => {
                        setPaneFontSizes((prev) => {
                          const next = { ...prev };
                          delete next[`${tab.id}:${sessionId}`];
                          return next;
                        });
                      }}
                      onToggleTimestamps={() => setShowTimestamps((s) => !s)}
                      showTimestamps={showTimestamps}
                      recordings={recordingSessions}
                      onToggleRecording={(sessionId) => {
                        void toggleTerminalRecording(sessionId);
                      }}
                      onReconnect={(sessionId) => handleReconnect(sessionId)}
                      agentRunStates={terminalAgentStates}
                      onClosePrimarySplit={() => {
                        void handleClosePrimarySplit(tab.id);
                      }}
                      onAiAction={hasConfiguredAiProfiles ? ((sessionId, action, selectedText) => {
                        const splitConfig = splitSessionConfigs[sessionId];
                        const connectionDisplayName =
                          sessionId === tab.id
                            ? tab.label
                            : splitConfig
                              ? `${splitConfig.username}@${splitConfig.host}`
                              : tab.label;
                        void handleRequestAiAction({
                          action,
                          sessionId,
                          selectedText,
                          connectionDisplayName,
                        });
                      }) : undefined}
                      onOpenSelectionInSnippetEditor={(sessionId, selectedText) => {
                        void handleOpenTerminalSelectionInSnippetEditor(sessionId, selectedText);
                      }}
                      onStartAgent={hasConfiguredAiProfiles ? ((sessionId) => {
                        if (isReadOnlyMirrorSession(sessionId)) {
                          return;
                        }
                        void handleRequestTerminalAgent({
                          sessionId,
                          connectionDisplayName: resolveConnectionDisplayName(sessionId),
                        });
                      }) : undefined}
                      onStartAgentPlan={hasConfiguredAiProfiles ? ((sessionId) => {
                        if (isReadOnlyMirrorSession(sessionId)) {
                          return;
                        }
                        void handleRequestTerminalAgentPlan({
                          sessionId,
                          connectionDisplayName: resolveConnectionDisplayName(sessionId),
                        });
                      }) : undefined}
                      onOpenSnippetManager={() => {
                        setSnippetFileDraft(null);
                        setOpenDialog("snippetManager");
                      }}
                      onAgentCommand={(sessionId, rawCommand) => {
                        void handleTerminalAgentShortcut(sessionId, rawCommand);
                      }}
                      onApproveAgent={(approval) => {
                        void handleApproveTerminalAgent(approval);
                      }}
                      onApproveAgentAlways={(approval) => {
                        void handleApproveTerminalAgentAlways(approval);
                      }}
                      onSubmitAgentPassword={(request, password) => {
                        void handleSubmitTerminalAgentPassword(request, password);
                      }}
                      onStopAgent={(runId) => {
                        void handleStopTerminalAgent(runId);
                      }}
                      onCloseRequest={() => closeTab(tab.id)}
                      onSplitSameServer={() => handleSplitSameServer(tab.id)}
                      onSplitNewServer={() => handleSplitNewServer(tab.id)}
                      onDisconnectSplitSession={(sessionId) => handleDisconnectSplitSession(tab.id, sessionId)}
                    />
                  ) : (
                    <div className="w-full h-full flex items-center justify-center">
                      <div className="text-center text-kortty-text-dim text-sm">
                        {tab.status === "connecting" ? (
                          <div className="flex items-center gap-2">
                            <div className="animate-spin w-4 h-4 border-2 border-kortty-accent border-t-transparent rounded-full" />
                            Connecting...
                          </div>
                        ) : (
                          <p>Not connected. Use the connection manager to connect.</p>
                        )}
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
            {tabs.length === 0 && (
              <div className="w-full h-full flex items-center justify-center">
                <div className="text-center text-kortty-text-dim">
                  <div className="text-6xl mb-4 font-mono font-bold text-kortty-accent opacity-30">
                    KorTTY
                  </div>
                  <p className="text-sm">
                    Press{" "}
                    <kbd className="px-1.5 py-0.5 bg-kortty-panel rounded text-xs font-mono">
                      Ctrl+T
                    </kbd>{" "}
                    to open a new tab or{" "}
                    <kbd className="px-1.5 py-0.5 bg-kortty-panel rounded text-xs font-mono">
                      Ctrl+K
                    </kbd>{" "}
                    for Quick Connect
                  </p>
                </div>
              </div>
            )}
          </div>
          {localFileBrowserVisible && !terminalOnlyFullscreen && localFileBrowserDock === "bottom" && (
            <>
              <ResizableDivider
                orientation="horizontal"
                onResize={(delta) =>
                  setFileBrowserPanelHeight((height) =>
                    clampPanelSize(height - delta, FILE_BROWSER_MIN_HEIGHT, FILE_BROWSER_MAX_HEIGHT),
                  )
                }
                onResizeEnd={() =>
                  storePanelSize(FILE_BROWSER_PANEL_HEIGHT_KEY, fileBrowserPanelHeight)
                }
              />
              <div
                className="kortty-resizable-filebrowser flex shrink-0 min-w-0"
                style={{ height: fileBrowserPanelHeight }}
              >
                <LocalFileBrowser
                  dock="bottom"
                  onClose={() => setLocalFileBrowserVisiblePreference(false)}
                  onEditFile={(draft) => {
                    setSnippetFileDraft(draft);
                    setOpenDialog("snippetManager");
                  }}
                />
              </div>
            </>
          )}
        </div>
        {localFileBrowserVisible && !terminalOnlyFullscreen && localFileBrowserDock === "right" && (
          <>
            <ResizableDivider
              orientation="vertical"
              onResize={(delta) =>
                setFileBrowserPanelWidth((width) =>
                  clampPanelSize(width - delta, FILE_BROWSER_MIN_WIDTH, FILE_BROWSER_MAX_WIDTH),
                )
              }
              onResizeEnd={() => storePanelSize(FILE_BROWSER_PANEL_WIDTH_KEY, fileBrowserPanelWidth)}
            />
            <div
              className="kortty-resizable-filebrowser flex shrink-0 min-h-0"
              style={{ width: fileBrowserPanelWidth }}
            >
              <LocalFileBrowser
                dock="right"
                onClose={() => setLocalFileBrowserVisiblePreference(false)}
                onEditFile={(draft) => {
                  setSnippetFileDraft(draft);
                  setOpenDialog("snippetManager");
                }}
              />
            </div>
          </>
        )}
      </div>
      {!terminalOnlyFullscreen && <StatusBar connectionCount={connectionCount} />}
      {/* WP1.3c: transient notice for the terminal text-file load flow. */}
      {terminalFileLoadNotice && (
        <div className="fixed bottom-10 right-4 z-[120] max-w-[420px] rounded border border-kortty-border bg-kortty-surface px-3 py-2 text-xs text-kortty-text shadow-2xl">
          {terminalFileLoadNotice}
        </div>
      )}

      <QuickConnect
        open={openDialog === "quickConnect"}
        onClose={() => setOpenDialog(null)}
        onConnect={handleQuickConnect}
      />
      <ConnectionManager
        open={openDialog === "connectionManager"}
        onClose={() => {
          if (splitResolveRef.current) {
            splitResolveRef.current(null);
            splitResolveRef.current = null;
            splitTabRef.current = null;
          }
          setOpenDialog(null);
        }}
        onConnect={connectFromSettings}
        onEdit={(conn) => {
          setEditingConnection(conn);
          setOpenDialog("connectionEditor");
        }}
      />
      {editingConnection && (
        <ConnectionEditor
          open={openDialog === "connectionEditor"}
          connection={editingConnection}
          onClose={() => {
            setOpenDialog(null);
            setEditingConnection(null);
          }}
          onSave={() => {
            setOpenDialog(null);
            setEditingConnection(null);
            loadConnections();
          }}
        />
      )}
      <SettingsDialog
        open={openDialog === "settings"}
        onClose={() => setOpenDialog(null)}
        onSaved={(settings) => {
          setShowMenuBar(settings.showMenuBar);
          setShowTimestamps(settings.defaultCommandTimestampsEnabled);
          setPromptHookEnabled(settings.defaultPromptHookEnabled !== false);
          setTerminalAgentCommandName(
            normalizeTerminalAgentCommandName(settings.terminalAgentCommandName),
          );
          setTerminalAgentCommandNameCaseInsensitive(!!settings.terminalAgentCommandNameCaseInsensitive);
          setTerminalAgentShowRunDialog(settings.terminalAgentShowRunDialog !== false);
          setTerminalAgentRememberPanelLayout(!!settings.terminalAgentRememberPanelLayout);
          setTerminalAgentPanelDock(settings.terminalAgentPanelDock ?? "bottom");
          setTerminalAgentPanelHeight(settings.terminalAgentPanelHeight);
          setTerminalAgentPanelSideWidth(settings.terminalAgentPanelSideWidth);
          setTerminalAgentPanelFontSize(settings.terminalAgentPanelFontSize);
          setLocalFileBrowserDock(settings.localFileBrowserDock ?? "left");
          setLocalFileBrowserVisible(!!settings.localFileBrowserVisible);
          setHideTerminalScrollbarsInFullscreen(!!settings.hideTerminalScrollbarsInFullscreen);
          terminalAgentPanelLayoutRef.current = {
            terminalAgentPanelDock: settings.terminalAgentPanelDock ?? "bottom",
            terminalAgentPanelHeight: settings.terminalAgentPanelHeight,
            terminalAgentPanelSideWidth: settings.terminalAgentPanelSideWidth,
            terminalAgentPanelFontSize: settings.terminalAgentPanelFontSize,
          };
          setTerminalAgentExecutionTarget(settings.terminalAgentExecutionTarget);
        }}
      />
      <AiActionDialog
        open={openDialog === "aiAction"}
        action={pendingAiAction?.action ?? null}
        selectedText={pendingAiAction?.selectedText ?? ""}
        connectionDisplayName={pendingAiAction?.connectionDisplayName}
        onClose={() => {
          setOpenDialog(null);
          setPendingAiAction(null);
        }}
        onManageProfiles={() => setOpenDialog("aiManager")}
        onRun={(request) =>
          createAiTab({
            ...request,
            connectionAiProfileId: pendingAiAction?.connectionAiProfileId,
            connectionAiSkillIds: pendingAiAction?.connectionAiSkillIds,
          })
        }
      />
      <AiAgentDialog
        open={openDialog === "aiAgent"}
        sessionId={pendingTerminalAgentAction?.sessionId}
        connectionDisplayName={pendingTerminalAgentAction?.connectionDisplayName}
        initialPrompt={pendingTerminalAgentAction?.initialPrompt}
        initialProfileId={
          pendingTerminalAgentAction?.connectionAiProfileId
          || pendingTerminalAgentAction?.initialProfileId
        }
        initialExecutionTarget={pendingTerminalAgentAction?.initialExecutionTarget}
        initialAskConfirmationBeforeEveryCommand={
          pendingTerminalAgentAction?.initialAskConfirmationBeforeEveryCommand
        }
        initialAutoApproveRootCommands={pendingTerminalAgentAction?.initialAutoApproveRootCommands}
        connectionAiProfileId={pendingTerminalAgentAction?.connectionAiProfileId}
        connectionAiSkillIds={pendingTerminalAgentAction?.connectionAiSkillIds}
        onClose={() => {
          setOpenDialog(null);
          setPendingTerminalAgentAction(null);
          setPendingTerminalAgentMode(null);
        }}
        onManageProfiles={() => setOpenDialog("aiManager")}
        onRun={handleLaunchTerminalAgentTask}
      />
      <AiAgentPlanDialog
        open={openDialog === "aiAgentPlan"}
        sessionId={pendingTerminalAgentAction?.sessionId}
        connectionDisplayName={pendingTerminalAgentAction?.connectionDisplayName}
        onClose={() => {
          setOpenDialog(null);
          setPendingTerminalAgentAction(null);
          setPendingTerminalAgentMode(null);
        }}
        onManageProfiles={() => setOpenDialog("aiManager")}
        onRun={(request) =>
          handleLaunchTerminalAgentPlanTask({
            ...request,
            connectionAiProfileId: pendingTerminalAgentAction?.connectionAiProfileId,
            connectionAiSkillIds: pendingTerminalAgentAction?.connectionAiSkillIds,
          })
        }
      />
      <AiManagerDialog
        open={openDialog === "aiManager"}
        onClose={() => {
          void handleCloseAiManager();
        }}
        onOpenChat={handleOpenSavedAiChat}
      />
      <Suspense fallback={null}>
        <JobSchedulerDialog
          open={openDialog === "jobScheduler"}
          onClose={() => setOpenDialog(null)}
        />
        <TerminalEffectManagerDialog
          open={openDialog === "terminalEffects"}
          onClose={() => {
            void reloadTerminalEffects();
            setOpenDialog(null);
          }}
        />
        <TerminalRecordingManagerDialog
          open={openDialog === "terminalRecordings"}
          onClose={() => setOpenDialog(null)}
        />
      </Suspense>
      <CredentialManager
        open={openDialog === "credentialManager"}
        onClose={() => setOpenDialog(null)}
      />
      <SSHKeyManager
        open={openDialog === "sshKeyManager"}
        onClose={() => setOpenDialog(null)}
      />
      <GPGKeyManager
        open={openDialog === "gpgKeyManager"}
        onClose={() => setOpenDialog(null)}
      />
      <Suspense fallback={null}>
        <SnippetManager
          open={openDialog === "snippetManager"}
          fileDraft={snippetFileDraft}
          onFileDraftSave={saveSnippetFileDraft}
          onClose={() => {
            setOpenDialog(null);
            setSnippetFileDraft(null);
          }}
        />
      </Suspense>
      <AsciiArtBanner
        open={openDialog === "asciiArt"}
        onClose={() => setOpenDialog(null)}
      />
      <BackupDialog
        open={openDialog === "backupCreate" || openDialog === "backupImport"}
        onClose={() => setOpenDialog(null)}
      />
      <TeamworkSettingsDialog
        open={openDialog === "teamworkSettings"}
        onClose={() => {
          setOpenDialog(null);
          loadConnections();
        }}
      />
      <ImportDialog
        open={openDialog === "importDialog"}
        onClose={() => setOpenDialog(null)}
      />
      <ConnectionExportDialog
        open={openDialog === "connectionExport"}
        onClose={() => setOpenDialog(null)}
      />
      <ThemeEditor
        open={openDialog === "terminalThemeEditor"}
        onClose={() => {
          setOpenDialog(null);
          loadActiveTheme();
          invoke<import("../store/themeStore").ThemeData[]>("get_themes")
            .then(setAllThemes)
            .catch(console.error);
        }}
      />
      <GuiThemeEditor
        open={openDialog === "guiThemeEditor"}
        onClose={() => setOpenDialog(null)}
      />
      <Suspense fallback={null}>
        <SFTPManager
          open={openDialog === "sftpManager"}
          onClose={() => setOpenDialog(null)}
          sessionId={activeTerminalSessionId}
          onEditFile={(draft) => {
            setSnippetFileDraft(draft);
            setOpenDialog("snippetManager");
          }}
        />
      </Suspense>
      <ProjectPreviewDialog
        open={openDialog === "projectPreview"}
        project={projectPreview}
        onClose={() => {
          setOpenDialog(null);
          setProjectPreview(null);
        }}
        onOpenProject={(autoReconnect) => {
          if (!projectPreview) return;
          void applyProjectToWorkspace(projectPreview, autoReconnect)
            .then(() => {
              setOpenDialog(null);
              setProjectPreview(null);
            })
            .catch((error) => {
              console.error("Failed to open project:", error);
            });
        }}
      />
      <ProjectSettingsDialog
        open={openDialog === "projectSettings"}
        project={projectSettingsDraft}
        connectionCount={buildProjectSnapshot(projectSettingsDraft).connectionIds.length}
        onClose={() => {
          setOpenDialog(null);
          setProjectSettingsDraft(null);
        }}
        onSave={(project) => {
          const mode = projectSaveModeRef.current;
          if (mode === "edit" && currentProject?.filePath) {
            void saveProjectToPath(project, false)
              .then(() => {
                setOpenDialog(null);
                setProjectSettingsDraft(null);
              })
              .catch((error) => {
                console.error("Failed to update project settings:", error);
              });
            return;
          }

          if (mode === "edit") {
            setCurrentProject(buildProjectSnapshot(project));
            setOpenDialog(null);
            setProjectSettingsDraft(null);
            return;
          }

          void saveProjectToPath(project, mode === "saveAs")
            .then(() => {
              setOpenDialog(null);
              setProjectSettingsDraft(null);
            })
            .catch((error) => {
              console.error("Failed to save project:", error);
            });
        }}
      />

      {openDialog === "about" && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-50">
          <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[480px] overflow-hidden">
            <div className="flex flex-col items-center px-8 py-8">
              <img
                src={korttyLogo}
                alt="KorTTY"
                className="mb-4 w-full max-w-[360px] select-none"
                draggable={false}
              />
              <div className="text-xs text-kortty-text-dim mb-6">SSH Terminal Client</div>
              <div className="space-y-1 text-center text-xs text-kortty-text">
                <p>Version 2.2.0</p>
                <p className="text-kortty-text-dim">Built with Tauri + React + Rust</p>
              </div>
              <div className="mt-6 text-center text-[11px] text-kortty-text-dim space-y-0.5">
                <p>&copy; 2024-2026 Daniel Mengel</p>
                <a
                  href="https://github.com/chardonnay/korTTY_rust"
                  target="_blank"
                  rel="noreferrer"
                  className="text-kortty-accent hover:underline"
                >
                  github.com/chardonnay/korTTY_rust
                </a>
              </div>
              <div className="mt-6 w-full border-t border-kortty-border pt-4">
                <div className="flex items-center gap-3">
                  <button
                    className="px-3 py-1.5 text-xs rounded border border-kortty-border text-kortty-text hover:bg-kortty-panel transition-colors disabled:opacity-50"
                    disabled={updateCheckBusy}
                    onClick={() => {
                      void runManualUpdateCheck();
                    }}
                  >
                    {t("updates.checkNow")}
                  </button>
                  {updateCheckBusy && (
                    <div className="animate-spin w-4 h-4 border-2 border-kortty-accent border-t-transparent rounded-full" />
                  )}
                </div>
                {updateCheckStatus && (
                  <p className="mt-2 text-[11px] text-kortty-text-dim break-words">
                    {updateCheckStatus}
                  </p>
                )}
              </div>
            </div>
            <div className="border-t border-kortty-border px-4 py-3 flex justify-center">
              <button
                onClick={() => setOpenDialog(null)}
                className="px-6 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
              >
                OK
              </button>
            </div>
          </div>
        </div>
      )}

      <TerminalRecordingScopeDialog
        open={recordingScopeRequest != null}
        defaultScope={recordingScopeRequest?.defaultScope ?? "ActiveSplit"}
        onCancel={() => setRecordingScopeRequest(null)}
        onConfirm={(scope) => {
          const request = recordingScopeRequest;
          setRecordingScopeRequest(null);
          if (request) {
            void startTerminalRecordingWithScope(request.tabId, request.targetSessionId, scope);
          }
        }}
      />

      {recordingErrorMessage != null && (
        <div className="fixed inset-0 bg-black/60 flex items-center justify-center z-[120]">
          <div className="bg-kortty-bg border border-kortty-border rounded-lg shadow-2xl w-[400px] overflow-hidden">
            <div className="px-4 py-3 border-b border-kortty-border">
              <div className="text-sm font-semibold text-kortty-text">
                {t("terminal.recording.error.title")}
              </div>
              <div className="text-xs text-kortty-text-dim mt-0.5">
                {t("terminal.recording.error.header")}
              </div>
            </div>
            <div className="px-4 py-4 text-xs text-kortty-text break-words">
              {recordingErrorMessage}
            </div>
            <div className="border-t border-kortty-border px-4 py-3 flex justify-end">
              <button
                className="px-5 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
                onClick={() => setRecordingErrorMessage(null)}
              >
                {t("common.ok")}
              </button>
            </div>
          </div>
        </div>
      )}

      <UpdateAvailableDialog
        open={availableUpdate != null}
        update={availableUpdate}
        manual={updateDialogManual}
        onClose={() => setAvailableUpdate(null)}
        onDownload={(update) => setUpdateDownload(update)}
      />
      <UpdateDownloadDialog
        open={updateDownload != null}
        update={updateDownload}
        onClose={() => setUpdateDownload(null)}
      />
    </div>
  );
}
