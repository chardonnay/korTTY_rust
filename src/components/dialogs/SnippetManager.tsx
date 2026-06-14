import { useState, useEffect, useMemo, useRef } from "react";
import {
  X,
  Plus,
  Trash2,
  Edit,
  FileCode,
  Star,
  StarOff,
  Upload,
  Download,
  Bot,
  ChevronDown,
  ChevronRight,
  CopyPlus,
  GitCompareArrows,
  Palette,
  PanelLeftClose,
  PanelLeftOpen,
  Maximize2,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import { useTranslation } from "react-i18next";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useSettingsStore } from "../../store/settingsStore";
import type { GlobalSettings } from "../../store/settingsStore";
import type { AiExecutionResult, AiProfile, AiRequestPayload } from "../../types/ai";
import type { SnippetCodeReference, SnippetEditorProfile, SnippetHistoryEntry } from "../../types/snippet";
import { resolvePreferredAiProfileId } from "../../utils/aiProfiles";
import { DEFAULT_AI_LANGUAGE_CODE, resolveGuiLanguageCode } from "../../utils/aiLanguage";
import {
  CURRENT_SETTINGS_PROFILE_ID,
  allProfiles,
  builtInProfiles,
  customProfiles,
  normalizeProfile,
  resolveActiveProfile,
} from "../../utils/snippetEditorProfiles";
import { ensureProfileTheme } from "../../utils/monacoProfileTheme";
import { canDiff, orderedPair } from "../../utils/snippetDiffSelection";
import { MonacoSnippetEditor } from "../editor/MonacoSnippetEditor";
import type {
  MonacoContextMenuEvent,
  MonacoLayoutInfo,
  MonacoSelectionInfo,
  MonacoSnippetEditorHandle,
} from "../editor/MonacoSnippetEditor";
import { SnippetColumnRuler } from "../editor/SnippetColumnRuler";
import { SnippetDiffDialog } from "./SnippetDiffDialog";
import { SnippetEditorProfileDialog } from "./SnippetEditorProfileDialog";
import { SnippetAiAssistDialog } from "./snippet/SnippetAiAssistDialog";
import { SnippetDiagramDialog } from "./snippet/SnippetDiagramDialog";
import { SnippetAiDiffDialog } from "./snippet/SnippetAiDiffDialog";
import { SnippetAiReviewDialog } from "./snippet/SnippetAiReviewDialog";
import { SnippetSecurityReportDialog } from "./snippet/SnippetSecurityReportDialog";
import { AlternativeSnippetSolutionsDialog } from "./snippet/AlternativeSnippetSolutionsDialog";
import { SnippetDescriptionDialog } from "./snippet/SnippetDescriptionDialog";
import {
  type SnippetAiSession,
  applySnippetSecurityFixes,
  assistSnippetCode,
  completeSnippetCode,
  correctSelectionText,
  describeSnippet,
  extractEditableSegments,
  generateAlternativeSolutions,
  improveSnippetCode,
  resolveSnippetAiSession,
  reviewSnippetCode,
  reviewSnippetSecurity,
  translateSelectionText,
} from "../../utils/snippetAiWorkflows";
import {
  type CodeReviewFinding,
  type SecurityFinding,
  isUsableCompletion,
  isUsableImprovement,
} from "../../utils/snippetAiResponse";
import {
  detectSnippetLanguage,
  findLineIndentation,
  firstContentOffset,
  startOfLine,
} from "../../utils/snippetLanguageComments";
import {
  type RemoteSiblingFileNameError,
  validateRemoteSiblingFileName,
} from "../../utils/remoteTextFileSelection";
import { AI_LANGUAGE_OPTIONS } from "../../utils/aiLanguage";

const LANGUAGES: { value: string; label: string }[] = [
  { value: "bash", label: "Bash / Shell" },
  { value: "shell", label: "Shell" },
  { value: "sh", label: "sh" },
  { value: "zsh", label: "zsh" },
  { value: "c", label: "C" },
  { value: "cpp", label: "C++" },
  { value: "css", label: "CSS" },
  { value: "html", label: "HTML" },
  { value: "java", label: "Java" },
  { value: "javascript", label: "JavaScript" },
  { value: "json", label: "JSON" },
  { value: "markdown", label: "Markdown" },
  { value: "php", label: "PHP" },
  { value: "python", label: "Python" },
  { value: "rust", label: "Rust" },
  { value: "sql", label: "SQL" },
  { value: "typescript", label: "TypeScript" },
  { value: "xml", label: "XML" },
  { value: "yaml", label: "YAML" },
  { value: "plain", label: "Plain Text" },
];

export interface SnippetVariable {
  name: string;
  defaultValue: string;
  description?: string;
}

export interface Snippet {
  id: string;
  name: string;
  content: string;
  category?: string;
  description?: string;
  language?: string;
  favorite: boolean;
  diagrams: SnippetDiagram[];
  editorProfileId?: string;
  variables: SnippetVariable[];
  history: SnippetHistoryEntry[];
}

export interface SnippetDiagram {
  id: string;
  name: string;
  diagramType: "PlantUml";
  source: string;
  renderedPath?: string;
  contentHash?: string;
  title?: string;
  customInstructions?: string;
  codeReferences?: SnippetCodeReference[];
  createdAt?: number;
  updatedAt?: number;
}

interface SnippetManagerProps {
  open: boolean;
  onClose: () => void;
  fileDraft?: SnippetFileDraft | null;
  onFileDraftSave?: (content: string) => Promise<void> | void;
}

export interface SnippetFileDraft {
  id: string;
  source: "local" | "remote";
  path: string;
  sessionId?: string;
  content: string;
}

const PACKAGE_FORMATS = ["JSON", "XML", "YAML"] as const;
const IMPORT_FORMATS = [...PACKAGE_FORMATS, "Plaintext"] as const;
const RUNTIME_EXPORT_FORMATS = ["ScriptFiles", "Zip", "AesZip", "GpgZip"] as const;
const FILE_RUNTIME_EXPORT_FORMATS = ["Zip", "AesZip", "GpgZip"] as const;
type SnippetPackageFormat = (typeof PACKAGE_FORMATS)[number];
type SnippetImportFormat = (typeof IMPORT_FORMATS)[number];
type RuntimeExportFormat = (typeof RUNTIME_EXPORT_FORMATS)[number];
type FileRuntimeExportFormat = (typeof FILE_RUNTIME_EXPORT_FORMATS)[number];
type SnippetExportFormat = SnippetPackageFormat | RuntimeExportFormat;
type SnippetFileExportFormat = SnippetPackageFormat | FileRuntimeExportFormat;
type TransferDialogMode = "import" | "export";

const SNIPPET_IMPORT_OPTIONS: { value: SnippetImportFormat; label: string }[] = [
  { value: "JSON", label: "JSON (.json)" },
  { value: "XML", label: "XML (.xml)" },
  { value: "YAML", label: "YAML (.yaml, .yml)" },
  { value: "Plaintext", label: "Plaintext" },
];

const SNIPPET_EXPORT_OPTIONS: { value: SnippetExportFormat; label: string }[] = [
  { value: "JSON", label: "JSON (.json)" },
  { value: "XML", label: "XML (.xml)" },
  { value: "YAML", label: "YAML (.yaml, .yml)" },
  { value: "ScriptFiles", label: "Script files (folder)" },
  { value: "Zip", label: "ZIP (.zip)" },
  { value: "AesZip", label: "AES ZIP (.aes.zip)" },
  { value: "GpgZip", label: "GPG ZIP (.zip.gpg)" },
];
const SNIPPET_CATEGORY_MAX_LENGTH = 30;

interface SnippetMetadataSuggestion {
  name?: string;
  category?: string;
  description?: string;
}

function normalizeSnippetCategory(value?: string | null): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) return undefined;
  return trimmed.slice(0, SNIPPET_CATEGORY_MAX_LENGTH);
}

function normalizeSnippetDescription(value?: string | null): string | undefined {
  const normalized = value?.replace(/\r\n?/g, "\n").trim();
  return normalized || undefined;
}

function normalizeSnippetForSave(snippet: Snippet): Snippet {
  return {
    ...snippet,
    category: normalizeSnippetCategory(snippet.category),
    description: normalizeSnippetDescription(snippet.description),
    diagrams: snippet.diagrams ?? [],
    editorProfileId: snippet.editorProfileId,
    variables: snippet.variables ?? [],
    history: snippet.history ?? [],
  };
}

function isFileRuntimeExportFormat(format: SnippetFileExportFormat): format is FileRuntimeExportFormat {
  return (FILE_RUNTIME_EXPORT_FORMATS as readonly string[]).includes(format);
}

function isRuntimeExportFormat(format: SnippetExportFormat): format is RuntimeExportFormat {
  return (RUNTIME_EXPORT_FORMATS as readonly string[]).includes(format);
}

function importDialogFilter(format: SnippetImportFormat) {
  switch (format) {
    case "JSON":
      return { name: "JSON snippets", extensions: ["json"] };
    case "XML":
      return { name: "XML snippets", extensions: ["xml"] };
    case "YAML":
      return { name: "YAML snippets", extensions: ["yaml", "yml"] };
    case "Plaintext":
      return {
        name: "Plaintext",
        extensions: [
          "txt",
          "text",
          "log",
          "md",
          "sh",
          "bash",
          "zsh",
          "py",
          "js",
          "ts",
          "rs",
          "java",
          "json",
          "xml",
          "yaml",
          "yml",
          "sql",
          "css",
          "html",
          "php",
          "c",
          "cpp",
        ],
      };
  }
}

function exportDialogFilter(format: SnippetFileExportFormat) {
  switch (format) {
    case "JSON":
      return { name: "JSON snippets", extensions: ["json"] };
    case "XML":
      return { name: "XML snippets", extensions: ["xml"] };
    case "YAML":
      return { name: "YAML snippets", extensions: ["yaml", "yml"] };
    case "Zip":
      return { name: "Snippet scripts ZIP", extensions: ["zip"] };
    case "AesZip":
      return { name: "AES encrypted ZIP", extensions: ["zip"] };
    case "GpgZip":
      return { name: "GPG encrypted ZIP", extensions: ["gpg"] };
  }
}

function exportDefaultPath(format: SnippetFileExportFormat): string {
  switch (format) {
    case "JSON":
      return "snippets.json";
    case "XML":
      return "snippets.xml";
    case "YAML":
      return "snippets.yaml";
    case "Zip":
      return "snippets.zip";
    case "AesZip":
      return "snippets.aes.zip";
    case "GpgZip":
      return "snippets.zip.gpg";
  }
}

function ensureExportPathExtension(path: string, format: SnippetFileExportFormat): string {
  const lowerPath = path.toLowerCase();
  switch (format) {
    case "JSON":
      return lowerPath.endsWith(".json") ? path : `${path}.json`;
    case "XML":
      return lowerPath.endsWith(".xml") ? path : `${path}.xml`;
    case "YAML":
      return lowerPath.endsWith(".yaml") || lowerPath.endsWith(".yml") ? path : `${path}.yaml`;
    case "Zip":
      return lowerPath.endsWith(".zip") ? path : `${path}.zip`;
    case "AesZip":
      if (lowerPath.endsWith(".aes.zip")) return path;
      if (lowerPath.endsWith(".zip")) return path.replace(/\.zip$/i, ".aes.zip");
      return `${path}.aes.zip`;
    case "GpgZip":
      if (lowerPath.endsWith(".zip.gpg") || lowerPath.endsWith(".gpg")) return path;
      if (lowerPath.endsWith(".zip")) return `${path}.gpg`;
      return `${path}.zip.gpg`;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function readStringField(source: Record<string, unknown>, key: string): string | undefined {
  const value = source[key];
  return typeof value === "string" ? value : undefined;
}

function tryParseJson(value: string): unknown | null {
  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

function parseSnippetMetadataSuggestion(raw: string): SnippetMetadataSuggestion {
  const candidates = [
    raw.trim(),
    raw.match(/```(?:json)?\s*([\s\S]*?)```/i)?.[1]?.trim(),
    raw.slice(raw.indexOf("{"), raw.lastIndexOf("}") + 1).trim(),
  ].filter((candidate): candidate is string => !!candidate);

  for (const candidate of candidates) {
    const parsed = tryParseJson(candidate);
    if (!isRecord(parsed)) {
      continue;
    }
    return {
      name: readStringField(parsed, "name"),
      category: readStringField(parsed, "category"),
      description: readStringField(parsed, "description"),
    };
  }

  throw new Error("AI response did not contain a valid JSON object");
}

function buildSnippetMetadataPrompt(responseLanguageCode: string): string {
  // WP2.6: hardened like the Java snippet-action prompts.
  return [
    "Analyze this script snippet and infer suitable metadata for a snippet library.",
    "Return exactly one JSON object and no Markdown.",
    'Schema: {"name":"...","category":"...","description":"..."}',
    `Use language code ${responseLanguageCode} for human-readable values.`,
    `category must be at most ${SNIPPET_CATEGORY_MAX_LENGTH} characters.`,
    "description must contain at least two concise lines separated by \\n.",
    "Do not include hidden reasoning, analysis, or <think> tags.",
    "Do not invent external facts, files, or URLs; use only what is visible in the script.",
    "Do not add explanations outside the JSON object.",
  ].join("\n");
}

// WP2.9: snake_case-free mirror of code_formatter::FormatterInfo (camelCase serde).
interface SnippetFormatterInfo {
  language: string;
  formatterId?: string;
  displayName: string;
  providerType: "builtIn" | "bundled" | "externalFallback" | "unavailable";
  installHint?: string;
  available: boolean;
  unavailableReason?: string;
}

const LINE_WIDTH_FORMATTERS = new Set(["prettier", "black", "perltidy"]);

function formatterSupportsLineWidth(info: SnippetFormatterInfo | null): boolean {
  return !!info?.formatterId && LINE_WIDTH_FORMATTERS.has(info.formatterId);
}

interface SnippetAiDiffPreview {
  title: string;
  summary: string;
  original: string;
  replacement: string;
  applyStart: number;
  applyEnd: number;
  appliedStatus: string;
}

interface AlternativesRequestState {
  wholeSnippet: boolean;
  start: number;
  end: number;
  targetText: string;
  fullContent: string;
}

interface DescriptionDialogState {
  description: string;
  insertOffset: number;
  indentation: string;
}

function normalizeSnippetContent(rawContent: string): string {
  const lines = rawContent.replace(/\r\n?/g, "\n").split("\n");

  while (lines.length > 0 && lines[0].trim() === "") {
    lines.shift();
  }
  while (lines.length > 0 && lines[lines.length - 1].trim() === "") {
    lines.pop();
  }

  if (lines.length === 0) {
    return "";
  }

  const commonIndent = lines.reduce((minIndent, line) => {
    if (line.trim() === "") {
      return minIndent;
    }

    const indent = line.match(/^[\t ]*/)?.[0].length ?? 0;
    return Math.min(minIndent, indent);
  }, Number.POSITIVE_INFINITY);

  const indentToStrip = Number.isFinite(commonIndent) ? commonIndent : 0;
  return lines
    .map((line) => {
      if (line.trim() === "") {
        return "";
      }

      const leadingWhitespace = line.match(/^[\t ]*/)?.[0].length ?? 0;
      return line.slice(Math.min(indentToStrip, leadingWhitespace));
    })
    .join("\n")
    .trim();
}

function parseSnippetsXml(content: string): Snippet[] {
  const parser = new DOMParser();
  const document = parser.parseFromString(content, "application/xml");
  if (document.querySelector("parsererror")) {
    throw new Error("Invalid XML");
  }

  return Array.from(document.querySelectorAll("snippet")).map((element) => ({
    id: element.getAttribute("id") || crypto.randomUUID(),
    name: element.getAttribute("name") || "",
    category: normalizeSnippetCategory(element.getAttribute("category")),
    description: normalizeSnippetDescription(
      element.querySelector("description")?.textContent ?? element.getAttribute("description"),
    ),
    language: element.getAttribute("language") || "bash",
    favorite: element.getAttribute("favorite") === "true",
    content: normalizeSnippetContent(element.querySelector("content")?.textContent ?? ""),
    diagrams: [],
    editorProfileId: undefined,
    history: [],
    variables: Array.from(element.querySelectorAll("variables > variable")).map((variable) => ({
      name: variable.getAttribute("name") || "",
      defaultValue: variable.getAttribute("defaultValue") || "",
      description: variable.getAttribute("description") || undefined,
    })),
  }));
}

function escapeXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function snippetsToXml(snippets: Snippet[]): string {
  const body = snippets.map((snippet) => {
    const variables = snippet.variables.map((variable) => (
      `      <variable name="${escapeXml(variable.name)}" defaultValue="${escapeXml(variable.defaultValue)}"${
        variable.description ? ` description="${escapeXml(variable.description)}"` : ""
      } />`
    )).join("\n");

    return [
      `  <snippet id="${escapeXml(snippet.id)}" name="${escapeXml(snippet.name)}"${
        snippet.category ? ` category="${escapeXml(snippet.category)}"` : ""
      }${snippet.language ? ` language="${escapeXml(snippet.language)}"` : ""} favorite="${snippet.favorite}">`,
      snippet.description ? `    <description>${escapeXml(snippet.description)}</description>` : "",
      `    <content>${escapeXml(snippet.content)}</content>`,
      "    <variables>",
      variables,
      "    </variables>",
      "  </snippet>",
    ].filter(Boolean).join("\n");
  }).join("\n");

  return `<?xml version="1.0" encoding="UTF-8"?>\n<snippets>\n${body}\n</snippets>\n`;
}

function newSnippet(): Snippet {
  return {
    id: crypto.randomUUID(),
    name: "",
    content: "",
    category: undefined,
    description: undefined,
    language: "bash",
    favorite: false,
    diagrams: [],
    editorProfileId: undefined,
    variables: [],
    history: [],
  };
}

function languageFromPath(path: string): string {
  const extension = path.split(".").pop()?.toLowerCase();
  switch (extension) {
    case "sh":
    case "bash":
    case "zsh":
      return "bash";
    case "py":
      return "python";
    case "js":
      return "javascript";
    case "ts":
      return "typescript";
    case "rs":
      return "rust";
    case "java":
      return "java";
    case "json":
      return "json";
    case "xml":
      return "xml";
    case "yaml":
    case "yml":
      return "yaml";
    case "sql":
      return "sql";
    case "md":
      return "markdown";
    default:
      return "plain";
  }
}

function snippetNameFromPath(path: string): string {
  const fileName = path.split(/[\\/]/).filter(Boolean).pop()?.trim();
  if (!fileName) return "Imported plaintext";
  return fileName.replace(/\.[^.]+$/, "") || fileName;
}

export function SnippetManager({ open, onClose, fileDraft, onFileDraftSave }: SnippetManagerProps) {
  const { t } = useTranslation();
  const { width, height, onResizeStart } = useDialogGeometry("snippet-manager", 720, 520, 480, 360);
  const { settings, loadSettings, saveSettings } = useSettingsStore();
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<Snippet | null>(null);
  const [categoryFilter, setCategoryFilter] = useState("");
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [aiMetadataLoading, setAiMetadataLoading] = useState(false);
  const [aiMetadataStatus, setAiMetadataStatus] = useState<string | null>(null);
  const [importExportStatus, setImportExportStatus] = useState<string | null>(null);
  const [transferDialogMode, setTransferDialogMode] = useState<TransferDialogMode | null>(null);
  const [importFormat, setImportFormat] = useState<SnippetImportFormat>("JSON");
  const [exportFormat, setExportFormat] = useState<SnippetExportFormat>("JSON");
  const [markedSnippetIds, setMarkedSnippetIds] = useState<Set<string>>(() => new Set());
  const [runtimeToolStatus, setRuntimeToolStatus] = useState<string | null>(null);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [metadataCollapsed, setMetadataCollapsed] = useState(false);
  // WP2.1: column ruler state fed by MonacoSnippetEditor callbacks.
  const [caretColumn, setCaretColumn] = useState(1);
  const [caretVisualX, setCaretVisualX] = useState<number>(Number.NaN);
  const [editorLayout, setEditorLayout] = useState<MonacoLayoutInfo>({
    contentLeft: 0,
    charWidth: 8,
    scrollLeft: 0,
  });
  const [limitColumn, setLimitColumn] = useState(0);
  // WP2.4: per-editing-session content history.
  const [contentHistory, setContentHistory] = useState<SnippetHistoryEntry[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const contentHistoryRef = useRef<SnippetHistoryEntry[]>([]);
  const historySnippetIdRef = useRef<string | null>(null);
  const historyDebounceRef = useRef<number | null>(null);
  const navigatingHistoryRef = useRef(false);
  // WP2.3 / WP2.5: profile management and diff dialogs.
  const [profileDialogOpen, setProfileDialogOpen] = useState(false);
  const [diffDialogOpen, setDiffDialogOpen] = useState(false);
  // WP2.10: snippet diagram dialog.
  const [diagramDialogOpen, setDiagramDialogOpen] = useState(false);
  // WP1.3: file-draft split save menu and remote "save as" dialog.
  const [fileSaveMenuOpen, setFileSaveMenuOpen] = useState(false);
  const [remoteSaveAsName, setRemoteSaveAsName] = useState<string | null>(null);
  // Distraction-free fullscreen editor for the currently edited snippet.
  const [fullscreenOpen, setFullscreenOpen] = useState(false);
  const fullscreenEditorRef = useRef<MonacoSnippetEditorHandle | null>(null);
  // WP2.6-2.9: snippet AI workflows.
  const editorRef = useRef<MonacoSnippetEditorHandle | null>(null);
  const selectionRef = useRef<MonacoSelectionInfo>({
    selectionStart: 0,
    selectionEnd: 0,
    caretOffset: 0,
    caretLine: 1,
    caretColumn: 1,
    caretVisualX: Number.NaN,
  });
  const [aiBusy, setAiBusy] = useState(false);
  const aiBusyRef = useRef(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [assistDialogOpen, setAssistDialogOpen] = useState(false);
  const [assistCursor, setAssistCursor] = useState({ offset: 0, line: 1, column: 1 });
  const [assistSkillsAvailable, setAssistSkillsAvailable] = useState(false);
  const [aiDiffPreview, setAiDiffPreview] = useState<SnippetAiDiffPreview | null>(null);
  const [reviewFindings, setReviewFindings] = useState<CodeReviewFinding[] | null>(null);
  const [securityFindings, setSecurityFindings] = useState<SecurityFinding[] | null>(null);
  const [alternativesRequest, setAlternativesRequest] = useState<AlternativesRequestState | null>(null);
  const [descriptionDialog, setDescriptionDialog] = useState<DescriptionDialogState | null>(null);
  const [translateDialogOpen, setTranslateDialogOpen] = useState(false);
  const [translateLanguage, setTranslateLanguage] = useState(AI_LANGUAGE_OPTIONS[0]?.value ?? "en");
  // WP2.8: session-scoped auto completion (default off, never persisted).
  const [autoCompleteEnabled, setAutoCompleteEnabled] = useState(false);
  const autoCompletionWarningAcceptedRef = useRef(false);
  const lastAutoCompletionKeyRef = useRef<string | null>(null);
  const autoCompleteTimerRef = useRef<number | null>(null);
  // WP2.9: formatter provider status for the Format button.
  const [formatterInfo, setFormatterInfo] = useState<SnippetFormatterInfo | null>(null);

  // WP2.3: active editor profile (theme, cursor). A snippet can pin its own
  // editor profile via editorProfileId; that selection takes precedence over
  // the global selectedSnippetEditorProfileId, which stays the fallback.
  const snippetProfileId = editing?.editorProfileId;
  const activeProfile = useMemo(() => {
    if (snippetProfileId && snippetProfileId.trim()) {
      const pinned = allProfiles(settings).find((entry) => entry.id === snippetProfileId);
      if (pinned) return normalizeProfile(pinned);
    }
    return resolveActiveProfile(settings);
  }, [settings, snippetProfileId]);
  const editorTheme = useMemo(() => ensureProfileTheme(activeProfile), [activeProfile]);
  const editorFontFamily = settings.snippetFontFamily?.trim() ? settings.snippetFontFamily : undefined;
  const editorFontSize =
    settings.snippetFontSize && settings.snippetFontSize > 0 ? settings.snippetFontSize : 12;
  const availableCustomProfiles = useMemo(() => customProfiles(settings), [settings]);
  const availableBuiltInProfiles = useMemo(() => builtInProfiles(), []);

  useEffect(() => {
    if (open) {
      loadSnippets();
      void loadSettings();
    }
  }, [open]);

  useEffect(() => {
    if (!open || !fileDraft) return;
    const fileName = fileDraft.path.split("/").filter(Boolean).pop() || "file";
    const draftSnippet: Snippet = {
      id: `file-${fileDraft.id}`,
      name: fileName,
      content: fileDraft.content,
      category: fileDraft.source === "remote" ? "Remote file" : "Local file",
      description: fileDraft.path,
      language: languageFromPath(fileDraft.path),
      favorite: false,
      diagrams: [],
      editorProfileId: undefined,
      variables: [],
      history: [],
    };
    setSelectedId(draftSnippet.id);
    setEditing(draftSnippet);
    setRuntimeToolStatus(`Editing ${fileDraft.source} file: ${fileDraft.path}`);
  }, [open, fileDraft]);

  useEffect(() => {
    if (selectedId && !editing) {
      const s = snippets.find((x) => x.id === selectedId);
      setEditing(s ? { ...s } : null);
    } else if (!selectedId) {
      setEditing(null);
    }
  }, [selectedId, snippets, editing]);

  // Close the fullscreen editor when the manager closes or nothing is being
  // edited, and let Escape exit it from anywhere.
  useEffect(() => {
    if (!fullscreenOpen) return;
    if (!open || !editing) {
      setFullscreenOpen(false);
      return;
    }
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        setFullscreenOpen(false);
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [fullscreenOpen, open, editing]);

  useEffect(() => {
    setMarkedSnippetIds((current) => {
      if (current.size === 0) return current;
      const existingIds = new Set(snippets.map((snippet) => snippet.id));
      const retained = new Set([...current].filter((id) => existingIds.has(id)));
      return retained.size === current.size ? current : retained;
    });
  }, [snippets]);

  // ---- WP2.4: content history (port of SnippetEditDialog history handling) ----

  function effectiveHistoryMaxSize(): number {
    const size = settings.snippetHistoryMaxSize;
    if (!size || size <= 0) return Number.POSITIVE_INFINITY; // 0 means unlimited
    return Math.max(1, Math.min(99, size));
  }

  function trimHistoryEntries(entries: SnippetHistoryEntry[]): SnippetHistoryEntry[] {
    const maxSize = effectiveHistoryMaxSize();
    if (!Number.isFinite(maxSize) || entries.length <= maxSize) {
      return entries;
    }
    // Oldest entries are removed first.
    return entries.slice(entries.length - maxSize);
  }

  function setHistoryState(entries: SnippetHistoryEntry[], index: number) {
    contentHistoryRef.current = entries;
    setContentHistory(entries);
    setHistoryIndex(index);
  }

  function clearHistoryDebounce() {
    if (historyDebounceRef.current !== null) {
      window.clearTimeout(historyDebounceRef.current);
      historyDebounceRef.current = null;
    }
  }

  function appendHistoryEntry(content: string) {
    if (!content.trim()) return;
    const current = contentHistoryRef.current;
    if (current.length > 0 && current[current.length - 1].content === content) {
      setHistoryIndex(current.length - 1);
      return;
    }
    const next = trimHistoryEntries([...current, { content, timestamp: Date.now() }]);
    setHistoryState(next, next.length - 1);
  }

  function flushPendingHistory() {
    clearHistoryDebounce();
    if (editing) {
      appendHistoryEntry(editing.content ?? "");
    }
  }

  function buildHistoryForSave(content: string): SnippetHistoryEntry[] {
    clearHistoryDebounce();
    let entries = [...contentHistoryRef.current];
    if (content.trim() && (entries.length === 0 || entries[entries.length - 1].content !== content)) {
      entries = [...entries, { content, timestamp: Date.now() }];
    }
    return trimHistoryEntries(entries);
  }

  function navigateToHistoryEntry(index: number) {
    const entries = contentHistoryRef.current;
    if (index < 0 || index >= entries.length) return;
    setHistoryIndex(index);
    const historicalContent = entries[index].content;
    setEditing((current) => {
      if (!current || current.content === historicalContent) return current;
      navigatingHistoryRef.current = true;
      return { ...current, content: historicalContent };
    });
  }

  function restoreHistoryEntry() {
    const entries = contentHistoryRef.current;
    const index = Math.max(0, Math.min(historyIndex, entries.length - 1));
    if (index < 0 || index >= entries.length) return;
    const entry = entries[index];
    setEditing((current) => (current ? { ...current, content: entry.content } : current));
    appendHistoryEntry(entry.content);
  }

  // Re-initialize the history whenever a different snippet enters the editor.
  useEffect(() => {
    const id = editing?.id ?? null;
    if (historySnippetIdRef.current === id) return;
    historySnippetIdRef.current = id;
    clearHistoryDebounce();
    navigatingHistoryRef.current = false;
    setLimitColumn(0);
    if (!editing) {
      setHistoryState([], -1);
      return;
    }
    const entries: SnippetHistoryEntry[] = (editing.history ?? [])
      .filter((entry) => !!entry && typeof entry.content === "string")
      .map((entry) => ({ content: entry.content, timestamp: entry.timestamp ?? Date.now() }));
    const currentContent = editing.content ?? "";
    if (entries.length === 0 || entries[entries.length - 1].content !== currentContent) {
      entries.push({ content: currentContent, timestamp: Date.now() });
    }
    const trimmed = trimHistoryEntries(entries);
    setHistoryState(trimmed, trimmed.length - 1);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing]);

  // Debounced history tracking (500 ms) while the user edits content.
  useEffect(() => {
    if (!editing) return;
    if (navigatingHistoryRef.current) {
      navigatingHistoryRef.current = false;
      return;
    }
    if (historySnippetIdRef.current !== editing.id) return;
    const content = editing.content ?? "";
    clearHistoryDebounce();
    const timer = window.setTimeout(() => {
      historyDebounceRef.current = null;
      appendHistoryEntry(content);
    }, 500);
    historyDebounceRef.current = timer;
    return () => {
      window.clearTimeout(timer);
      if (historyDebounceRef.current === timer) {
        historyDebounceRef.current = null;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing?.content, editing?.id]);

  // WP2.8: 900 ms typing-pause trigger for the session auto completion.
  useEffect(() => {
    if (!open || !autoCompleteEnabled || !editing) return;
    const content = editing.content ?? "";
    if (!content.trim()) return;
    if (autoCompleteTimerRef.current !== null) {
      window.clearTimeout(autoCompleteTimerRef.current);
    }
    const timer = window.setTimeout(() => {
      autoCompleteTimerRef.current = null;
      if (aiBusyRef.current) return;
      const caret = editorRef.current?.getCursorPosition() ?? { offset: 0, line: 1, column: 1 };
      const key = `${editorRef.current?.getValue() ?? content}::${caret.offset}`;
      if (key === lastAutoCompletionKeyRef.current) return;
      lastAutoCompletionKeyRef.current = key;
      void runCompletion(true);
    }, 900);
    autoCompleteTimerRef.current = timer;
    return () => {
      window.clearTimeout(timer);
      if (autoCompleteTimerRef.current === timer) {
        autoCompleteTimerRef.current = null;
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing?.content, autoCompleteEnabled, open]);

  // WP2.9: keep the formatter provider status label current per language.
  useEffect(() => {
    if (!open || !editing) {
      setFormatterInfo(null);
      return;
    }
    let cancelled = false;
    invoke<SnippetFormatterInfo | null>("get_snippet_formatter_info", {
      language: editing.language || "plain",
    })
      .then((info) => {
        if (!cancelled) setFormatterInfo(info);
      })
      .catch(() => {
        if (!cancelled) setFormatterInfo(null);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, editing?.language]);

  // Reset transient AI dialog state when a different snippet enters the editor.
  useEffect(() => {
    lastAutoCompletionKeyRef.current = null;
    setAiDiffPreview(null);
    setReviewFindings(null);
    setSecurityFindings(null);
    setAlternativesRequest(null);
    setDescriptionDialog(null);
    setTranslateDialogOpen(false);
    setAssistDialogOpen(false);
    setContextMenu(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editing?.id]);

  async function loadSnippets(): Promise<Snippet[]> {
    setLoading(true);
    try {
      const s = await invoke<Snippet[]>("get_snippets");
      const normalized = s.map(normalizeSnippetForSave);
      setSnippets(normalized);
      if (!selectedId && normalized.length > 0) setSelectedId(normalized[0].id);
      if (selectedId && !normalized.find((x) => x.id === selectedId)) setSelectedId(normalized[0]?.id ?? null);
      return normalized;
    } catch (err) {
      console.error("Failed to load snippets:", err);
      return [];
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    try {
      if (fileDraft && onFileDraftSave) {
        await onFileDraftSave(editing.content);
        setRuntimeToolStatus(`Saved ${fileDraft.source} file: ${fileDraft.path}`);
      } else {
        const history = buildHistoryForSave(editing.content ?? "");
        setHistoryState(history, history.length - 1);
        await invoke("save_snippet", {
          snippet: normalizeSnippetForSave({ ...editing, history }),
        });
        setEditing((current) => (current ? { ...current, history } : current));
        await loadSnippets();
      }
    } catch (err) {
      console.error("Failed to save snippet:", err);
      setRuntimeToolStatus(`Save failed: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  // WP2.11: clone the current snippet into a brand-new one (fresh id, empty
  // history, "(Copy)" name suffix) — mirrors SnippetEditDialog saveAsNew.
  async function handleSaveAsNew() {
    if (!editing || fileDraft) return;
    setSaving(true);
    try {
      const clone: Snippet = {
        ...normalizeSnippetForSave(editing),
        id: crypto.randomUUID(),
        name: `${editing.name.trim()}${t("snippet.copySuffix")}`,
        favorite: false,
        history: [],
      };
      await invoke("save_snippet", { snippet: clone });
      await loadSnippets();
      setSelectedId(clone.id);
      setEditing({ ...clone });
    } catch (err) {
      console.error("Failed to save snippet as new:", err);
      setRuntimeToolStatus(`Save failed: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string) {
    try {
      await invoke("delete_snippet", { id });
      setMarkedSnippetIds((current) => {
        if (!current.has(id)) return current;
        const next = new Set(current);
        next.delete(id);
        return next;
      });
      // Use the freshly reloaded list (the stale `snippets` state still
      // contains the just-deleted snippet and could re-select it).
      const refreshed = await loadSnippets();
      if (selectedId === id) {
        setSelectedId(refreshed.find((s) => s.id !== id)?.id ?? null);
      }
    } catch (err) {
      console.error("Failed to delete snippet:", err);
    }
  }

  function setSnippetMarked(id: string, marked: boolean) {
    setMarkedSnippetIds((current) => {
      const next = new Set(current);
      if (marked) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
  }

  async function handleToggleFavorite(id: string) {
    const s = snippets.find((x) => x.id === id);
    if (!s) return;
    const updated = { ...s, favorite: !s.favorite };
    try {
      await invoke("save_snippet", { snippet: normalizeSnippetForSave(updated) });
      await loadSnippets();
    } catch (err) {
      console.error("Failed to toggle favorite:", err);
    }
  }

  async function handleGenerateMetadata() {
    if (!editing?.content.trim()) {
      setAiMetadataStatus("Snippet content is required for AI metadata analysis.");
      return;
    }

    setAiMetadataLoading(true);
    setAiMetadataStatus("Analyzing snippet with AI...");
    try {
      const [profiles, settings] = await Promise.all([
        invoke<AiProfile[]>("get_ai_profiles"),
        invoke<GlobalSettings>("get_settings").catch(() => null),
      ]);
      const profileId = resolvePreferredAiProfileId(profiles, settings?.defaultAiProfileId);
      if (!profileId) {
        setAiMetadataStatus("No AI profile configured.");
        return;
      }

      const responseLanguageCode = resolveGuiLanguageCode(settings) || DEFAULT_AI_LANGUAGE_CODE;
      const request: AiRequestPayload = {
        action: "Ask",
        profileId,
        selectedText: editing.content,
        responseLanguageCode,
        userPrompt: buildSnippetMetadataPrompt(responseLanguageCode),
      };
      const result = await invoke<AiExecutionResult>("execute_ai_action", {
        request,
        requestId: crypto.randomUUID(),
      });
      const suggestion = parseSnippetMetadataSuggestion(result.content);

      setEditing((current) => {
        if (!current) return current;
        return {
          ...current,
          name: suggestion.name?.trim() || current.name,
          category: normalizeSnippetCategory(suggestion.category) ?? current.category,
          description: normalizeSnippetDescription(suggestion.description) ?? current.description,
        };
      });
      setAiMetadataStatus("AI metadata applied. Review and save the snippet.");
    } catch (error) {
      setAiMetadataStatus(`AI metadata failed: ${String(error)}`);
    } finally {
      setAiMetadataLoading(false);
    }
  }

  async function handleImport(format: SnippetImportFormat) {
    setImportExportStatus(null);
    try {
      const path = await openDialog({
        title: "Import snippets",
        multiple: false,
        directory: false,
        filters: [importDialogFilter(format)],
      });
      if (!path || typeof path !== "string") return;
      const content = await readTextFile(path);
      let imported: Snippet[] = [];
      if (format === "Plaintext") {
        imported = [
          {
            id: crypto.randomUUID(),
            name: snippetNameFromPath(path),
            content,
            category: undefined,
            description: undefined,
            language: languageFromPath(path),
            favorite: false,
            diagrams: [],
            editorProfileId: undefined,
            variables: [],
            history: [],
          },
        ];
      } else if (format === "JSON") {
        imported = JSON.parse(content);
      } else if (format === "YAML") {
        try {
          const yaml = await import("yaml");
          imported = yaml.parse(content) ?? [];
        } catch {
          setImportExportStatus("YAML package not installed");
          return;
        }
      } else {
        imported = parseSnippetsXml(content);
      }
      if (!Array.isArray(imported)) {
        setImportExportStatus("Invalid file format");
        return;
      }
      for (const s of imported) {
        if (s.id && s.name) {
          await invoke("save_snippet", { snippet: normalizeSnippetForSave(s) });
        }
      }
      setImportExportStatus(`Imported ${imported.length} snippets`);
      await loadSnippets();
    } catch (err) {
      setImportExportStatus(`Import failed: ${String(err)}`);
    }
  }

  async function handleExport(format: SnippetExportFormat) {
    setImportExportStatus(null);
    try {
      const snippetsToExport = snippetsForExport(isRuntimeExportFormat(format) ? "active" : "all");
      if (snippetsToExport.length === 0) {
        setImportExportStatus("No snippets selected for export");
        return;
      }
      if (format === "ScriptFiles") {
        await handleExportRuntime(format, undefined, snippetsToExport);
        return;
      }
      const path = await saveDialog({
        title: "Export snippets",
        defaultPath: exportDefaultPath(format),
        filters: [exportDialogFilter(format)],
      });
      if (!path) return;
      const targetPath = ensureExportPathExtension(path, format);
      if (isFileRuntimeExportFormat(format)) {
        await handleExportRuntime(format, targetPath, snippetsToExport);
        return;
      }
      let content = "";
      if (format === "JSON") {
        content = JSON.stringify(snippetsToExport, null, 2);
      } else if (format === "YAML") {
        try {
          const yaml = await import("yaml");
          content = yaml.stringify(snippetsToExport);
        } catch {
          setImportExportStatus("YAML package not installed");
          return;
        }
      } else {
        content = snippetsToXml(snippetsToExport);
      }
      await writeTextFile(targetPath, content);
      setImportExportStatus(`Exported to ${targetPath}`);
    } catch (err) {
      setImportExportStatus(`Export failed: ${String(err)}`);
    }
  }

  // WP2.9: format with local formatter, falling back to an AI-assisted format
  // (IMPROVE_SNIPPET_CODE with a format theme) when the formatter is missing,
  // unavailable, or cannot honor the requested line width. Port of
  // SnippetEditDialog.runFormat / confirmAiFormatFallback / runAiFormat.
  async function handleFormatSnippet(maxLineLength?: number) {
    if (!editing) return;
    setRuntimeToolStatus(null);
    const language = editing.language || "plain";
    let info: SnippetFormatterInfo | null = null;
    try {
      info = await invoke<SnippetFormatterInfo | null>("get_snippet_formatter_info", { language });
    } catch (error) {
      console.error("Failed to load formatter info:", error);
    }
    setFormatterInfo(info);
    if (!info) {
      await maybeRunAiFormat(t("snippet.ai.format.notSupported", { language }), maxLineLength);
      return;
    }
    if (maxLineLength !== undefined && !formatterSupportsLineWidth(info)) {
      await maybeRunAiFormat(t("snippet.ai.format.widthNotSupported", { language }), maxLineLength);
      return;
    }
    if (!info.available) {
      await maybeRunAiFormat(
        t("snippet.ai.format.providerUnavailable", { name: info.displayName }),
        maxLineLength,
      );
      return;
    }
    try {
      const result = await invoke<{
        content: string;
        formatterName: string;
        usedExternalFormatter: boolean;
      }>("format_snippet", {
        request: {
          content: editing.content,
          language: editing.language,
          maxLineLength,
        },
      });
      setEditing((current) => (current ? { ...current, content: result.content } : null));
      setRuntimeToolStatus(
        maxLineLength !== undefined
          ? t("snippet.ruler.formatSuccess", { limit: maxLineLength })
          : `Formatted with ${result.formatterName}.`,
      );
    } catch (error) {
      setRuntimeToolStatus(`Format failed: ${String(error)}`);
    }
  }

  // WP2.1: format the snippet to the line-length limit set in the ruler.
  async function handleFormatToLimit() {
    if (!editing || limitColumn <= 0) return;
    await handleFormatSnippet(limitColumn);
  }

  async function maybeRunAiFormat(reason: string, maxLineLength?: number) {
    const session = await resolveSnippetAiSession().catch(() => null);
    if (!session) {
      setRuntimeToolStatus(t("snippet.ai.format.unavailable", { reason }));
      return;
    }
    const confirmed = window.confirm(
      `${t("snippet.ai.format.fallbackTitle")}\n\n${t("snippet.ai.format.fallbackHeader")}\n\n${t(
        "snippet.ai.format.fallbackContent",
        { reason },
      )}`,
    );
    if (!confirmed) {
      setRuntimeToolStatus(reason);
      return;
    }
    await runAiFormat(session, maxLineLength);
  }

  async function runAiFormat(session: SnippetAiSession, maxLineLength?: number) {
    const fullContent = editing?.content ?? "";
    if (!fullContent.trim()) return;
    // With a line-width request the whole snippet is formatted; otherwise an
    // active selection limits the AI format scope (Java AiFormatScope).
    const selection = selectionRef.current;
    const selectionOnly =
      maxLineLength === undefined && selection.selectionEnd > selection.selectionStart;
    const replacementStart = selectionOnly ? selection.selectionStart : 0;
    const replacementEnd = selectionOnly ? selection.selectionEnd : fullContent.length;
    const targetText = fullContent.slice(replacementStart, replacementEnd);
    if (!targetText.trim()) return;
    const theme =
      maxLineLength !== undefined
        ? t("snippet.ai.format.widthTheme", { width: maxLineLength })
        : t("snippet.ai.format.theme");
    await runAiTask(t("snippet.ai.format.running"), async () => {
      const improvement = await improveSnippetCode(
        session,
        fullContent,
        targetText,
        snippetAiLanguage(),
        theme,
        undefined,
        true,
      );
      if (!isUsableImprovement(improvement)) {
        setRuntimeToolStatus(t("snippet.ai.format.empty"));
        return;
      }
      setAiDiffPreview({
        title: t("snippet.ai.format.diffTitle"),
        summary: improvement.summary,
        original: targetText,
        replacement: improvement.replacement,
        applyStart: replacementStart,
        applyEnd: replacementEnd,
        appliedStatus: t("snippet.ai.format.applied"),
      });
      setRuntimeToolStatus(null);
    }, t("snippet.ai.format.failed"));
  }

  // WP2.9: small provider-status label next to the Format button.
  function formatterStatusLabel(): string {
    if (!formatterInfo || formatterInfo.providerType === "unavailable" || !formatterInfo.available) {
      return t("snippet.formatter.unavailable");
    }
    switch (formatterInfo.providerType) {
      case "builtIn":
        return t("snippet.formatter.builtIn");
      case "bundled":
        return t("snippet.formatter.bundled");
      default:
        return t("snippet.formatter.external");
    }
  }

  // ---- WP2.6/2.7/2.8: snippet AI workflows ----

  function snippetAiLanguage(): string {
    return detectSnippetLanguage(editing?.language, editing?.content);
  }

  function setAiBusyState(busy: boolean) {
    aiBusyRef.current = busy;
    setAiBusy(busy);
  }

  /** Shared run wrapper: busy flag + running/failed status handling. */
  async function runAiTask(
    runningStatus: string,
    task: () => Promise<void>,
    failedStatus: string,
  ) {
    if (aiBusyRef.current) return;
    setAiBusyState(true);
    setRuntimeToolStatus(runningStatus);
    try {
      await task();
    } catch (error) {
      console.error("Snippet AI action failed:", error);
      setRuntimeToolStatus(`${failedStatus} ${String(error)}`);
    } finally {
      setAiBusyState(false);
    }
  }

  async function requireAiSession(): Promise<SnippetAiSession | null> {
    try {
      const session = await resolveSnippetAiSession();
      if (!session) {
        setRuntimeToolStatus(t("snippet.ai.noProfile"));
      }
      return session;
    } catch (error) {
      setRuntimeToolStatus(`${t("snippet.ai.noProfile")} ${String(error)}`);
      return null;
    }
  }

  /**
   * Port of SnippetEditDialog.ensureSnippetAiDataNoticeAccepted: only the
   * continuously-sending auto completion needs a one-time confirmation.
   */
  function ensureSnippetAiDataNoticeAccepted(autoCompletion: boolean): boolean {
    if (autoCompletion && !autoCompletionWarningAcceptedRef.current) {
      const accepted = window.confirm(
        `${t("snippet.ai.autocomplete.warningTitle")}\n\n${t(
          "snippet.ai.autocomplete.warningHeader",
        )}\n\n${t("snippet.ai.autocomplete.warningContent")}`,
      );
      if (!accepted) {
        return false;
      }
      autoCompletionWarningAcceptedRef.current = true;
    }
    return true;
  }

  function applyAiContentChange(start: number, end: number, replacement: string) {
    const editor = editorRef.current;
    if (editor) {
      editor.replaceRange(start, end, replacement);
      editor.selectRange(start, start + replacement.length);
      return;
    }
    setEditing((current) => {
      if (!current) return current;
      const content = current.content ?? "";
      const safeStart = Math.max(0, Math.min(start, content.length));
      const safeEnd = Math.max(safeStart, Math.min(end, content.length));
      return { ...current, content: content.slice(0, safeStart) + replacement + content.slice(safeEnd) };
    });
  }

  function applyAiDiffPreview() {
    const preview = aiDiffPreview;
    if (!preview) return;
    setAiDiffPreview(null);
    applyAiContentChange(preview.applyStart, preview.applyEnd, preview.replacement);
    setRuntimeToolStatus(preview.appliedStatus);
  }

  function hasEditorSelection(): boolean {
    const selection = selectionRef.current;
    return selection.selectionEnd > selection.selectionStart;
  }

  // WP2.7: AI assistant (whole snippet, cursor as focal point).
  async function openAiAssistDialog() {
    if (!editing?.content.trim() || aiBusy) return;
    const session = await requireAiSession();
    if (!session) return;
    setAssistSkillsAvailable(session.skillsAvailable);
    setAssistCursor(editorRef.current?.getCursorPosition() ?? { offset: 0, line: 1, column: 1 });
    setAssistDialogOpen(true);
  }

  async function runAiAssist(instruction: string, includeAiSkills: boolean) {
    setAssistDialogOpen(false);
    if (!editing || !ensureSnippetAiDataNoticeAccepted(false)) return;
    const fullContent = editing.content ?? "";
    if (!fullContent.trim()) return;
    const session = await requireAiSession();
    if (!session) return;
    const cursor = assistCursor;
    await runAiTask(t("snippet.ai.assistant.running"), async () => {
      const improvement = await assistSnippetCode(
        session,
        fullContent,
        snippetAiLanguage(),
        cursor,
        instruction,
        includeAiSkills,
      );
      if (!isUsableImprovement(improvement)) {
        setRuntimeToolStatus(t("snippet.ai.assistant.empty"));
        return;
      }
      setAiDiffPreview({
        title: t("snippet.ai.assistant.diffTitle"),
        summary: improvement.summary,
        original: fullContent,
        replacement: improvement.replacement,
        applyStart: 0,
        applyEnd: fullContent.length,
        appliedStatus: t("snippet.ai.assistant.applied"),
      });
      setRuntimeToolStatus(null);
    }, t("snippet.ai.assistant.failed"));
  }

  // WP2.8: manual + automatic AI completion shown as Monaco ghost text.
  async function runCompletion(autoCompletion: boolean) {
    if (!editing || aiBusyRef.current) return;
    if (!ensureSnippetAiDataNoticeAccepted(autoCompletion)) {
      if (autoCompletion) {
        setAutoCompleteEnabled(false);
      }
      return;
    }
    const contentSnapshot = editing.content ?? "";
    if (!contentSnapshot.trim()) return;
    const session = await requireAiSession();
    if (!session) return;
    const caret = editorRef.current?.getCursorPosition() ?? { offset: 0, line: 1, column: 1 };
    await runAiTask(t("snippet.ai.complete.running"), async () => {
      const suggestion = await completeSnippetCode(
        session,
        contentSnapshot,
        caret.offset,
        snippetAiLanguage(),
      );
      if (!isUsableCompletion(suggestion)) {
        setRuntimeToolStatus(t("snippet.ai.complete.empty"));
        return;
      }
      const editor = editorRef.current;
      if (
        !editor ||
        editor.getValue() !== contentSnapshot ||
        editor.getCursorPosition().offset !== caret.offset
      ) {
        setRuntimeToolStatus(t("snippet.ai.complete.discarded"));
        return;
      }
      editor.showInlineSuggestion(suggestion.insertText);
      setRuntimeToolStatus(t("snippet.ai.complete.ready"));
    }, t("snippet.ai.complete.failed"));
  }

  function handleAutoCompletionToggle() {
    if (!autoCompleteEnabled) {
      if (!ensureSnippetAiDataNoticeAccepted(true)) {
        return;
      }
      lastAutoCompletionKeyRef.current = null;
      setAutoCompleteEnabled(true);
      setRuntimeToolStatus(t("snippet.ai.autocomplete.enabled"));
    } else {
      setAutoCompleteEnabled(false);
      editorRef.current?.dismissInlineSuggestion();
      setRuntimeToolStatus(t("snippet.ai.autocomplete.disabled"));
    }
  }

  // WP2.7: code review with line navigation back into the editor.
  async function runCodeReview() {
    if (!editing?.content.trim() || aiBusy || !ensureSnippetAiDataNoticeAccepted(false)) return;
    const session = await requireAiSession();
    if (!session) return;
    const fullContent = editing.content ?? "";
    const selection = selectionRef.current;
    const wholeSnippet = !hasEditorSelection();
    const selectedText = wholeSnippet
      ? fullContent
      : fullContent.slice(selection.selectionStart, selection.selectionEnd);
    await runAiTask(t("snippet.ai.review.running"), async () => {
      const findings = await reviewSnippetCode(
        session,
        fullContent,
        selectedText,
        wholeSnippet,
        snippetAiLanguage(),
        t("snippet.ai.review.theme"),
      );
      setReviewFindings(findings);
      setRuntimeToolStatus(t("snippet.ai.review.ready"));
    }, t("snippet.ai.review.failed"));
  }

  function lineOffsets(content: string, line: number): { start: number; end: number } {
    const safeLine = Math.max(1, line);
    let start = 0;
    let currentLine = 1;
    while (currentLine < safeLine) {
      const next = content.indexOf("\n", start);
      if (next < 0) break;
      start = next + 1;
      currentLine++;
    }
    const lineEnd = content.indexOf("\n", start);
    return { start, end: lineEnd < 0 ? content.length : lineEnd };
  }

  function selectReviewLine(line: number) {
    const content = editing?.content ?? "";
    const { start, end } = lineOffsets(content, line);
    editorRef.current?.selectRange(start, end);
    editorRef.current?.focus();
  }

  // WP2.7: themed improvement of the current selection with diff preview.
  async function runCodeImprovement(theme: string) {
    if (!editing || aiBusy || !ensureSnippetAiDataNoticeAccepted(false)) return;
    if (!hasEditorSelection()) {
      setRuntimeToolStatus(t("snippet.ai.improve.selectFirst"));
      return;
    }
    const session = await requireAiSession();
    if (!session) return;
    const fullContent = editing.content ?? "";
    const selection = selectionRef.current;
    const selectionStart = selection.selectionStart;
    const selectionEnd = selection.selectionEnd;
    const selectedText = fullContent.slice(selectionStart, selectionEnd);
    await runAiTask(t("snippet.ai.improve.running"), async () => {
      const improvement = await improveSnippetCode(
        session,
        fullContent,
        selectedText,
        snippetAiLanguage(),
        theme,
      );
      if (!isUsableImprovement(improvement)) {
        setRuntimeToolStatus(t("snippet.ai.improve.empty"));
        return;
      }
      setAiDiffPreview({
        title: t("snippet.ai.diff.title"),
        summary: improvement.summary,
        original: selectedText,
        replacement: improvement.replacement,
        applyStart: selectionStart,
        applyEnd: selectionEnd,
        appliedStatus: t("snippet.ai.improve.applied"),
      });
      setRuntimeToolStatus(null);
    }, t("snippet.ai.improve.failed"));
  }

  function runCustomCodeImprovement() {
    const theme = window.prompt(
      `${t("snippet.ai.improve.customTitle")}\n${t("snippet.ai.improve.customHeader")}`,
    );
    const trimmed = theme?.trim();
    if (trimmed) {
      void runCodeImprovement(trimmed);
    }
  }

  // WP2.7: security review with selectable findings + applied-fix preview.
  async function runSecurityCheck() {
    if (!editing?.content.trim() || aiBusy || !ensureSnippetAiDataNoticeAccepted(false)) return;
    const session = await requireAiSession();
    if (!session) return;
    const fullContent = editing.content ?? "";
    await runAiTask(t("snippet.ai.security.running"), async () => {
      const findings = await reviewSnippetSecurity(session, fullContent, snippetAiLanguage());
      setSecurityFindings(findings);
      setRuntimeToolStatus(t("snippet.ai.security.ready"));
    }, t("snippet.ai.security.failed"));
  }

  async function runSecurityFixes(selectedFindings: SecurityFinding[]) {
    setSecurityFindings(null);
    if (!editing || selectedFindings.length === 0) return;
    const session = await requireAiSession();
    if (!session) return;
    const originalContent = editing.content ?? "";
    await runAiTask(t("snippet.ai.security.fixRunning"), async () => {
      const fix = await applySnippetSecurityFixes(
        session,
        originalContent,
        snippetAiLanguage(),
        selectedFindings,
      );
      if (!isUsableImprovement(fix)) {
        setRuntimeToolStatus(t("snippet.ai.security.fixEmpty"));
        return;
      }
      setAiDiffPreview({
        title: t("snippet.ai.security.diffTitle"),
        summary: fix.summary,
        original: originalContent,
        replacement: fix.replacement,
        applyStart: 0,
        applyEnd: originalContent.length,
        appliedStatus: t("snippet.ai.security.fixApplied"),
      });
      setRuntimeToolStatus(null);
    }, t("snippet.ai.security.fixFailed"));
  }

  // WP2.7: alternative solutions (selection-aware, configurable count).
  async function openAlternativeSolutions() {
    if (!editing?.content.trim() || aiBusy || !ensureSnippetAiDataNoticeAccepted(false)) return;
    const session = await requireAiSession();
    if (!session) return;
    const fullContent = editing.content ?? "";
    const selection = selectionRef.current;
    const hasSelection = hasEditorSelection();
    setAlternativesRequest({
      wholeSnippet: !hasSelection,
      start: hasSelection ? selection.selectionStart : 0,
      end: hasSelection ? selection.selectionEnd : fullContent.length,
      targetText: hasSelection
        ? fullContent.slice(selection.selectionStart, selection.selectionEnd)
        : fullContent,
      fullContent,
    });
  }

  function configuredAlternativeSolutionCount(): number {
    const count = settings.aiSnippetAlternativeSolutionCount;
    if (!count || !Number.isFinite(count)) return 3;
    return Math.max(1, Math.min(9, Math.trunc(count)));
  }

  async function loadAlternativeSolutions(additionalInstructions: string) {
    const request = alternativesRequest;
    if (!request) return [];
    const session = await resolveSnippetAiSession();
    if (!session) {
      throw new Error(t("snippet.ai.noProfile"));
    }
    return generateAlternativeSolutions(
      session,
      request.fullContent,
      request.targetText,
      request.wholeSnippet,
      snippetAiLanguage(),
      configuredAlternativeSolutionCount(),
      additionalInstructions.trim() || undefined,
    );
  }

  // WP2.7: technical description inserted as comment block.
  async function runSnippetDescription() {
    if (!editing?.content.trim() || aiBusy || !ensureSnippetAiDataNoticeAccepted(false)) return;
    const session = await requireAiSession();
    if (!session) return;
    const fullContent = editing.content ?? "";
    const selection = selectionRef.current;
    const wholeSnippet = !hasEditorSelection();
    const selectedText = wholeSnippet
      ? fullContent
      : fullContent.slice(selection.selectionStart, selection.selectionEnd);
    const insertOffset = wholeSnippet ? 0 : startOfLine(fullContent, selection.selectionStart);
    const indentation = wholeSnippet
      ? findLineIndentation(fullContent, firstContentOffset(fullContent))
      : findLineIndentation(fullContent, selection.selectionStart);
    await runAiTask(t("snippet.ai.describe.generating"), async () => {
      const description = await describeSnippet(
        session,
        fullContent,
        selectedText,
        wholeSnippet,
        snippetAiLanguage(),
      );
      if (!description.trim()) {
        setRuntimeToolStatus(t("snippet.ai.describe.generateFailed"));
        return;
      }
      setDescriptionDialog({ description, insertOffset, indentation });
      setRuntimeToolStatus(t("snippet.ai.describe.generated"));
    }, t("snippet.ai.describe.generateFailed"));
  }

  /** Port of SnippetEditDialog.insertTechnicalDescription. */
  function insertTechnicalDescription(text: string, insertOffset: number) {
    const content = editing?.content ?? "";
    const safeOffset = Math.max(0, Math.min(insertOffset, content.length));
    const insertion = text.trim();
    if (!insertion) return;
    const prefix = safeOffset > 0 && content[safeOffset - 1] !== "\n" ? "\n" : "";
    applyAiContentChange(safeOffset, safeOffset, `${prefix}${insertion}\n\n`);
    setRuntimeToolStatus(t("snippet.ai.describe.inserted"));
  }

  // WP2.7: correct/translate user-facing text segments inside the selection.
  async function runSelectionCorrection() {
    await runSelectionTextTransform(undefined);
  }

  function openTranslateDialog() {
    if (!editing || aiBusy || !hasEditorSelection()) return;
    setTranslateDialogOpen(true);
  }

  async function runSelectionTextTransform(targetLanguageCode: string | undefined) {
    if (!editing || aiBusy || !hasEditorSelection() || !ensureSnippetAiDataNoticeAccepted(false)) {
      return;
    }
    const fullContent = editing.content ?? "";
    const selection = selectionRef.current;
    const selectionStart = selection.selectionStart;
    const selectionEnd = selection.selectionEnd;
    const selectedText = fullContent.slice(selectionStart, selectionEnd);
    const segments = extractEditableSegments(selectedText, snippetAiLanguage());
    if (segments.length === 0) {
      setRuntimeToolStatus(t("snippet.ai.noTextSegments"));
      return;
    }
    const session = await requireAiSession();
    if (!session) return;
    const translating = targetLanguageCode !== undefined;
    await runAiTask(
      translating ? t("snippet.ai.selection.translating") : t("snippet.ai.selection.correcting"),
      async () => {
        const replacement = translating
          ? await translateSelectionText(
              session,
              fullContent,
              selectedText,
              snippetAiLanguage(),
              targetLanguageCode,
            )
          : await correctSelectionText(session, fullContent, selectedText, snippetAiLanguage());
        if (replacement !== selectedText) {
          applyAiContentChange(selectionStart, selectionEnd, replacement);
        }
        setRuntimeToolStatus(
          translating ? t("snippet.ai.selection.translated") : t("snippet.ai.selection.corrected"),
        );
      },
      translating ? t("snippet.ai.selection.translateFailed") : t("snippet.ai.selection.correctFailed"),
    );
  }

  // WP2.3: persist the active profile selection and custom profile list.
  // When a stored snippet is open, the selection is pinned to that snippet
  // (editorProfileId, persisted on save); otherwise it updates the global
  // selectedSnippetEditorProfileId. File drafts have no persisted profile and
  // fall back to the global setting.
  async function handleSelectProfile(profileId: string | undefined) {
    const normalizedId = profileId && profileId.trim() ? profileId : undefined;
    if (editing && !fileDraft) {
      setEditing((current) => (current ? { ...current, editorProfileId: normalizedId } : current));
      return;
    }
    const current = useSettingsStore.getState().settings;
    await saveSettings({
      ...current,
      selectedSnippetEditorProfileId: normalizedId,
    });
  }

  async function handleSaveProfiles(profiles: SnippetEditorProfile[]) {
    const current = useSettingsStore.getState().settings;
    await saveSettings({ ...current, snippetEditorProfiles: profiles });
  }

  function snippetsForExport(fallback: "active" | "all"): Snippet[] {
    const markedSnippets = snippets.filter((snippet) => markedSnippetIds.has(snippet.id));
    if (markedSnippets.length > 0) {
      return markedSnippets.map(normalizeSnippetForSave);
    }
    if (fallback === "all") {
      return snippets.map(normalizeSnippetForSave);
    }
    return selected ? [normalizeSnippetForSave(selected)] : [];
  }

  async function handleExportRuntime(format: RuntimeExportFormat, requestedTargetPath?: string, snippetsToExport?: Snippet[]) {
    setImportExportStatus(null);
    try {
      const selectedSnippets = snippetsToExport ?? snippetsForExport("active");
      if (selectedSnippets.length === 0) {
        setImportExportStatus("No snippets selected for export");
        return;
      }
      let targetPath: string | null = null;
      if (requestedTargetPath) {
        targetPath = requestedTargetPath;
      } else if (format === "ScriptFiles") {
        const directory = await openDialog({ directory: true, multiple: false });
        targetPath = typeof directory === "string" ? directory : null;
      } else {
        const extension = format === "GpgZip" ? "zip.gpg" : "zip";
        targetPath = await saveDialog({
          defaultPath: `snippets.${extension}`,
          filters: [
            { name: extension.toUpperCase(), extensions: [extension] },
            { name: "All files", extensions: ["*"] },
          ],
        });
      }
      if (!targetPath) return;
      const password = format === "AesZip" ? window.prompt("AES ZIP password") || undefined : undefined;
      if (format === "AesZip" && !password) return;
      const gpgRecipient = format === "GpgZip" ? window.prompt("GPG recipient") || undefined : undefined;
      if (format === "GpgZip" && !gpgRecipient) return;
      const result = await invoke<{ targetPath: string; exportedCount: number }>("export_snippet_scripts", {
        request: {
          snippets: selectedSnippets,
          targetPath,
          format,
          password,
          gpgRecipient,
        },
      });
      setImportExportStatus(`Exported ${result.exportedCount} snippet(s) to ${result.targetPath}`);
    } catch (error) {
      setImportExportStatus(`Export failed: ${String(error)}`);
    }
  }

  function confirmTransferDialog() {
    const mode = transferDialogMode;
    setTransferDialogMode(null);
    if (mode === "import") {
      void handleImport(importFormat);
    } else if (mode === "export") {
      void handleExport(exportFormat);
    }
  }

  // ---- WP2.10: snippet diagram dialog wiring ----

  function upsertEditingDiagram(diagram: SnippetDiagram) {
    setEditing((current) => {
      if (!current) return current;
      const diagrams = [...(current.diagrams || [])];
      const index = diagrams.findIndex((existing) => existing.id === diagram.id);
      if (index >= 0) {
        diagrams[index] = diagram;
      } else {
        diagrams.push(diagram);
      }
      return { ...current, diagrams };
    });
  }

  function deleteEditingDiagram(diagramId: string) {
    setEditing((current) => {
      if (!current) return current;
      return {
        ...current,
        diagrams: (current.diagrams || []).filter((diagram) => diagram.id !== diagramId),
      };
    });
  }

  /** Port of SnippetEditDialog.lineStartOffset. */
  function lineStartOffset(content: string, lineNumber: number): number {
    if (lineNumber <= 1) return 0;
    let currentLine = 1;
    for (let offset = 0; offset < content.length; offset++) {
      if (content[offset] === "\n") {
        currentLine++;
        if (currentLine === lineNumber) return offset + 1;
      }
    }
    return content.length;
  }

  /** Port of SnippetEditDialog.lineEndOffset. */
  function lineEndOffset(content: string, lineNumber: number): number {
    const startOffset = lineStartOffset(content, lineNumber);
    const endOffset = content.indexOf("\n", startOffset);
    return endOffset >= 0 ? endOffset : content.length;
  }

  /** Port of SnippetEditDialog.navigateToDiagramCodeReference. */
  function navigateToDiagramCodeReference(startLine: number, endLine: number) {
    const content = editorRef.current?.getValue() ?? editing?.content ?? "";
    const safeStartLine = Math.max(1, startLine);
    const safeEndLine = Math.max(safeStartLine, endLine);
    const start = Math.max(0, Math.min(lineStartOffset(content, safeStartLine), content.length));
    const end = Math.max(start, Math.min(lineEndOffset(content, safeEndLine), content.length));
    setDiagramDialogOpen(false);
    editorRef.current?.selectRange(start, end);
    editorRef.current?.revealCaret();
    editorRef.current?.focus();
  }

  // ---- WP1.3: file-draft "save as" / "save as snippet" flows ----

  function remoteSaveAsValidationError(name: string): string | null {
    const result = validateRemoteSiblingFileName(name);
    if ("fileName" in result) return null;
    const errorKeys: Record<RemoteSiblingFileNameError, string> = {
      empty: "sftp.saveAs.error.empty",
      dots: "sftp.saveAs.error.dots",
      separators: "sftp.saveAs.error.separators",
    };
    return t(errorKeys[result.error]);
  }

  async function handleFileSaveAsLocal() {
    if (!editing || !fileDraft) return;
    try {
      const targetPath = await saveDialog({ defaultPath: fileDraft.path });
      if (!targetPath) return;
      await invoke("write_local_text_file", { path: targetPath, content: editing.content });
      setRuntimeToolStatus(t("sftp.saveAs.savedFile", { path: targetPath }));
    } catch (error) {
      setRuntimeToolStatus(t("sftp.saveAs.saveFailed", { error: String(error) }));
    }
  }

  async function remoteSiblingExists(originalPath: string, fileName: string): Promise<boolean> {
    if (!fileDraft?.sessionId) return false;
    const parentIndex = originalPath.lastIndexOf("/");
    const parentPath = parentIndex <= 0 ? "/" : originalPath.slice(0, parentIndex);
    try {
      const entries = await invoke<{ name: string }[]>("sftp_list_dir", {
        sessionId: fileDraft.sessionId,
        path: parentPath,
      });
      return entries.some((entry) => entry.name === fileName);
    } catch {
      // Mirrors the Java flow: an unreadable directory falls through to the
      // write attempt, which reports its own error.
      return false;
    }
  }

  async function handleFileSaveAsRemote(newFileName: string) {
    if (!editing || !fileDraft || !fileDraft.sessionId) return;
    const validation = validateRemoteSiblingFileName(newFileName);
    if (!("fileName" in validation)) {
      setRuntimeToolStatus(t("sftp.saveAs.invalidFileName", { name: newFileName }));
      return;
    }
    setSaving(true);
    try {
      const originalName = fileDraft.path.split("/").filter(Boolean).pop() || "";
      if (
        validation.fileName !== originalName &&
        (await remoteSiblingExists(fileDraft.path, validation.fileName)) &&
        !window.confirm(t("sftp.saveAs.confirmOverwrite", { path: validation.fileName }))
      ) {
        return;
      }
      const newPath = await invoke<string>("write_remote_text_file_as", {
        sessionId: fileDraft.sessionId,
        originalPath: fileDraft.path,
        newFileName: validation.fileName,
        content: editing.content,
      });
      setRemoteSaveAsName(null);
      setRuntimeToolStatus(t("sftp.saveAs.savedFile", { path: newPath }));
    } catch (error) {
      setRuntimeToolStatus(t("sftp.saveAs.saveFailed", { error: String(error) }));
    } finally {
      setSaving(false);
    }
  }

  /**
   * Saves the file draft content as a brand-new snippet while the editor
   * stays in draft mode (port of SFTPManagerTab.saveDraftAsSnippet).
   */
  async function handleFileSaveAsSnippet() {
    if (!editing || !fileDraft) return;
    setSaving(true);
    try {
      const snippet: Snippet = normalizeSnippetForSave({
        ...editing,
        id: crypto.randomUUID(),
        favorite: false,
        history: [],
      });
      await invoke("save_snippet", { snippet });
      await loadSnippets();
      setRuntimeToolStatus(t("sftp.saveAs.savedSnippet", { name: snippet.name }));
    } catch (error) {
      setRuntimeToolStatus(t("sftp.saveAs.saveFailed", { error: String(error) }));
    } finally {
      setSaving(false);
    }
  }

  function handleAdd() {
    const s = newSnippet();
    setSnippets((prev) => [...prev, s]);
    setSelectedId(s.id);
    setEditing({ ...s });
  }

  const categories = [...new Set(snippets.map((s) => s.category).filter(Boolean))] as string[];
  const filtered = snippets.filter((s) => {
    const matchCat = !categoryFilter || s.category === categoryFilter;
    const matchSearch =
      !search ||
      s.name.toLowerCase().includes(search.toLowerCase()) ||
      (s.description || "").toLowerCase().includes(search.toLowerCase()) ||
      (s.content || "").toLowerCase().includes(search.toLowerCase());
    return matchCat && matchSearch;
  });

  if (!open) return null;

  const selected = snippets.find((s) => s.id === selectedId);
  // WP2.5: diff requires exactly two marked snippets, ordered by visible list order.
  const diffPair = orderedPair(filtered, markedSnippetIds);
  const isExistingSnippet = !fileDraft && !!editing && snippets.some((s) => s.id === editing.id);
  const safeHistoryIndex = Math.max(0, Math.min(historyIndex, contentHistory.length - 1));

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}>
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <FileCode className="w-4 h-4 text-kortty-accent" />
            Snippet Manager
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          <div
            className={`border-r border-kortty-border flex flex-col overflow-hidden transition-[width] duration-150 ${
              sidebarCollapsed ? "w-10" : "w-[220px]"
            }`}
          >
            <div className="flex items-center justify-between gap-1 p-2 border-b border-kortty-border">
              {!sidebarCollapsed && (
                <div className="min-w-0 text-xs font-medium text-kortty-text truncate">
                  Snippets
                </div>
              )}
              <button
                className="ml-auto flex h-6 w-6 items-center justify-center rounded text-kortty-text-dim hover:bg-kortty-panel hover:text-kortty-text"
                onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
                title={sidebarCollapsed ? "Show snippet list" : "Hide snippet list"}
              >
                {sidebarCollapsed ? (
                  <PanelLeftOpen className="h-3.5 w-3.5" />
                ) : (
                  <PanelLeftClose className="h-3.5 w-3.5" />
                )}
              </button>
            </div>
            {!sidebarCollapsed && (
              <>
                <div className="p-2 space-y-2 border-b border-kortty-border">
                  <select
                    className="input-field text-xs"
                    value={categoryFilter}
                    onChange={(e) => setCategoryFilter(e.target.value)}
                  >
                    <option value="">All categories</option>
                    {categories.map((c) => (
                      <option key={c} value={c}>
                        {c}
                      </option>
                    ))}
                  </select>
                  <input
                    className="input-field text-xs"
                    placeholder="Search…"
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                  />
                </div>
                <div className="flex-1 overflow-y-auto p-2">
                  {loading ? (
                    <div className="text-xs text-kortty-text-dim p-3">Loading…</div>
                  ) : (
                    filtered.map((s) => {
                      const marked = markedSnippetIds.has(s.id);
                      return (
                        <div
                          key={s.id}
                          className={`w-full px-2 py-1.5 text-xs rounded flex items-center gap-1 ${
                            selectedId === s.id
                              ? "bg-kortty-accent/10 text-kortty-accent"
                              : "text-kortty-text hover:bg-kortty-panel"
                          }`}
                        >
                          <input
                            type="checkbox"
                            className="h-3 w-3 shrink-0 accent-kortty-accent"
                            checked={marked}
                            onChange={(event) => setSnippetMarked(s.id, event.currentTarget.checked)}
                            title="Mark for export"
                          />
                          <button
                            type="button"
                            className="shrink-0 p-0.5 hover:text-kortty-accent"
                            onClick={() => {
                              void handleToggleFavorite(s.id);
                            }}
                          >
                            {s.favorite ? (
                              <Star className="w-3 h-3 fill-kortty-accent text-kortty-accent" />
                            ) : (
                              <StarOff className="w-3 h-3 text-kortty-text-dim" />
                            )}
                          </button>
                          <button
                            type="button"
                            className="min-w-0 flex-1 truncate text-left"
                            onClick={() => {
                              setSelectedId(s.id);
                              setEditing({ ...s });
                            }}
                          >
                            {s.name || "Unnamed"}
                          </button>
                        </div>
                      );
                    })
                  )}
                  {!loading && filtered.length === 0 && (
                    <div className="text-xs text-kortty-text-dim p-3">No snippets</div>
                  )}
                </div>
              </>
            )}
          </div>

          <div className="flex-1 p-4 overflow-hidden flex flex-col min-h-0">
            {editing ? (
              <div className="flex-1 flex flex-col space-y-3 min-h-0">
                <div className="rounded border border-kortty-border bg-kortty-panel/30">
                  <div className="flex items-center justify-between gap-3 px-3 py-2">
                    <button
                      className="flex min-w-0 items-center gap-2 text-left text-xs font-medium text-kortty-text"
                      onClick={() => setMetadataCollapsed((collapsed) => !collapsed)}
                      title={metadataCollapsed ? "Show metadata" : "Hide metadata"}
                    >
                      {metadataCollapsed ? (
                        <ChevronRight className="h-3.5 w-3.5 shrink-0 text-kortty-text-dim" />
                      ) : (
                        <ChevronDown className="h-3.5 w-3.5 shrink-0 text-kortty-text-dim" />
                      )}
                      <span className="truncate">Metadata</span>
                    </button>
                    <div className="flex items-center gap-2">
                      <button
                        className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-50"
                        disabled={aiMetadataLoading || !editing.content.trim()}
                        onClick={() => {
                          void handleGenerateMetadata();
                        }}
                        title="Analyze script content and fill snippet metadata"
                      >
                        <Bot className="w-3.5 h-3.5" />
                        AI
                      </button>
                      <button
                        className="flex h-7 w-7 items-center justify-center rounded text-kortty-text-dim hover:bg-kortty-border hover:text-kortty-text"
                        onClick={() => setMetadataCollapsed((collapsed) => !collapsed)}
                        title={metadataCollapsed ? "Show metadata" : "Hide metadata"}
                      >
                        {metadataCollapsed ? (
                          <ChevronRight className="h-3.5 w-3.5" />
                        ) : (
                          <ChevronDown className="h-3.5 w-3.5" />
                        )}
                      </button>
                    </div>
                  </div>
                  {!metadataCollapsed && (
                    <div className="space-y-3 border-t border-kortty-border px-3 py-3">
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Name</label>
                        <input
                          className="input-field"
                          value={editing.name}
                          onChange={(e) =>
                            setEditing((p) => (p ? { ...p, name: e.target.value } : null))
                          }
                          placeholder="Snippet name"
                        />
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Category</label>
                        <input
                          className="input-field"
                          value={editing.category || ""}
                          maxLength={SNIPPET_CATEGORY_MAX_LENGTH}
                          onChange={(e) =>
                            setEditing((p) => (p ? { ...p, category: normalizeSnippetCategory(e.target.value) } : null))
                          }
                          placeholder="Optional category"
                        />
                        <div className="mt-1 text-[10px] text-kortty-text-dim text-right">
                          {(editing.category || "").length}/{SNIPPET_CATEGORY_MAX_LENGTH}
                        </div>
                      </div>
                      <div>
                        <label className="block text-xs text-kortty-text-dim mb-1">Description</label>
                        <textarea
                          className="input-field min-h-12 resize-y"
                          rows={2}
                          value={editing.description || ""}
                          onChange={(e) =>
                            setEditing((p) => (p ? { ...p, description: e.target.value || undefined } : null))
                          }
                          placeholder="Optional two-line description"
                        />
                      </div>
                      {aiMetadataStatus && (
                        <div className="rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text-dim">
                          {aiMetadataStatus}
                        </div>
                      )}
                    </div>
                  )}
                </div>
                <div className="flex-1 flex flex-col min-h-0">
                  <div className="flex items-center justify-between mb-1">
                    <label className="text-xs text-kortty-text-dim">Content</label>
                    <div className="flex items-center gap-2">
                      <button
                        className="px-2 py-1 text-xs rounded bg-kortty-panel hover:bg-kortty-border transition-colors"
                        onClick={() => void handleFormatSnippet()}
                        title="Format snippet content"
                      >
                        Format
                      </button>
                      <span
                        className={`text-[10px] ${
                          formatterInfo?.available ? "text-kortty-text-dim" : "text-kortty-error"
                        }`}
                        title={
                          formatterInfo?.installHint ||
                          formatterInfo?.unavailableReason ||
                          formatterInfo?.displayName ||
                          ""
                        }
                      >
                        {formatterStatusLabel()}
                      </span>
                      <button
                        className="px-2 py-1 text-xs rounded bg-kortty-panel hover:bg-kortty-border transition-colors"
                        onClick={() => setDiagramDialogOpen(true)}
                        title={t("snippet.diagram.title")}
                      >
                        {t("snippet.diagram.button")}
                      </button>
                      <select
                        className="input-field text-xs w-36 py-0.5"
                        value={(() => {
                          // A snippet-pinned profile (editorProfileId) wins over the
                          // global selection; both treat CURRENT_SETTINGS as "".
                          const effective =
                            editing && !fileDraft && editing.editorProfileId
                              ? editing.editorProfileId
                              : settings.selectedSnippetEditorProfileId;
                          return effective && effective !== CURRENT_SETTINGS_PROFILE_ID ? effective : "";
                        })()}
                        title={t("snippet.profile.label")}
                        onChange={(e) => void handleSelectProfile(e.target.value || undefined)}
                      >
                        <option value="">{t("snippet.profile.current")}</option>
                        {availableCustomProfiles.length > 0 && (
                          <optgroup label={t("snippet.profile.custom")}>
                            {availableCustomProfiles.map((profile) => (
                              <option key={profile.id} value={profile.id}>
                                {profile.name}
                              </option>
                            ))}
                          </optgroup>
                        )}
                        <optgroup label={t("snippet.profile.presets")}>
                          {availableBuiltInProfiles.map((profile) => (
                            <option key={profile.id} value={profile.id}>
                              {profile.name}
                            </option>
                          ))}
                        </optgroup>
                      </select>
                      <button
                        className="flex h-6 w-6 items-center justify-center rounded bg-kortty-panel text-kortty-text-dim hover:bg-kortty-border hover:text-kortty-text transition-colors"
                        onClick={() => setProfileDialogOpen(true)}
                        title={t("snippet.profile.manage")}
                      >
                        <Palette className="h-3.5 w-3.5" />
                      </button>
                      <select
                        className="input-field text-xs w-40 py-0.5"
                        value={editing.language || "bash"}
                        onChange={(e) =>
                          setEditing((p) => (p ? { ...p, language: e.target.value } : null))
                        }
                      >
                        {LANGUAGES.map((l) => (
                          <option key={l.value} value={l.value}>
                            {l.label}
                          </option>
                        ))}
                      </select>
                    </div>
                  </div>
                  <SnippetColumnRuler
                    caretColumn={caretColumn}
                    caretVisualX={caretVisualX}
                    contentLeft={editorLayout.contentLeft}
                    charWidth={editorLayout.charWidth}
                    scrollLeft={editorLayout.scrollLeft}
                    limitColumn={limitColumn}
                    onLimitColumnChange={setLimitColumn}
                    onFormatAtLimit={() => void handleFormatToLimit()}
                    fontFamily={editorFontFamily}
                    fontSize={editorFontSize}
                    foregroundColor={activeProfile.foregroundColor}
                    backgroundColor={activeProfile.backgroundColor}
                    className="overflow-hidden rounded-t border border-b-0 border-kortty-border"
                  />
                  <MonacoSnippetEditor
                    ref={editorRef}
                    value={editing.content}
                    language={editing.language || "bash"}
                    onChange={(val) =>
                      setEditing((p) => (p ? { ...p, content: val } : null))
                    }
                    wordWrap={settings.snippetWordWrap}
                    lineNumbers={settings.snippetLineNumbers}
                    rulerColumn={limitColumn > 0 ? limitColumn : null}
                    fontFamily={editorFontFamily}
                    fontSize={editorFontSize}
                    theme={editorTheme}
                    cursorStyle={activeProfile.cursorStyle}
                    onSelectionChange={(selection: MonacoSelectionInfo) => {
                      selectionRef.current = selection;
                      setCaretColumn(selection.caretColumn);
                      setCaretVisualX(selection.caretVisualX);
                    }}
                    onLayoutChange={setEditorLayout}
                    onContextMenu={(event: MonacoContextMenuEvent) => {
                      setContextMenu({ x: event.x, y: event.y });
                    }}
                    className="flex-1 min-h-[120px] overflow-hidden rounded-b border border-kortty-border"
                  />
                  {contentHistory.length > 1 && (
                    <div className="mt-1 flex items-center gap-2 text-xs text-kortty-text-dim">
                      <span className="whitespace-nowrap">
                        {t("snippet.history.position", {
                          index: safeHistoryIndex + 1,
                          total: contentHistory.length,
                        })}
                      </span>
                      <input
                        type="range"
                        className="h-1.5 min-w-0 flex-1 accent-kortty-accent"
                        min={0}
                        max={contentHistory.length - 1}
                        step={1}
                        value={safeHistoryIndex}
                        onPointerDown={flushPendingHistory}
                        onChange={(e) => navigateToHistoryEntry(Number(e.target.value))}
                        title={t("snippet.history.label")}
                      />
                      <span className="whitespace-nowrap">
                        {contentHistory[safeHistoryIndex]
                          ? new Date(contentHistory[safeHistoryIndex].timestamp).toLocaleString()
                          : ""}
                      </span>
                      <button
                        className="px-2 py-0.5 text-xs rounded bg-kortty-panel hover:bg-kortty-border transition-colors"
                        onClick={restoreHistoryEntry}
                        title={t("snippet.history.restore")}
                      >
                        {t("snippet.history.restore")}
                      </button>
                    </div>
                  )}
                  {runtimeToolStatus && (
                    <div className="mt-2 rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text-dim">
                      {runtimeToolStatus}
                    </div>
                  )}
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">
                    Variables (name: default)
                  </label>
                  <input
                    className="input-field"
                    value={(editing.variables || [])
                      .map((v) => `${v.name}:${v.defaultValue}`)
                      .join(", ")}
                    onChange={(e) => {
                      const parts = e.target.value.split(",").map((s) => s.trim());
                      const vars: SnippetVariable[] = parts
                        .filter(Boolean)
                        .map((p) => {
                          const [name, ...rest] = p.split(":");
                          return {
                            name: name || "",
                            defaultValue: rest.join(":").trim() || "",
                          };
                        });
                      setEditing((p) => (p ? { ...p, variables: vars } : null));
                    }}
                    placeholder="var1: default1, var2: default2"
                  />
                </div>
              </div>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select or add a snippet
              </div>
            )}
          </div>
        </div>

        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border flex-wrap gap-2">
          <div className="flex gap-2 flex-wrap">
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
              onClick={handleAdd}
            >
              <Plus className="w-3 h-3" /> Add
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected && !editing}
              title={t("snippet.fullscreen.open")}
              onClick={() => {
                if (selected) setEditing({ ...selected });
                if (selected || editing) setFullscreenOpen(true);
              }}
            >
              <Maximize2 className="w-3 h-3" /> {t("snippet.fullscreen.open")}
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && handleDelete(selected.id)}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={() => setTransferDialogMode("import")}
              title="Import snippets"
            >
              <Upload className="w-3 h-3" /> Import
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={() => setTransferDialogMode("export")}
              title="Export snippets"
            >
              <Download className="w-3 h-3" /> Export
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!canDiff(markedSnippetIds) || !diffPair}
              onClick={() => setDiffDialogOpen(true)}
              title={t("snippet.diff.hint")}
            >
              <GitCompareArrows className="w-3 h-3" /> {t("snippet.diff.compare")}
            </button>
          </div>
          <div className="flex gap-2 items-center">
            {importExportStatus && (
              <span className="text-xs text-kortty-text-dim">{importExportStatus}</span>
            )}
            <button
              className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Close
            </button>
            {isExistingSnippet && (
              <button
                className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-50"
                disabled={!editing || saving || !editing.name.trim() || !editing.content.trim()}
                onClick={() => void handleSaveAsNew()}
                title={t("snippet.saveAsNew")}
              >
                <CopyPlus className="w-3 h-3" /> {t("snippet.saveAsNew")}
              </button>
            )}
            {fileDraft ? (
              // WP1.3b: split save menu for local/remote file drafts.
              <div className="relative flex">
                <button
                  className="rounded-l bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
                  disabled={!editing || saving}
                  onClick={() => {
                    setFileSaveMenuOpen(false);
                    void handleSave();
                  }}
                >
                  {fileDraft.source === "remote"
                    ? t("sftp.saveAs.overwriteRemote")
                    : t("sftp.saveAs.overwriteLocal")}
                </button>
                <button
                  className="rounded-r border-l border-kortty-bg/30 bg-kortty-accent px-1.5 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
                  disabled={!editing || saving}
                  onClick={() => setFileSaveMenuOpen((open) => !open)}
                  title={t("sftp.saveAs.menu")}
                >
                  <ChevronDown className="h-3.5 w-3.5" />
                </button>
                {fileSaveMenuOpen && (
                  <div className="fixed inset-0 z-[74]" onClick={() => setFileSaveMenuOpen(false)} />
                )}
                {fileSaveMenuOpen && (
                  <div className="absolute bottom-full right-0 z-[75] mb-1 w-56 rounded border border-kortty-border bg-kortty-surface py-1 shadow-2xl">
                    <button
                      className="block w-full px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-panel"
                      onClick={() => {
                        setFileSaveMenuOpen(false);
                        if (fileDraft.source === "remote") {
                          setRemoteSaveAsName(
                            fileDraft.path.split("/").filter(Boolean).pop() || "",
                          );
                        } else {
                          void handleFileSaveAsLocal();
                        }
                      }}
                    >
                      {t("sftp.saveAs.menu")}
                    </button>
                    <button
                      className="block w-full px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-panel"
                      onClick={() => {
                        setFileSaveMenuOpen(false);
                        void handleFileSaveAsSnippet();
                      }}
                    >
                      {t("sftp.saveAs.saveSnippet")}
                    </button>
                  </div>
                )}
              </div>
            ) : (
              <button
                className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
                disabled={!editing || saving}
                onClick={handleSave}
              >
                Save
              </button>
            )}
          </div>
        </div>
        {transferDialogMode && (
          <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
            <div className="w-[360px] rounded-lg border border-kortty-border bg-kortty-bg shadow-2xl">
              <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
                <h3 className="text-sm font-semibold text-kortty-text">
                  {transferDialogMode === "import" ? "Import snippets" : "Export snippets"}
                </h3>
                <button
                  className="text-kortty-text-dim hover:text-kortty-text"
                  onClick={() => setTransferDialogMode(null)}
                  type="button"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
              <div className="space-y-2 p-4">
                <label className="block text-xs text-kortty-text-dim" htmlFor="snippet-transfer-format">
                  Format
                </label>
                {transferDialogMode === "import" ? (
                  <select
                    id="snippet-transfer-format"
                    className="w-full rounded border border-kortty-border bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text"
                    value={importFormat}
                    onChange={(event) => setImportFormat(event.target.value as SnippetImportFormat)}
                  >
                    {SNIPPET_IMPORT_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                ) : (
                  <select
                    id="snippet-transfer-format"
                    className="w-full rounded border border-kortty-border bg-kortty-panel px-2 py-1.5 text-xs text-kortty-text"
                    value={exportFormat}
                    onChange={(event) => setExportFormat(event.target.value as SnippetExportFormat)}
                  >
                    {SNIPPET_EXPORT_OPTIONS.map((option) => (
                      <option key={option.value} value={option.value}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                )}
              </div>
              <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
                <button
                  className="px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
                  onClick={() => setTransferDialogMode(null)}
                  type="button"
                >
                  Cancel
                </button>
                <button
                  className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
                  onClick={confirmTransferDialog}
                  type="button"
                >
                  {transferDialogMode === "import" ? <Upload className="h-3 w-3" /> : <Download className="h-3 w-3" />}
                  {transferDialogMode === "import" ? "Import" : "Export"}
                </button>
              </div>
            </div>
          </div>
        )}
        {diffDialogOpen && diffPair && (
          <SnippetDiffDialog
            open
            onClose={() => setDiffDialogOpen(false)}
            leftSnippet={diffPair.left}
            rightSnippet={diffPair.right}
            fontFamily={editorFontFamily}
            fontSize={editorFontSize}
            theme={editorTheme}
          />
        )}
        <SnippetEditorProfileDialog
          open={profileDialogOpen}
          onClose={() => setProfileDialogOpen(false)}
          customProfiles={availableCustomProfiles}
          onSaveProfiles={handleSaveProfiles}
          selectedProfileId={settings.selectedSnippetEditorProfileId}
          onSelectProfile={(id) => void handleSelectProfile(id)}
        />
        {/* WP2.10: snippet PlantUML diagram dialog. */}
        {editing && (
          <SnippetDiagramDialog
            open={diagramDialogOpen}
            onClose={() => setDiagramDialogOpen(false)}
            snippetName={editing.name}
            content={editing.content}
            language={editing.language || "bash"}
            diagrams={editing.diagrams || []}
            onUpsertDiagram={upsertEditingDiagram}
            onDeleteDiagram={deleteEditingDiagram}
            onNavigateToCode={navigateToDiagramCodeReference}
          />
        )}
        {/* Distraction-free fullscreen code editor for the current snippet,
            bound to the same `editing` state so edits and Save stay in sync. */}
        {fullscreenOpen && editing && (
          <div className="fixed inset-0 z-[140] flex flex-col bg-kortty-bg">
            <div className="flex items-center justify-between gap-3 border-b border-kortty-border px-4 py-2">
              <div className="flex min-w-0 items-center gap-2">
                <Maximize2 className="h-4 w-4 shrink-0 text-kortty-text-dim" />
                <span className="truncate text-sm font-medium text-kortty-text">
                  {editing.name || t("snippet.unnamed")}
                </span>
                <select
                  className="input-field ml-2 h-7 py-0 text-xs"
                  value={editing.language || ""}
                  onChange={(e) =>
                    setEditing((p) => (p ? { ...p, language: e.target.value } : null))
                  }
                >
                  {LANGUAGES.map((l) => (
                    <option key={l.value} value={l.value}>
                      {l.label}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex items-center gap-2">
                <button
                  className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                  onClick={() => void handleFormatSnippet()}
                >
                  {t("snippet.fullscreen.format")}
                </button>
                <button
                  className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-40"
                  disabled={saving}
                  onClick={() => void handleSave()}
                >
                  {t("snippet.fullscreen.save")}
                </button>
                <button
                  className="rounded p-1 text-kortty-text-dim transition-colors hover:text-kortty-text"
                  title={t("snippet.fullscreen.exit")}
                  onClick={() => setFullscreenOpen(false)}
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
            </div>
            <MonacoSnippetEditor
              ref={fullscreenEditorRef}
              value={editing.content}
              language={editing.language || "bash"}
              onChange={(val) => setEditing((p) => (p ? { ...p, content: val } : null))}
              wordWrap={settings.snippetWordWrap}
              lineNumbers={settings.snippetLineNumbers}
              rulerColumn={limitColumn > 0 ? limitColumn : null}
              fontFamily={editorFontFamily}
              fontSize={editorFontSize}
              theme={editorTheme}
              cursorStyle={activeProfile.cursorStyle}
              className="min-h-0 flex-1"
            />
          </div>
        )}
        {/* WP1.3b: remote "save as" file-name dialog with live validation. */}
        {remoteSaveAsName !== null && fileDraft && (
          <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50">
            <div className="w-[400px] rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl">
              <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
                <h3 className="text-sm font-semibold text-kortty-text">
                  {t("sftp.saveAs.remoteTitle")}
                </h3>
                <button
                  className="text-kortty-text-dim hover:text-kortty-text"
                  onClick={() => setRemoteSaveAsName(null)}
                  type="button"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
              <div className="space-y-2 p-4">
                <div className="text-xs text-kortty-text-dim">{t("sftp.saveAs.remoteHeader")}</div>
                <label className="block text-xs text-kortty-text-dim" htmlFor="remote-save-as-name">
                  {t("sftp.saveAs.remoteLabel")}
                </label>
                <input
                  id="remote-save-as-name"
                  className="input-field w-full text-xs"
                  value={remoteSaveAsName}
                  autoFocus
                  onChange={(event) => setRemoteSaveAsName(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" && !remoteSaveAsValidationError(remoteSaveAsName)) {
                      void handleFileSaveAsRemote(remoteSaveAsName);
                    }
                  }}
                />
                {remoteSaveAsValidationError(remoteSaveAsName) && (
                  <div className="text-xs text-kortty-error">
                    {remoteSaveAsValidationError(remoteSaveAsName)}
                  </div>
                )}
              </div>
              <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
                <button
                  className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                  onClick={() => setRemoteSaveAsName(null)}
                  type="button"
                >
                  {t("common.cancel")}
                </button>
                <button
                  className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
                  disabled={saving || !!remoteSaveAsValidationError(remoteSaveAsName)}
                  onClick={() => void handleFileSaveAsRemote(remoteSaveAsName)}
                  type="button"
                >
                  {t("common.ok")}
                </button>
              </div>
            </div>
          </div>
        )}
        {/* WP2.7: snippet editor AI context menu. */}
        {contextMenu && editing && (
          <div
            className="fixed inset-0 z-[85]"
            onClick={() => setContextMenu(null)}
            onContextMenu={(event) => {
              event.preventDefault();
              setContextMenu(null);
            }}
          >
            <div
              className="absolute w-64 rounded border border-kortty-border bg-kortty-surface py-1 shadow-2xl"
              style={{
                left: Math.min(contextMenu.x, window.innerWidth - 272),
                top: Math.min(contextMenu.y, window.innerHeight - 420),
              }}
              onClick={(event) => event.stopPropagation()}
            >
              {(
                [
                  {
                    key: "assistant",
                    label: `✨ ${t("snippet.ai.context.assistant")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void openAiAssistDialog(),
                  },
                  {
                    key: "complete",
                    label: `✨ ${t("snippet.ai.context.complete")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void runCompletion(false),
                  },
                  {
                    key: "autoComplete",
                    label: `${autoCompleteEnabled ? "✓ " : ""}✨ ${t(
                      "snippet.ai.context.autoComplete",
                    )}`,
                    disabled: aiBusy && !autoCompleteEnabled,
                    action: () => handleAutoCompletionToggle(),
                  },
                  { key: "sep1" },
                  {
                    key: "review",
                    label: `✨ ${t("snippet.ai.context.review")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void runCodeReview(),
                  },
                  {
                    key: "improveReadability",
                    label: `✨ ${t("snippet.ai.context.improveReadability")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => void runCodeImprovement(t("snippet.ai.improve.readabilityTheme")),
                  },
                  {
                    key: "improveRobustness",
                    label: `✨ ${t("snippet.ai.context.improveRobustness")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => void runCodeImprovement(t("snippet.ai.improve.robustnessTheme")),
                  },
                  {
                    key: "improvePerformance",
                    label: `✨ ${t("snippet.ai.context.improvePerformance")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => void runCodeImprovement(t("snippet.ai.improve.performanceTheme")),
                  },
                  {
                    key: "improveCustom",
                    label: `✨ ${t("snippet.ai.context.improveCustom")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => runCustomCodeImprovement(),
                  },
                  {
                    key: "alternatives",
                    label: `✨ ${t("snippet.ai.context.alternatives")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void openAlternativeSolutions(),
                  },
                  {
                    key: "security",
                    label: `✨ ${t("snippet.ai.context.security")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void runSecurityCheck(),
                  },
                  {
                    key: "describe",
                    label: `✨ ${t("snippet.ai.context.describe")}`,
                    disabled: aiBusy || !editing.content.trim(),
                    action: () => void runSnippetDescription(),
                  },
                  { key: "sep2" },
                  {
                    key: "correctSelection",
                    label: `✨ ${t("snippet.ai.context.correctSelection")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => void runSelectionCorrection(),
                  },
                  {
                    key: "translateSelection",
                    label: `✨ ${t("snippet.ai.context.translateSelection")}`,
                    disabled: aiBusy || !hasEditorSelection(),
                    action: () => openTranslateDialog(),
                  },
                ] as { key: string; label?: string; disabled?: boolean; action?: () => void }[]
              ).map((item) =>
                item.label ? (
                  <button
                    key={item.key}
                    type="button"
                    className="block w-full px-3 py-1.5 text-left text-xs text-kortty-text transition-colors hover:bg-kortty-panel disabled:cursor-default disabled:opacity-40 disabled:hover:bg-transparent"
                    disabled={item.disabled}
                    onClick={() => {
                      setContextMenu(null);
                      item.action?.();
                    }}
                  >
                    {item.label}
                  </button>
                ) : (
                  <div key={item.key} className="my-1 border-t border-kortty-border" />
                ),
              )}
            </div>
          </div>
        )}
        {/* WP2.7: snippet AI dialogs. */}
        <SnippetAiAssistDialog
          open={assistDialogOpen}
          onClose={() => setAssistDialogOpen(false)}
          cursor={assistCursor}
          skillsAvailable={assistSkillsAvailable}
          onSubmit={(instruction, includeAiSkills) => void runAiAssist(instruction, includeAiSkills)}
        />
        {aiDiffPreview && (
          <SnippetAiDiffDialog
            open
            onClose={() => setAiDiffPreview(null)}
            onApply={applyAiDiffPreview}
            title={aiDiffPreview.title}
            summary={aiDiffPreview.summary}
            original={aiDiffPreview.original}
            replacement={aiDiffPreview.replacement}
            language={editing?.language}
            fontFamily={editorFontFamily}
            fontSize={editorFontSize}
            theme={editorTheme}
          />
        )}
        {reviewFindings && (
          <SnippetAiReviewDialog
            open
            onClose={() => setReviewFindings(null)}
            findings={reviewFindings}
            onSelectLine={selectReviewLine}
          />
        )}
        {securityFindings && (
          <SnippetSecurityReportDialog
            open
            onClose={() => setSecurityFindings(null)}
            findings={securityFindings}
            onApplySelected={(selected) => void runSecurityFixes(selected)}
          />
        )}
        {alternativesRequest && (
          <AlternativeSnippetSolutionsDialog
            open
            onClose={() => setAlternativesRequest(null)}
            language={editing?.language || "bash"}
            wholeSnippet={alternativesRequest.wholeSnippet}
            loader={loadAlternativeSolutions}
            onApply={(solution) => {
              applyAiContentChange(alternativesRequest.start, alternativesRequest.end, solution.code);
              setAlternativesRequest(null);
              setRuntimeToolStatus(t("snippet.ai.alternatives.applied"));
            }}
            fontFamily={editorFontFamily}
            fontSize={editorFontSize}
            theme={editorTheme}
          />
        )}
        {descriptionDialog && (
          <SnippetDescriptionDialog
            open
            onClose={() => setDescriptionDialog(null)}
            description={descriptionDialog.description}
            language={editing?.language}
            indentation={descriptionDialog.indentation}
            onInsert={(text) => insertTechnicalDescription(text, descriptionDialog.insertOffset)}
          />
        )}
        {/* WP2.7: target language picker for selection translation. */}
        {translateDialogOpen && (
          <div className="fixed inset-0 z-[90] flex items-center justify-center bg-black/50">
            <div className="w-[380px] rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl">
              <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
                <h3 className="text-sm font-semibold text-kortty-text">
                  {t("snippet.ai.selection.translateTitle")}
                </h3>
                <button
                  className="text-kortty-text-dim hover:text-kortty-text"
                  onClick={() => setTranslateDialogOpen(false)}
                  type="button"
                >
                  <X className="h-4 w-4" />
                </button>
              </div>
              <div className="space-y-2 p-4">
                <label className="block text-xs text-kortty-text-dim">
                  {t("snippet.ai.selection.translatePrompt")}
                </label>
                <select
                  className="input-field w-full text-xs"
                  value={translateLanguage}
                  onChange={(event) => setTranslateLanguage(event.target.value)}
                >
                  {AI_LANGUAGE_OPTIONS.map((option) => (
                    <option key={option.value} value={option.value}>
                      {option.label}
                    </option>
                  ))}
                </select>
              </div>
              <div className="flex justify-end gap-2 border-t border-kortty-border px-4 py-3">
                <button
                  className="rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border"
                  onClick={() => setTranslateDialogOpen(false)}
                  type="button"
                >
                  {t("common.cancel")}
                </button>
                <button
                  className="rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover"
                  onClick={() => {
                    setTranslateDialogOpen(false);
                    void runSelectionTextTransform(translateLanguage);
                  }}
                  type="button"
                >
                  {t("common.ok")}
                </button>
              </div>
            </div>
          </div>
        )}
        <div
          className="absolute bottom-0 right-0 w-4 h-4 cursor-nwse-resize opacity-40 hover:opacity-100 transition-opacity"
          onMouseDown={onResizeStart}
        >
          <svg viewBox="0 0 16 16" className="w-full h-full text-kortty-text-dim">
            <path d="M14 14L8 14L14 8Z" fill="currentColor" />
            <path d="M14 14L11 14L14 11Z" fill="currentColor" opacity="0.5" />
          </svg>
        </div>
      </div>
    </div>
  );
}
