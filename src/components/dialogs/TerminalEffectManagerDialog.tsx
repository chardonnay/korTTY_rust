import { useEffect, useMemo, useState } from "react";
import { Download, PlugZap, RefreshCw, Upload, X } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useDialogGeometry } from "../../hooks/useDialogGeometry";
import type { TerminalEffectPluginBundle, TerminalEffectPluginEntry } from "../../types/terminalEffects";

interface TerminalEffectManagerDialogProps {
  open: boolean;
  onClose: () => void;
}

export function TerminalEffectManagerDialog({ open, onClose }: TerminalEffectManagerDialogProps) {
  const { width, height, onResizeStart } = useDialogGeometry("terminal-effects", 760, 540, 520, 360);
  const [plugins, setPlugins] = useState<TerminalEffectPluginEntry[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [bundle, setBundle] = useState<TerminalEffectPluginBundle | null>(null);
  const [status, setStatus] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    void refreshPlugins();
  }, [open]);

  const selected = useMemo(
    () => plugins.find((plugin) => plugin.id === selectedId) ?? null,
    [plugins, selectedId],
  );

  if (!open) return null;

  async function refreshPlugins() {
    setStatus(null);
    try {
      const loaded = await invoke<TerminalEffectPluginEntry[]>("list_terminal_effect_plugins");
      setPlugins(loaded);
      setSelectedId((current) => current && loaded.some((plugin) => plugin.id === current) ? current : loaded[0]?.id ?? null);
      setBundle(null);
    } catch (error) {
      setStatus(`Load failed: ${String(error)}`);
    }
  }

  async function loadBundle(pluginId = selectedId) {
    if (!pluginId) return;
    try {
      setBundle(await invoke<TerminalEffectPluginBundle>("load_terminal_effect_plugin", { pluginId }));
      setStatus(`Loaded ${pluginId}.`);
    } catch (error) {
      setBundle(null);
      setStatus(`Load failed: ${String(error)}`);
    }
  }

  async function togglePlugin(plugin: TerminalEffectPluginEntry) {
    try {
      await invoke("set_terminal_effect_plugin_enabled", { pluginId: plugin.id, enabled: !plugin.enabled });
      await refreshPlugins();
      setSelectedId(plugin.id);
    } catch (error) {
      setStatus(`Enable/disable failed: ${String(error)}`);
    }
  }

  async function importPlugin() {
    const path = await openDialog({
      directory: false,
      multiple: false,
      filters: [
        { name: "Terminal effect bundle", extensions: ["zip", "json"] },
        { name: "All files", extensions: ["*"] },
      ],
    });
    if (typeof path !== "string") return;
    try {
      const imported = await invoke<TerminalEffectPluginEntry>("import_terminal_effect_plugin", { sourcePath: path });
      await refreshPlugins();
      setSelectedId(imported.id);
      setStatus(`Imported ${imported.name}.`);
    } catch (error) {
      setStatus(`Import failed: ${String(error)}`);
    }
  }

  async function exportPlugin() {
    if (!selected) return;
    const path = await saveDialog({
      defaultPath: `${selected.id}.zip`,
      filters: [{ name: "Terminal effect bundle", extensions: ["zip"] }],
    });
    if (typeof path !== "string") return;
    try {
      await invoke("export_terminal_effect_plugin", { pluginId: selected.id, targetPath: path });
      setStatus(`Exported ${selected.name}.`);
    } catch (error) {
      setStatus(`Export failed: ${String(error)}`);
    }
  }

  return (
    <div className="fixed inset-0 z-[70] flex items-center justify-center bg-black/50">
      <div
        className="relative flex flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl"
        style={{ width, height, maxWidth: "95vw", maxHeight: "95vh" }}
      >
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h2 className="flex items-center gap-2 text-sm font-semibold">
            <PlugZap className="h-4 w-4 text-kortty-accent" />
            Terminal Effects
          </h2>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose}>
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex items-center gap-2 border-b border-kortty-border px-3 py-2">
          <button className="btn-secondary flex items-center gap-2 text-xs" onClick={() => void refreshPlugins()}>
            <RefreshCw className="h-3.5 w-3.5" /> Reload
          </button>
          <button className="btn-primary flex items-center gap-2 text-xs" onClick={() => void importPlugin()}>
            <Upload className="h-3.5 w-3.5" /> Import
          </button>
          <button className="btn-secondary flex items-center gap-2 text-xs" disabled={!selected} onClick={() => void exportPlugin()}>
            <Download className="h-3.5 w-3.5" /> Export
          </button>
          <div className="ml-auto text-[11px] text-kortty-text-dim">
            Trusted local JS bundles under ~/.kortty/plugins/terminal-effects. Java .jar effects are rejected by the backend.
          </div>
        </div>

        <div className="flex min-h-0 flex-1">
          <div className="w-72 shrink-0 overflow-y-auto border-r border-kortty-border p-2">
            {plugins.map((plugin) => (
              <button
                key={`${plugin.source}:${plugin.id}`}
                className={`mb-1 w-full rounded px-3 py-2 text-left text-xs ${
                  selectedId === plugin.id ? "bg-kortty-accent/10 text-kortty-accent" : "hover:bg-kortty-panel"
                }`}
                onClick={() => setSelectedId(plugin.id)}
              >
                <div className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate font-medium">{plugin.name}</span>
                  <span className={plugin.enabled ? "text-kortty-success" : "text-kortty-text-dim"}>
                    {plugin.enabled ? "on" : "off"}
                  </span>
                </div>
                <div className="truncate text-[11px] text-kortty-text-dim">{plugin.id} · {plugin.source}</div>
              </button>
            ))}
            {plugins.length === 0 && <div className="p-3 text-xs text-kortty-text-dim">No terminal effect plugins found.</div>}
          </div>

          <div className="min-w-0 flex-1 overflow-y-auto p-4">
            {selected ? (
              <div className="space-y-4 text-xs">
                <div>
                  <div className="text-lg font-semibold">{selected.name}</div>
                  <div className="font-mono text-[11px] text-kortty-text-dim">{selected.id}</div>
                </div>
                {selected.description && <p className="text-kortty-text-dim">{selected.description}</p>}
                <div className="grid grid-cols-2 gap-3">
                  <Info label="Version" value={selected.version || "not set"} />
                  <Info label="Source" value={selected.source} />
                  <Info label="Entry" value={selected.entryPath} />
                  <Info label="CSS" value={selected.cssPath || "none"} />
                </div>
                <div className="flex gap-2">
                  <button className="btn-primary text-xs" onClick={() => void loadBundle(selected.id)}>Load bundle</button>
                  <button className="btn-secondary text-xs" onClick={() => void togglePlugin(selected)}>
                    {selected.enabled ? "Disable" : "Enable"}
                  </button>
                </div>
                {bundle && (
                  <div className="rounded border border-kortty-border bg-kortty-bg/60">
                    <div className="border-b border-kortty-border px-2 py-1 text-[11px] text-kortty-text-dim">
                      {bundle.manifest.entry}
                    </div>
                    <pre className="max-h-64 overflow-auto whitespace-pre-wrap p-2 font-mono text-[11px]">
                      {bundle.entryJs}
                    </pre>
                  </div>
                )}
              </div>
            ) : (
              <div className="py-10 text-center text-xs text-kortty-text-dim">Select a terminal effect plugin.</div>
            )}
          </div>
        </div>

        {status && <div className="border-t border-kortty-border px-4 py-2 text-xs text-kortty-text-dim">{status}</div>}
        <div className="absolute bottom-0 right-0 h-4 w-4 cursor-nwse-resize opacity-40 hover:opacity-100" onMouseDown={onResizeStart} />
      </div>
    </div>
  );
}

function Info({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded border border-kortty-border bg-kortty-panel/30 px-2 py-1">
      <div className="text-[10px] text-kortty-text-dim">{label}</div>
      <div className="truncate font-mono text-[11px]">{value}</div>
    </div>
  );
}
