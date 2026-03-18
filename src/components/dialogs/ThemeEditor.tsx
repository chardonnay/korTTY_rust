import { useState, useEffect } from "react";
import { X, Palette, Plus, Trash2, Copy, Check } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useThemeStore } from "../../store/themeStore";

interface ThemeEditorProps {
  open: boolean;
  onClose: () => void;
}

export interface ThemeData {
  id: string;
  name: string;
  foregroundColor: string;
  backgroundColor: string;
  cursorColor: string;
  selectionColor: string;
  fontFamily: string;
  fontSize: number;
  ansiColors: string[];
}

const FONT_FAMILIES = [
  "JetBrains Mono",
  "Fira Code",
  "Cascadia Code",
  "Source Code Pro",
  "Hack",
  "Inconsolata",
  "IBM Plex Mono",
  "Roboto Mono",
  "Victor Mono",
  "Ubuntu Mono",
  "SF Mono",
  "Menlo",
  "Monaco",
  "Consolas",
  "Courier New",
  "DejaVu Sans Mono",
  "Droid Sans Mono",
  "Liberation Mono",
  "Noto Sans Mono",
  "Anonymous Pro",
  "Fantasque Sans Mono",
  "Iosevka",
  "Monaspace Neon",
  "Monaspace Argon",
  "Comic Mono",
  "Geist Mono",
  "Berkeley Mono",
  "Maple Mono",
  "Input Mono",
  "Recursive Mono",
];

const ANSI_LABELS = [
  "Black",
  "Red",
  "Green",
  "Yellow",
  "Blue",
  "Magenta",
  "Cyan",
  "White",
  "Bright Black",
  "Bright Red",
  "Bright Green",
  "Bright Yellow",
  "Bright Blue",
  "Bright Magenta",
  "Bright Cyan",
  "Bright White",
];

const PREVIEW_LINES = [
  { text: "user@host", colorIndex: 2 },
  { text: ":", colorIndex: 7 },
  { text: "~/projects", colorIndex: 4 },
  { text: "$ ", colorIndex: 7 },
  { text: "ls -la\n", colorIndex: -1 },
  { text: "total 42\n", colorIndex: 7 },
  { text: "drwxr-xr-x  5 user  staff   160 Feb 22 10:00 ", colorIndex: 7 },
  { text: ".\n", colorIndex: 4 },
  { text: "-rw-r--r--  1 user  staff  1234 Feb 22 09:00 ", colorIndex: 7 },
  { text: "main.rs\n", colorIndex: 2 },
  { text: "-rw-r--r--  1 user  staff   567 Feb 22 09:00 ", colorIndex: 7 },
  { text: "Cargo.toml\n", colorIndex: 3 },
  { text: "drwxr-xr-x  2 user  staff    64 Feb 22 09:00 ", colorIndex: 7 },
  { text: "src/\n", colorIndex: 4 },
  { text: "-rwxr-xr-x  1 user  staff  8192 Feb 22 08:00 ", colorIndex: 7 },
  { text: "build.sh\n", colorIndex: 1 },
  { text: "user@host", colorIndex: 2 },
  { text: ":", colorIndex: 7 },
  { text: "~/projects", colorIndex: 4 },
  { text: "$ ", colorIndex: 7 },
];

function ColorSwatch({
  value,
  onChange,
  label,
}: {
  value: string;
  onChange: (v: string) => void;
  label?: string;
}) {
  return (
    <div className="flex flex-col items-center gap-0.5">
      {label && (
        <span className="text-[9px] text-kortty-text-dim leading-tight truncate max-w-[52px] text-center">
          {label}
        </span>
      )}
      <div className="relative group">
        <input
          type="color"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="w-7 h-7 rounded border border-kortty-border cursor-pointer bg-transparent"
        />
        <span className="absolute -bottom-4 left-1/2 -translate-x-1/2 text-[8px] text-kortty-text-dim opacity-0 group-hover:opacity-100 transition-opacity whitespace-nowrap">
          {value}
        </span>
      </div>
    </div>
  );
}

export function ThemeEditor({ open, onClose }: ThemeEditorProps) {
  const { width, height, onResizeStart } = useDialogGeometry("theme-editor", 820, 660, 640, 500);
  const { loadActiveTheme } = useThemeStore();
  const [themes, setThemes] = useState<ThemeData[]>([]);
  const [activeThemeId, setActiveThemeId] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<ThemeData | null>(null);
  const [dirty, setDirty] = useState(false);
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (open) load();
  }, [open]);

  async function load() {
    setLoading(true);
    try {
      const [all, activeId] = await Promise.all([
        invoke<ThemeData[]>("get_themes"),
        invoke<string>("get_active_theme_id"),
      ]);
      setThemes(all);
      setActiveThemeId(activeId);
      const sel = all.find((t) => t.id === activeId) ?? all[0];
      if (sel) {
        setSelectedId(sel.id);
        setEditing({ ...sel });
      }
    } catch (err) {
      console.error("Failed to load themes:", err);
    } finally {
      setLoading(false);
    }
  }

  function selectTheme(id: string) {
    const t = themes.find((x) => x.id === id);
    if (!t) return;
    setSelectedId(id);
    setEditing({ ...t });
    setDirty(false);
    setStatus(null);
  }

  function updateEditing(partial: Partial<ThemeData>) {
    setEditing((prev) => (prev ? { ...prev, ...partial } : null));
    setDirty(true);
  }

  function updateAnsi(index: number, value: string) {
    setEditing((prev) => {
      if (!prev) return null;
      const ansiColors = [...prev.ansiColors];
      ansiColors[index] = value;
      return { ...prev, ansiColors };
    });
    setDirty(true);
  }

  async function handleSave() {
    if (!editing) return;
    setSaving(true);
    setStatus(null);
    try {
      let toSave = { ...editing };
      if (toSave.id.startsWith("builtin-")) {
        toSave = {
          ...toSave,
          id: crypto.randomUUID(),
          name: toSave.name.endsWith(" (Custom)") ? toSave.name : `${toSave.name} (Custom)`,
        };
      }
      await invoke("save_theme", { theme: toSave });
      await invoke("set_active_theme_id", { id: toSave.id });
      setActiveThemeId(toSave.id);
      setDirty(false);
      setStatus("Theme saved");
      await load();
      setSelectedId(toSave.id);
      setEditing({ ...toSave });
      loadActiveTheme();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  async function handleActivate() {
    if (!selectedId) return;
    try {
      await invoke("set_active_theme_id", { id: selectedId });
      setActiveThemeId(selectedId);
      setStatus("Theme activated");
      loadActiveTheme();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    }
  }

  async function handleDelete() {
    if (!selectedId || selectedId.startsWith("builtin-")) return;
    try {
      await invoke("delete_theme", { id: selectedId });
      setStatus("Theme deleted");
      await load();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    }
  }

  function handleDuplicate() {
    if (!editing) return;
    const newTheme: ThemeData = {
      ...editing,
      id: crypto.randomUUID(),
      name: `${editing.name} (Copy)`,
    };
    setEditing(newTheme);
    setSelectedId(null);
    setDirty(true);
    setStatus(null);
  }

  function handleNew() {
    setEditing({
      id: crypto.randomUUID(),
      name: "New Theme",
      foregroundColor: "#cdd6f4",
      backgroundColor: "#1e1e2e",
      cursorColor: "#f5e0dc",
      selectionColor: "#45475a",
      fontFamily: "JetBrains Mono",
      fontSize: 14,
      ansiColors: [
        "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
        "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
        "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
        "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
      ],
    });
    setSelectedId(null);
    setDirty(true);
    setStatus(null);
  }

  if (!open) return null;

  const isBuiltin = selectedId?.startsWith("builtin-") ?? false;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div
        className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl flex flex-col relative"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Palette className="w-4 h-4 text-kortty-accent" />
            Theme Editor
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          {/* Sidebar: theme list */}
          <div className="w-[200px] border-r border-kortty-border flex flex-col overflow-hidden">
            <div className="p-2 border-b border-kortty-border">
              <button
                className="w-full flex items-center justify-center gap-1 px-2 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors"
                onClick={handleNew}
              >
                <Plus className="w-3 h-3" /> New Theme
              </button>
            </div>
            <div className="flex-1 overflow-y-auto p-1">
              {loading ? (
                <div className="text-xs text-kortty-text-dim p-3">Loading...</div>
              ) : (
                themes.map((t) => (
                  <button
                    key={t.id}
                    className={`w-full text-left px-2 py-1.5 text-xs rounded flex items-center gap-2 ${
                      selectedId === t.id
                        ? "bg-kortty-accent/10 text-kortty-accent"
                        : "text-kortty-text hover:bg-kortty-panel"
                    }`}
                    onClick={() => selectTheme(t.id)}
                  >
                    <span
                      className="w-3 h-3 rounded-full border border-kortty-border shrink-0"
                      style={{ backgroundColor: t.backgroundColor }}
                    />
                    <span className="truncate flex-1">{t.name}</span>
                    {t.id === activeThemeId && (
                      <Check className="w-3 h-3 text-kortty-accent shrink-0" />
                    )}
                  </button>
                ))
              )}
            </div>
          </div>

          {/* Editor panel */}
          <div className="flex-1 overflow-y-auto p-4 space-y-4 min-h-0">
            {editing ? (
              <>
                {/* Name + Font row */}
                <div className="grid grid-cols-3 gap-3">
                  <div>
                    <label className="block text-xs text-kortty-text-dim mb-1">Theme Name</label>
                    <input
                      className="input-field"
                      value={editing.name}
                      onChange={(e) => updateEditing({ name: e.target.value })}
                      placeholder="My Theme"
                      disabled={isBuiltin && !!selectedId}
                    />
                  </div>
                  <div>
                    <label className="block text-xs text-kortty-text-dim mb-1">Font Family</label>
                    <select
                      className="input-field"
                      value={editing.fontFamily}
                      onChange={(e) => updateEditing({ fontFamily: e.target.value })}
                    >
                      {FONT_FAMILIES.map((f) => (
                        <option key={f} value={f}>
                          {f}
                        </option>
                      ))}
                    </select>
                  </div>
                  <div>
                    <label className="block text-xs text-kortty-text-dim mb-1">Font Size</label>
                    <input
                      className="input-field"
                      type="number"
                      min={8}
                      max={32}
                      value={editing.fontSize}
                      onChange={(e) => updateEditing({ fontSize: parseInt(e.target.value) || 14 })}
                    />
                  </div>
                </div>

                {/* Base colors */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">Base Colors</label>
                  <div className="flex flex-wrap gap-4">
                    <ColorSwatch
                      value={editing.foregroundColor}
                      onChange={(v) => updateEditing({ foregroundColor: v })}
                      label="Foreground"
                    />
                    <ColorSwatch
                      value={editing.backgroundColor}
                      onChange={(v) => updateEditing({ backgroundColor: v })}
                      label="Background"
                    />
                    <ColorSwatch
                      value={editing.cursorColor}
                      onChange={(v) => updateEditing({ cursorColor: v })}
                      label="Cursor"
                    />
                    <ColorSwatch
                      value={editing.selectionColor}
                      onChange={(v) => updateEditing({ selectionColor: v })}
                      label="Selection"
                    />
                  </div>
                </div>

                {/* ANSI Normal */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">ANSI Colors (Normal)</label>
                  <div className="flex flex-wrap gap-2">
                    {editing.ansiColors.slice(0, 8).map((c, i) => (
                      <ColorSwatch
                        key={i}
                        value={c}
                        onChange={(v) => updateAnsi(i, v)}
                        label={ANSI_LABELS[i]}
                      />
                    ))}
                  </div>
                </div>

                {/* ANSI Bright */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">ANSI Colors (Bright)</label>
                  <div className="flex flex-wrap gap-2">
                    {editing.ansiColors.slice(8, 16).map((c, i) => (
                      <ColorSwatch
                        key={i}
                        value={c}
                        onChange={(v) => updateAnsi(8 + i, v)}
                        label={ANSI_LABELS[8 + i]}
                      />
                    ))}
                  </div>
                </div>

                {/* Live Preview */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">Live Preview</label>
                  <div
                    className="p-3 rounded border border-kortty-border font-mono overflow-auto min-h-[90px]"
                    style={{
                      backgroundColor: editing.backgroundColor,
                      color: editing.foregroundColor,
                      fontFamily: editing.fontFamily,
                      fontSize: editing.fontSize,
                    }}
                  >
                    {PREVIEW_LINES.map((line, i) => (
                      <span
                        key={i}
                        style={
                          line.colorIndex >= 0 && editing.ansiColors[line.colorIndex]
                            ? { color: editing.ansiColors[line.colorIndex] }
                            : undefined
                        }
                      >
                        {line.text}
                      </span>
                    ))}
                    <span
                      className="inline-block w-2 h-4 animate-pulse"
                      style={{ backgroundColor: editing.cursorColor }}
                    />
                  </div>
                </div>
              </>
            ) : (
              <div className="text-xs text-kortty-text-dim text-center py-8">
                Select or create a theme
              </div>
            )}
          </div>
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 border-t border-kortty-border flex-wrap gap-2">
          <div className="flex gap-2">
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!editing}
              onClick={handleDuplicate}
              title="Duplicate theme"
            >
              <Copy className="w-3 h-3" /> Duplicate
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selectedId || isBuiltin}
              onClick={handleDelete}
              title={isBuiltin ? "Built-in themes cannot be deleted" : "Delete theme"}
            >
              <Trash2 className="w-3 h-3" /> Delete
            </button>
            {selectedId && selectedId !== activeThemeId && !dirty && (
              <button
                className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-accent rounded hover:bg-kortty-border transition-colors"
                onClick={handleActivate}
              >
                <Check className="w-3 h-3" /> Activate
              </button>
            )}
          </div>
          <div className="flex items-center gap-2">
            {status && <span className="text-xs text-kortty-text-dim">{status}</span>}
            <button
              className="px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
              onClick={onClose}
            >
              Cancel
            </button>
            <button
              className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              onClick={handleSave}
              disabled={saving || loading || !editing || (!dirty && !!selectedId)}
            >
              {isBuiltin && dirty ? "Save as Copy" : "Save"}
            </button>
          </div>
        </div>

        {/* Resize handle */}
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
