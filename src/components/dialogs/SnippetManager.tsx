import { useState, useEffect } from "react";
import { X, Plus, Trash2, Edit, FileCode, Star, StarOff, Upload, Download } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { readTextFile, writeTextFile } from "@tauri-apps/plugin-fs";

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
  favorite: boolean;
  variables: SnippetVariable[];
}

interface SnippetManagerProps {
  open: boolean;
  onClose: () => void;
}

const EXPORT_FORMATS = ["JSON", "XML", "YAML"] as const;

function newSnippet(): Snippet {
  return {
    id: crypto.randomUUID(),
    name: "",
    content: "",
    category: undefined,
    favorite: false,
    variables: [],
  };
}

export function SnippetManager({ open, onClose }: SnippetManagerProps) {
  const [snippets, setSnippets] = useState<Snippet[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [editing, setEditing] = useState<Snippet | null>(null);
  const [categoryFilter, setCategoryFilter] = useState("");
  const [search, setSearch] = useState("");
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [importExportStatus, setImportExportStatus] = useState<string | null>(null);

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
      setSnippets(s);
      if (!selectedId && s.length > 0) setSelectedId(s[0].id);
      if (selectedId && !s.find((x) => x.id === selectedId)) setSelectedId(s[0]?.id ?? null);
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
      await invoke("save_snippet", { snippet: editing });
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
      await invoke("save_snippet", { snippet: updated });
      await loadSnippets();
    } catch (err) {
      console.error("Failed to toggle favorite:", err);
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
        setImportExportStatus("XML import not yet implemented");
        return;
      }
      if (!Array.isArray(imported)) {
        setImportExportStatus("Invalid file format");
        return;
      }
      for (const s of imported) {
        if (s.id && s.name) await invoke("save_snippet", { snippet: s });
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
        setImportExportStatus("XML export not yet implemented");
        return;
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
      (s.content || "").toLowerCase().includes(search.toLowerCase());
    return matchCat && matchSearch;
  });

  if (!open) return null;

  const selected = snippets.find((s) => s.id === selectedId);

  return (
    <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50">
      <div className="bg-kortty-surface border border-kortty-border rounded-lg shadow-2xl w-[720px] max-h-[85vh] flex flex-col">
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
          <div className="w-[220px] border-r border-kortty-border flex flex-col overflow-hidden">
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
          </div>

          <div className="flex-1 p-4 overflow-y-auto flex flex-col">
            {editing ? (
              <div className="space-y-3">
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
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, category: e.target.value || undefined } : null))
                    }
                    placeholder="Optional category"
                  />
                </div>
                <div>
                  <label className="block text-xs text-kortty-text-dim mb-1">Content</label>
                  <textarea
                    className="input-field font-mono text-xs min-h-[120px] resize-y"
                    value={editing.content}
                    onChange={(e) =>
                      setEditing((p) => (p ? { ...p, content: e.target.value } : null))
                    }
                    placeholder="Snippet content…"
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
      </div>
    </div>
  );
}
