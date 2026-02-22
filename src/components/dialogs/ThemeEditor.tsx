import { useState, useEffect } from "react";
import { X, Palette } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

interface ThemeEditorProps {
  open: boolean;
  onClose: () => void;
}

export interface ThemeColors {
  ansi: string[];
  foreground: string;
  background: string;
  cursor: string;
  selection: string;
  fontFamily: string;
  fontSize: number;
}

const DEFAULT_ANSI = [
  "#1e1e2e",
  "#f38ba8",
  "#a6e3a1",
  "#f9e2af",
  "#89b4fa",
  "#f5c2e7",
  "#94e2d5",
  "#cdd6f4",
  "#45475a",
  "#f38ba8",
  "#a6e3a1",
  "#f9e2af",
  "#89b4fa",
  "#f5c2e7",
  "#94e2d5",
  "#bac2de",
];

const PREVIEW_LINES = [
  { text: "user@host:~$ ", colorIndex: 2 },
  { text: "ls -la\n", colorIndex: -1 },
  { text: "total 42\n", colorIndex: 7 },
  { text: "drwxr-xr-x  5 user  staff   160 Feb 22 10:00 ", colorIndex: 7 },
  { text: ".\n", colorIndex: 4 },
  { text: "drwxr-xr-x  2 user  staff    64 Feb 22 09:00 ", colorIndex: 7 },
  { text: "src\n", colorIndex: 4 },
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
    <div className="flex flex-col items-center gap-1">
      {label && <span className="text-[10px] text-kortty-text-dim">{label}</span>}
      <input
        type="color"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-8 h-8 rounded border border-kortty-border cursor-pointer bg-transparent"
      />
    </div>
  );
}

export function ThemeEditor({ open, onClose }: ThemeEditorProps) {
  const [theme, setTheme] = useState<ThemeColors>({
    ansi: [...DEFAULT_ANSI],
    foreground: "#cdd6f4",
    background: "#11111b",
    cursor: "#f5e0dc",
    selection: "#45475a",
    fontFamily: "JetBrains Mono",
    fontSize: 14,
  });
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (open) loadTheme();
  }, [open]);

  async function loadTheme() {
    setLoading(true);
    try {
      const t = await invoke<ThemeColors>("get_theme");
      if (t) setTheme(t);
    } catch {
      // use defaults
    } finally {
      setLoading(false);
    }
  }

  async function handleSave() {
    setSaving(true);
    try {
      await invoke("save_theme", { theme });
      onClose();
    } catch (err) {
      console.error("Failed to save theme:", err);
    } finally {
      setSaving(false);
    }
  }

  function updateTheme(partial: Partial<ThemeColors>) {
    setTheme((prev) => ({ ...prev, ...partial }));
  }

  function updateAnsi(index: number, value: string) {
    setTheme((prev) => {
      const ansi = [...prev.ansi];
      ansi[index] = value;
      return { ...prev, ansi };
    });
  }

  if (!open) return null;

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[560px] max-h-[90vh] flex flex-col">
        <div className="flex items-center justify-between px-4 py-3 border-b border-kortty-border">
          <h2 className="text-sm font-semibold flex items-center gap-2">
            <Palette className="w-4 h-4 text-kortty-accent" />
            Theme Editor
          </h2>
          <button onClick={onClose} className="text-kortty-text-dim hover:text-kortty-text">
            <X className="w-4 h-4" />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          <div className="grid grid-cols-2 gap-3">
            <div>
              <label className="block text-xs text-kortty-text-dim mb-1">Font Family</label>
              <input
                className="input-field"
                value={theme.fontFamily}
                onChange={(e) => updateTheme({ fontFamily: e.target.value })}
              />
            </div>
            <div>
              <label className="block text-xs text-kortty-text-dim mb-1">Font Size</label>
              <input
                className="input-field"
                type="number"
                value={theme.fontSize}
                onChange={(e) => updateTheme({ fontSize: parseInt(e.target.value) || 14 })}
              />
            </div>
          </div>

          <div>
            <label className="block text-xs text-kortty-text-dim mb-2">ANSI Colors (Normal)</label>
            <div className="flex flex-wrap gap-2">
              {theme.ansi.slice(0, 8).map((c, i) => (
                <ColorSwatch
                  key={i}
                  value={c}
                  onChange={(v) => updateAnsi(i, v)}
                  label={i === 0 ? "0" : i === 7 ? "7" : undefined}
                />
              ))}
            </div>
          </div>
          <div>
            <label className="block text-xs text-kortty-text-dim mb-2">ANSI Colors (Bright)</label>
            <div className="flex flex-wrap gap-2">
              {theme.ansi.slice(8, 16).map((c, i) => (
                <ColorSwatch
                  key={i}
                  value={c}
                  onChange={(v) => updateAnsi(8 + i, v)}
                  label={i === 0 ? "8" : i === 7 ? "15" : undefined}
                />
              ))}
            </div>
          </div>

          <div className="flex flex-wrap gap-4">
            <ColorSwatch
              value={theme.foreground}
              onChange={(v) => updateTheme({ foreground: v })}
              label="Foreground"
            />
            <ColorSwatch
              value={theme.background}
              onChange={(v) => updateTheme({ background: v })}
              label="Background"
            />
            <ColorSwatch
              value={theme.cursor}
              onChange={(v) => updateTheme({ cursor: v })}
              label="Cursor"
            />
            <ColorSwatch
              value={theme.selection}
              onChange={(v) => updateTheme({ selection: v })}
              label="Selection"
            />
          </div>

          <div>
            <label className="block text-xs text-kortty-text-dim mb-2">Live Preview</label>
            <div
              className="p-3 rounded border border-kortty-border font-mono text-xs overflow-auto min-h-[80px]"
              style={{
                backgroundColor: theme.background,
                color: theme.foreground,
                fontFamily: theme.fontFamily,
                fontSize: theme.fontSize,
              }}
            >
              {PREVIEW_LINES.map((line, i) => (
                <span
                  key={i}
                  style={
                    line.colorIndex >= 0 && theme.ansi[line.colorIndex]
                      ? { color: theme.ansi[line.colorIndex] }
                      : undefined
                  }
                >
                  {line.text}
                </span>
              ))}
              <span
                className="inline-block w-2 h-4 animate-pulse"
                style={{ backgroundColor: theme.cursor }}
              />
            </div>
          </div>
        </div>

        <div className="flex justify-end gap-2 px-4 py-3 border-t border-kortty-border">
          <button
            className="px-4 py-1.5 text-xs bg-kortty-panel text-kortty-text rounded hover:bg-kortty-border transition-colors"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            className="px-4 py-1.5 text-xs bg-kortty-accent text-kortty-bg rounded hover:bg-kortty-accent-hover transition-colors disabled:opacity-50"
            onClick={handleSave}
            disabled={saving || loading}
          >
            Save
          </button>
        </div>
      </div>
    </div>
  );
}
