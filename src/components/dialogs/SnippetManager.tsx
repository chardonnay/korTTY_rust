import { useState, useEffect, useCallback, useMemo } from "react";
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
  PanelLeftClose,
  PanelLeftOpen,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { GlobalSettings } from "../../store/settingsStore";
import type { AiExecutionResult, AiProfile, AiRequestPayload } from "../../types/ai";
import { resolvePreferredAiProfileId } from "../../utils/aiProfiles";
import { DEFAULT_AI_LANGUAGE_CODE, resolveGuiLanguageCode } from "../../utils/aiLanguage";
import CodeMirror from "@uiw/react-codemirror";
import { oneDark } from "@codemirror/theme-one-dark";
import { StreamLanguage } from "@codemirror/language";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { sql } from "@codemirror/lang-sql";
import { json } from "@codemirror/lang-json";
import { xml } from "@codemirror/lang-xml";
import { yaml } from "@codemirror/lang-yaml";
import { html } from "@codemirror/lang-html";
import { css } from "@codemirror/lang-css";
import { markdown } from "@codemirror/lang-markdown";
import { php } from "@codemirror/lang-php";
import { rust } from "@codemirror/lang-rust";
import { java } from "@codemirror/lang-java";
import { cpp } from "@codemirror/lang-cpp";
import type { Extension } from "@codemirror/state";

const LANGUAGES: { value: string; label: string; ext: () => Extension }[] = [
  { value: "bash", label: "Bash / Shell", ext: () => StreamLanguage.define(shell) },
  { value: "shell", label: "Shell", ext: () => StreamLanguage.define(shell) },
  { value: "sh", label: "sh", ext: () => StreamLanguage.define(shell) },
  { value: "zsh", label: "zsh", ext: () => StreamLanguage.define(shell) },
  { value: "c", label: "C", ext: cpp },
  { value: "cpp", label: "C++", ext: cpp },
  { value: "css", label: "CSS", ext: css },
  { value: "html", label: "HTML", ext: html },
  { value: "java", label: "Java", ext: java },
  { value: "javascript", label: "JavaScript", ext: javascript },
  { value: "json", label: "JSON", ext: json },
  { value: "markdown", label: "Markdown", ext: markdown },
  { value: "php", label: "PHP", ext: php },
  { value: "python", label: "Python", ext: python },
  { value: "rust", label: "Rust", ext: rust },
  { value: "sql", label: "SQL", ext: sql },
  { value: "typescript", label: "TypeScript", ext: () => javascript({ typescript: true }) },
  { value: "xml", label: "XML", ext: xml },
  { value: "yaml", label: "YAML", ext: yaml },
  { value: "plain", label: "Plain Text", ext: () => [] as unknown as Extension },
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
  variables: SnippetVariable[];
}

interface SnippetManagerProps {
  open: boolean;
  onClose: () => void;
}

const EXPORT_FORMATS = ["JSON", "XML", "YAML"] as const;
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
    variables: snippet.variables ?? [],
  };
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
  return [
    "Analyze this script snippet and infer suitable metadata for a snippet library.",
    "Return exactly one JSON object and no Markdown.",
    'Schema: {"name":"...","category":"...","description":"..."}',
    `Use language code ${responseLanguageCode} for human-readable values.`,
    `category must be at most ${SNIPPET_CATEGORY_MAX_LENGTH} characters.`,
    "description must contain at least two concise lines separated by \\n.",
    "Do not invent external facts; use only what is visible in the script.",
  ].join("\n");
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
    variables: [],
  };
}

function SnippetCodeEditor({
  value,
  language,
  onChange,
}: {
  value: string;
  language: string;
  onChange: (val: string) => void;
}) {
  const langExtensions = useMemo(() => {
    const normalized = (language || "bash").toLowerCase();
    const lang = LANGUAGES.find((l) => l.value === normalized);
    if (!lang) return [];
    const ext = lang.ext();
    if (Array.isArray(ext) && ext.length === 0) return [];
    return [ext];
  }, [language]);

  const handleChange = useCallback(
    (val: string) => onChange(val),
    [onChange],
  );

  return (
    <CodeMirror
      value={value}
      onChange={handleChange}
      extensions={langExtensions}
      theme={oneDark}
      className="flex-1 min-h-[120px] overflow-auto rounded border border-kortty-border text-xs [&_.cm-editor]:!bg-[#1a1b26] [&_.cm-gutters]:!bg-[#16171f] [&_.cm-gutters]:!border-r-kortty-border"
      basicSetup={{
        lineNumbers: true,
        foldGutter: true,
        highlightActiveLine: true,
        bracketMatching: true,
        autocompletion: false,
        syntaxHighlighting: true,
      }}
    />
  );
}

export function SnippetManager({ open, onClose }: SnippetManagerProps) {
  const { width, height, onResizeStart } = useDialogGeometry("snippet-manager", 720, 520, 480, 360);
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
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [metadataCollapsed, setMetadataCollapsed] = useState(false);

  useEffect(() => {
    if (open) loadSnippets();
  }, [open]);

  useEffect(() => {
    if (selectedId && !editing) {
      const s = snippets.find((x) => x.id === selectedId);
      setEditing(s ? { ...s } : null);
    } else if (!selectedId) {
      setEditing(null);
    }
  }, [selectedId, snippets, editing]);

  async function loadSnippets() {
    setLoading(true);
    try {
      const s = await invoke<Snippet[]>("get_snippets");
      const normalized = s.map(normalizeSnippetForSave);
      setSnippets(normalized);
      if (!selectedId && normalized.length > 0) setSelectedId(normalized[0].id);
      if (selectedId && !normalized.find((x) => x.id === selectedId)) setSelectedId(normalized[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to load snippets:", err);
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    try {
      await invoke("save_snippet", { snippet: normalizeSnippetForSave(editing) });
      await loadSnippets();
    } catch (err) {
      console.error("Failed to save snippet:", err);
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(id: string) {
    try {
      await invoke("delete_snippet", { id });
      await loadSnippets();
      if (selectedId === id) setSelectedId(snippets[0]?.id ?? null);
    } catch (err) {
      console.error("Failed to delete snippet:", err);
    }
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

  async function handleImport(format: string) {
    setImportExportStatus(null);
    try {
      const path = await openDialog({
        multiple: false,
        directory: false,
        filters: [
          { name: format, extensions: [format.toLowerCase()] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      if (!path || typeof path !== "string") return;
      const content = await readTextFile(path);
      let imported: Snippet[] = [];
      if (format === "JSON") {
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

  async function handleExport(format: string) {
    setImportExportStatus(null);
    try {
      const path = await saveDialog({
        defaultPath: "snippets",
        filters: [
          { name: format, extensions: [format.toLowerCase()] },
          { name: "All files", extensions: ["*"] },
        ],
      });
      if (!path) return;
      let content = "";
      if (format === "JSON") {
        content = JSON.stringify(snippets, null, 2);
      } else if (format === "YAML") {
        try {
          const yaml = await import("yaml");
          content = yaml.stringify(snippets);
        } catch {
          setImportExportStatus("YAML package not installed");
          return;
        }
      } else {
        content = snippetsToXml(snippets);
      }
      await writeTextFile(path, content);
      setImportExportStatus(`Exported to ${path}`);
    } catch (err) {
      setImportExportStatus(`Export failed: ${String(err)}`);
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
                    filtered.map((s) => (
                      <button
                        key={s.id}
                        className={`w-full text-left px-2 py-1.5 text-xs rounded truncate flex items-center gap-1 ${
                          selectedId === s.id
                            ? "bg-kortty-accent/10 text-kortty-accent"
                            : "text-kortty-text hover:bg-kortty-panel"
                        }`}
                        onClick={() => setSelectedId(s.id)}
                      >
                        <button
                          className="shrink-0 p-0.5 hover:text-kortty-accent"
                          onClick={(e) => {
                            e.stopPropagation();
                            handleToggleFavorite(s.id);
                          }}
                        >
                          {s.favorite ? (
                            <Star className="w-3 h-3 fill-kortty-accent text-kortty-accent" />
                          ) : (
                            <StarOff className="w-3 h-3 text-kortty-text-dim" />
                          )}
                        </button>
                        <span className="truncate">{s.name || "Unnamed"}</span>
                      </button>
                    ))
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
                  <SnippetCodeEditor
                    value={editing.content}
                    language={editing.language || "bash"}
                    onChange={(val) =>
                      setEditing((p) => (p ? { ...p, content: val } : null))
                    }
                  />
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
              disabled={!selected}
              onClick={() => selected && setEditing({ ...selected })}
            >
              <Edit className="w-3 h-3" /> Edit
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selected}
              onClick={() => selected && handleDelete(selected.id)}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            <div className="flex gap-1">
              {EXPORT_FORMATS.map((fmt) => (
                <button
                  key={fmt}
                  className="flex items-center gap-1 px-2 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
                  onClick={() => handleImport(fmt)}
                >
                  <Upload className="w-3 h-3" /> {fmt}
                </button>
              ))}
            </div>
            <div className="flex gap-1">
              {EXPORT_FORMATS.map((fmt) => (
                <button
                  key={fmt}
                  className="flex items-center gap-1 px-2 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
                  onClick={() => handleExport(fmt)}
                >
                  <Download className="w-3 h-3" /> {fmt}
                </button>
              ))}
            </div>
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
            <button
              className="px-3 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              disabled={!editing || saving}
              onClick={handleSave}
            >
              Save
            </button>
          </div>
        </div>
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
