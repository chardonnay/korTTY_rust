import { useState, useEffect } from "react";
import { X, Palette, Plus, Trash2, Copy, Check } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import { useGuiThemeStore, GuiThemeData } from "../../store/guiThemeStore";

interface GuiThemeEditorProps {
  open: boolean;
  onClose: () => void;
}

const COLOR_FIELDS: { key: keyof GuiThemeData; label: string }[] = [
  { key: "bg", label: "Background" },
  { key: "surface", label: "Surface" },
  { key: "panel", label: "Panel" },
  { key: "border", label: "Border" },
  { key: "text", label: "Text" },
  { key: "textDim", label: "Text Dim" },
  { key: "accent", label: "Accent" },
  { key: "accentHover", label: "Accent Hover" },
  { key: "success", label: "Success" },
  { key: "warning", label: "Warning" },
  { key: "error", label: "Error" },
  { key: "terminal", label: "Terminal BG" },
];

function ColorSwatch({
  value,
  onChange,
  label,
}: {
  value: string;
  onChange: (v: string) => void;
  label: string;
}) {
  return (
    <div className="flex items-center gap-2">
      <input
        type="color"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-7 h-7 rounded border border-kortty-border cursor-pointer bg-transparent shrink-0"
      />
      <div className="flex flex-col min-w-0">
        <span className="text-[10px] text-kortty-text-dim leading-tight">{label}</span>
        <span className="text-[9px] text-kortty-text-dim/60 font-mono">{value}</span>
      </div>
    </div>
  );
}

function PreviewPane({ theme }: { theme: GuiThemeData }) {
  return (
    <div
      className="rounded-lg border overflow-hidden text-[10px]"
      style={{ borderColor: theme.border, backgroundColor: theme.bg }}
    >
      {/* Mini menu bar */}
      <div
        className="flex items-center h-5 px-2 gap-3 border-b"
        style={{ backgroundColor: theme.surface, borderColor: theme.border }}
      >
        <span style={{ color: theme.text }}>File</span>
        <span style={{ color: theme.textDim }}>Edit</span>
        <span style={{ color: theme.textDim }}>View</span>
      </div>
      {/* Mini tab bar */}
      <div
        className="flex items-center h-5 px-1 gap-1 border-b"
        style={{ backgroundColor: theme.surface, borderColor: theme.border }}
      >
        <span
          className="px-2 py-0.5 rounded text-[9px]"
          style={{ backgroundColor: theme.panel, color: theme.accent }}
        >
          user@server
        </span>
        <span className="px-2 py-0.5 text-[9px]" style={{ color: theme.textDim }}>
          local
        </span>
      </div>
      {/* Mini terminal area */}
      <div className="p-2" style={{ backgroundColor: theme.terminal, minHeight: 48 }}>
        <div style={{ color: theme.success }}>
          user@host<span style={{ color: theme.text }}>:</span>
          <span style={{ color: theme.accent }}>~$</span>
          <span style={{ color: theme.text }}> ls -la</span>
        </div>
        <div style={{ color: theme.text }}>total 42</div>
        <div style={{ color: theme.warning }}>drwxr-xr-x 5 user staff</div>
      </div>
      {/* Mini status bar */}
      <div
        className="flex items-center justify-between h-4 px-2 border-t"
        style={{ backgroundColor: theme.surface, borderColor: theme.border }}
      >
        <span style={{ color: theme.textDim }}>KorTTY v1.0</span>
        <div className="flex gap-1.5 items-center">
          <span
            className="w-1.5 h-1.5 rounded-full inline-block"
            style={{ backgroundColor: theme.success }}
          />
          <span
            className="w-1.5 h-1.5 rounded-full inline-block"
            style={{ backgroundColor: theme.warning }}
          />
          <span
            className="w-1.5 h-1.5 rounded-full inline-block"
            style={{ backgroundColor: theme.error }}
          />
        </div>
      </div>
      {/* Mini button row */}
      <div className="p-2 flex gap-1.5" style={{ backgroundColor: theme.bg }}>
        <span
          className="px-2 py-0.5 rounded text-[9px]"
          style={{ backgroundColor: theme.accent, color: theme.bg }}
        >
          Save
        </span>
        <span
          className="px-2 py-0.5 rounded text-[9px]"
          style={{ backgroundColor: theme.panel, color: theme.text }}
        >
          Cancel
        </span>
        <span
          className="px-2 py-0.5 rounded text-[9px]"
          style={{ backgroundColor: theme.panel, color: theme.error }}
        >
          Delete
        </span>
      </div>
    </div>
  );
}

export function GuiThemeEditor({ open, onClose }: GuiThemeEditorProps) {
  const { width, height, onResizeStart } = useDialogGeometry("gui-theme-editor", 840, 660, 640, 500);
  const { loadActiveGuiTheme } = useGuiThemeStore();
  const [themes, setThemes] = useState<GuiThemeData[]>([]);
  const [activeThemeId, setActiveThemeId] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<GuiThemeData | null>(null);
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
        invoke<GuiThemeData[]>("get_gui_themes"),
        invoke<string>("get_active_gui_theme_id"),
      ]);
      setThemes(all);
      setActiveThemeId(activeId);
      const sel = all.find((t) => t.id === activeId) ?? all[0];
      if (sel) {
        setSelectedId(sel.id);
        setEditing({ ...sel });
      }
    } catch (err) {
      console.error("Failed to load GUI themes:", err);
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

  function updateColor(key: keyof GuiThemeData, value: string) {
    setEditing((prev) => (prev ? { ...prev, [key]: value } : null));
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
      await invoke("save_gui_theme", { theme: toSave });
      await invoke("set_active_gui_theme_id", { id: toSave.id });
      setActiveThemeId(toSave.id);
      setDirty(false);
      setStatus("Theme saved & activated");
      await load();
      setSelectedId(toSave.id);
      setEditing({ ...toSave });
      loadActiveGuiTheme();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    } finally {
      setSaving(false);
    }
  }

  async function handleActivate() {
    if (!selectedId) return;
    try {
      await invoke("set_active_gui_theme_id", { id: selectedId });
      setActiveThemeId(selectedId);
      setStatus("Theme activated");
      loadActiveGuiTheme();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    }
  }

  async function handleDelete() {
    if (!selectedId || selectedId.startsWith("builtin-")) return;
    try {
      await invoke("delete_gui_theme", { id: selectedId });
      setStatus("Theme deleted");
      await load();
    } catch (err) {
      setStatus(`Error: ${String(err)}`);
    }
  }

  function handleDuplicate() {
    if (!editing) return;
    setEditing({
      ...editing,
      id: crypto.randomUUID(),
      name: `${editing.name} (Copy)`,
    });
    setSelectedId(null);
    setDirty(true);
    setStatus(null);
  }

  function handleNew() {
    setEditing({
      id: crypto.randomUUID(),
      name: "New GUI Theme",
      bg: "#1e1e2e",
      surface: "#252536",
      panel: "#2a2a3c",
      border: "#3a3a4c",
      text: "#cdd6f4",
      textDim: "#6c7086",
      accent: "#89b4fa",
      accentHover: "#74a8fc",
      success: "#a6e3a1",
      warning: "#f9e2af",
      error: "#f38ba8",
      terminal: "#11111b",
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
            GUI Theme Editor
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex flex-1 min-h-0">
          {/* Sidebar */}
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
                      style={{ backgroundColor: t.bg }}
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
                {/* Name field */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Theme Name</label>
                  <input
                    className="input-field"
                    value={editing.name}
                    onChange={(e) => {
                      setEditing((prev) => (prev ? { ...prev, name: e.target.value } : null));
                      setDirty(true);
                    }}
                    placeholder="My GUI Theme"
                    disabled={isBuiltin && !!selectedId}
                  />
                </div>

                {/* Color grid */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">Colors</label>
                  <div className="grid grid-cols-2 sm:grid-cols-3 gap-3">
                    {COLOR_FIELDS.map((f) => (
                      <ColorSwatch
                        key={f.key}
                        value={(editing[f.key] as string) || "#000000"}
                        onChange={(v) => updateColor(f.key, v)}
                        label={f.label}
                      />
                    ))}
                  </div>
                </div>

                {/* Live preview */}
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-2">Live Preview</label>
                  <PreviewPane theme={editing} />
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
            >
              <Copy className="w-3 h-3" /> Duplicate
            </button>
            <button
              className="flex items-center gap-1 px-3 py-1.5 text-xs bg-kortty-panel text-kortty-error rounded hover:bg-kortty-border transition-colors disabled:opacity-40"
              disabled={!selectedId || isBuiltin}
              onClick={handleDelete}
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
              Close
            </button>
            <button
              className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
              onClick={handleSave}
              disabled={saving || loading || !editing || (!dirty && !!selectedId)}
            >
              {isBuiltin && dirty ? "Save as Copy" : "Save & Apply"}
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
