// Snippet PlantUML diagram dialog (WP2.10).
// Port of de.kortty.ui.SnippetDiagramDialog: shows the persisted diagrams of
// a snippet, renders them locally to inline SVG, marks stale diagrams,
// regenerates them via the GenerateSnippetPlantUml AI action and offers
// PlantUML-source/SVG export, a persisted background color, readable activity
// colors and clickable code-reference hotspots that select the matching
// source lines in the editor.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import DOMPurify from "dompurify";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import { Bot, ClipboardCopy, Download, Paintbrush, Trash2, X } from "lucide-react";
import { useSettingsStore } from "../../../store/settingsStore";
import type { SnippetDiagram } from "../../../types/snippet";
import { resolveSnippetAiSession } from "../../../utils/snippetAiWorkflows";
import {
  DEFAULT_DIAGRAM_BACKGROUND_COLOR,
  buildFallbackLogicalStructurePlantUml,
  buildValidatedCodeReferences,
  contentHash,
  ensureReadableActivityColors,
  generateSnippetPlantUml,
  isDiagramStale,
  isRenderablePlantUml,
  normalizeHexColor,
  normalizeSvgText,
} from "../../../utils/snippetDiagramSupport";

interface RenderedSvg {
  diagramId: string;
  svg: string;
}

export interface SnippetDiagramDialogProps {
  open: boolean;
  onClose: () => void;
  snippetName: string;
  /** Current (possibly unsaved) snippet content used for staleness and hotspots. */
  content: string;
  language: string;
  diagrams: SnippetDiagram[];
  /** Adds or replaces a diagram on the edited snippet (persisted on snippet save). */
  onUpsertDiagram: (diagram: SnippetDiagram) => void;
  onDeleteDiagram: (diagramId: string) => void;
  /** Selects the 1-based line range in the snippet editor. */
  onNavigateToCode: (startLine: number, endLine: number) => void;
}

// Snippet content is attacker-controllable (shared snippets) and the rendered
// SVG runs inside a privileged Tauri webview, so the PlantUML SVG must be
// sanitized before it is injected via dangerouslySetInnerHTML or written to a
// file. DOMPurify strips <script>, event handlers and other active content
// while keeping valid SVG (including filters used by PlantUML gradients).
function sanitizeSvg(svg: string): string {
  return DOMPurify.sanitize(svg, { USE_PROFILES: { svg: true, svgFilters: true } });
}

function diagramListLabel(diagram: SnippetDiagram, fallback: string): string {
  return diagram.title?.trim() || diagram.name?.trim() || fallback;
}

function buildExportFileName(diagram: SnippetDiagram | null, fallback: string): string {
  const title = diagram?.title?.trim() || diagram?.name?.trim() || fallback;
  const sanitized = title.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
  return `${sanitized || "snippet-diagram"}.svg`;
}

export function SnippetDiagramDialog({
  open,
  onClose,
  snippetName,
  content,
  language,
  diagrams,
  onUpsertDiagram,
  onDeleteDiagram,
  onNavigateToCode,
}: SnippetDiagramDialogProps) {
  const { t } = useTranslation();
  const { settings, saveSettings } = useSettingsStore();
  const [selectedDiagramId, setSelectedDiagramId] = useState<string | null>(null);
  const [rendered, setRendered] = useState<RenderedSvg | null>(null);
  const [rendering, setRendering] = useState(false);
  const [stale, setStale] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const [generating, setGenerating] = useState(false);
  const [customInstructions, setCustomInstructions] = useState("");
  const svgContainerRef = useRef<HTMLDivElement | null>(null);

  const backgroundColor = normalizeHexColor(
    settings.snippetDiagramBackgroundColor,
    DEFAULT_DIAGRAM_BACKGROUND_COLOR,
  );

  const selectedDiagram = useMemo(
    () => diagrams.find((diagram) => diagram.id === selectedDiagramId) ?? null,
    [diagrams, selectedDiagramId],
  );

  // Keep a valid selection while diagrams come and go.
  useEffect(() => {
    if (!open) return;
    if (diagrams.length === 0) {
      setSelectedDiagramId(null);
      return;
    }
    if (!selectedDiagramId || !diagrams.some((diagram) => diagram.id === selectedDiagramId)) {
      setSelectedDiagramId(diagrams[0].id);
    }
  }, [open, diagrams, selectedDiagramId]);

  // Load the saved custom instructions of the selected diagram.
  useEffect(() => {
    setCustomInstructions(selectedDiagram?.customInstructions ?? "");
  }, [selectedDiagram?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  // Stale badge: compare the persisted content hash with the current content.
  useEffect(() => {
    if (!open || !selectedDiagram) {
      setStale(false);
      return;
    }
    let cancelled = false;
    void isDiagramStale(selectedDiagram, content).then((result) => {
      if (!cancelled) setStale(result);
    });
    return () => {
      cancelled = true;
    };
  }, [open, selectedDiagram, content]);

  // Render the selected diagram locally to an inline SVG.
  useEffect(() => {
    if (!open || !selectedDiagram) {
      setRendered(null);
      return;
    }
    const displaySource = ensureReadableActivityColors(selectedDiagram.source);
    if (!isRenderablePlantUml(displaySource)) {
      setRendered(null);
      setStatus(t("snippet.diagram.renderFailed", { error: "" }));
      return;
    }
    let cancelled = false;
    setRendering(true);
    setStatus(t("snippet.diagram.rendering"));
    invoke<{ svg: string; contentHash: string; tool: string }>("render_snippet_plantuml_svg", {
      source: displaySource,
      backgroundColor,
    })
      .then((result) => {
        if (cancelled) return;
        setRendered({ diagramId: selectedDiagram.id, svg: sanitizeSvg(result.svg) });
        setStatus(null);
      })
      .catch((error) => {
        if (cancelled) return;
        setRendered(null);
        setStatus(t("snippet.diagram.renderFailed", { error: String(error) }));
      })
      .finally(() => {
        if (!cancelled) setRendering(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, selectedDiagram?.id, selectedDiagram?.source, backgroundColor]); // eslint-disable-line react-hooks/exhaustive-deps

  const codeReferenceTooltip = useCallback(
    (label: string, startLine: number, endLine: number): string => {
      const range =
        startLine === endLine
          ? t("snippet.diagram.codeReference.line", { line: startLine })
          : t("snippet.diagram.codeReference.lines", { start: startLine, end: endLine });
      return `${label} (${range})`;
    },
    [t],
  );

  // Hotspots: after the SVG is injected, match its <text> nodes against the
  // validated code-reference labels and attach click navigation + tooltips.
  // Mirrors SnippetDiagramDialog.buildSvgHotspots (text node + preceding shape).
  useEffect(() => {
    const container = svgContainerRef.current;
    if (!container || !rendered || !selectedDiagram || rendered.diagramId !== selectedDiagram.id) {
      return;
    }
    const displaySource = ensureReadableActivityColors(selectedDiagram.source);
    const references = buildValidatedCodeReferences(
      displaySource,
      content,
      selectedDiagram.codeReferences ?? [],
    );
    if (references.length === 0) return;

    const referencesByLabel = new Map<string, typeof references>();
    for (const reference of references) {
      const key = normalizeSvgText(reference.label);
      const queue = referencesByLabel.get(key) ?? [];
      queue.push(reference);
      referencesByLabel.set(key, queue);
    }

    const cleanups: (() => void)[] = [];
    const textNodes = container.querySelectorAll("text");
    textNodes.forEach((textElement) => {
      const queue = referencesByLabel.get(normalizeSvgText(textElement.textContent));
      if (!queue || queue.length === 0) return;
      const reference = queue.shift()!;
      const tooltip = codeReferenceTooltip(reference.label, reference.startLine, reference.endLine);
      const onClick = (event: Event) => {
        event.preventDefault();
        event.stopPropagation();
        onNavigateToCode(reference.startLine, reference.endLine);
      };
      const targets: Element[] = [textElement];
      // The PlantUML SVG places the node shape (rect/polygon) before its label.
      let sibling = textElement.previousElementSibling;
      while (sibling) {
        if (sibling.tagName === "rect" || sibling.tagName === "polygon") {
          targets.push(sibling);
          break;
        }
        sibling = sibling.previousElementSibling;
      }
      for (const target of targets) {
        target.addEventListener("click", onClick);
        (target as SVGElement).style.cursor = "pointer";
        const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
        title.textContent = tooltip;
        target.appendChild(title);
        cleanups.push(() => {
          target.removeEventListener("click", onClick);
          title.remove();
        });
      }
    });
    return () => {
      for (const cleanup of cleanups) cleanup();
    };
  }, [rendered, selectedDiagram, content, codeReferenceTooltip, onNavigateToCode]);

  const changeBackgroundColor = useCallback(
    (color: string) => {
      const normalized = normalizeHexColor(color, DEFAULT_DIAGRAM_BACKGROUND_COLOR);
      if (normalized === backgroundColor) return;
      void saveSettings({ ...settings, snippetDiagramBackgroundColor: normalized });
    },
    [backgroundColor, saveSettings, settings],
  );

  async function handleGenerate(existing: SnippetDiagram | null) {
    if (generating || !content.trim()) return;
    setGenerating(true);
    setStatus(t("snippet.diagram.generating"));
    try {
      const session = await resolveSnippetAiSession();
      if (!session) {
        setStatus(t("snippet.ai.noProfile"));
        return;
      }
      const instructions = customInstructions.trim();
      let generated = await generateSnippetPlantUml(session, content, language, instructions || undefined);
      let plantUml = generated.plantUml;
      let codeReferences = generated.codeReferences;
      let title = generated.title;
      // Validate by rendering once; fall back to the local structure diagram
      // when the AI output is unusable (port of runDiagramGeneration).
      let renderable = isRenderablePlantUml(plantUml);
      if (renderable) {
        renderable = await invoke<{ svg: string }>("render_snippet_plantuml_svg", {
          source: plantUml,
          backgroundColor,
        })
          .then(() => true)
          .catch(() => false);
      }
      if (!renderable) {
        const fallbackSource = buildFallbackLogicalStructurePlantUml(content);
        const fallbackOk = await invoke<{ svg: string }>("render_snippet_plantuml_svg", {
          source: fallbackSource,
          backgroundColor,
        })
          .then(() => true)
          .catch(() => false);
        if (!fallbackOk) {
          setStatus(t("snippet.diagram.failed"));
          return;
        }
        plantUml = ensureReadableActivityColors(fallbackSource);
        codeReferences = [];
        title = title?.trim() || t("snippet.diagram.title");
      }
      const now = Date.now();
      const diagram: SnippetDiagram = {
        id: existing?.id ?? crypto.randomUUID(),
        name: title || existing?.name || t("snippet.diagram.typeLogicalStructure"),
        diagramType: "PlantUml",
        source: plantUml,
        renderedPath: existing?.renderedPath,
        contentHash: await contentHash(content),
        title,
        customInstructions: instructions,
        codeReferences,
        createdAt: existing?.createdAt ?? now,
        updatedAt: now,
      };
      onUpsertDiagram(diagram);
      setSelectedDiagramId(diagram.id);
      setStatus(t("snippet.diagram.ready"));
    } catch (error) {
      console.error("Snippet diagram generation failed:", error);
      setStatus(t("snippet.diagram.failed"));
    } finally {
      setGenerating(false);
    }
  }

  async function handleCopyPlantUml() {
    if (!selectedDiagram) return;
    const source = ensureReadableActivityColors(selectedDiagram.source);
    if (!source.trim()) return;
    try {
      await navigator.clipboard.writeText(source);
      setStatus(t("snippet.diagram.copied"));
    } catch (error) {
      setStatus(t("snippet.diagram.exportFailed", { error: String(error) }));
    }
  }

  function handleApplyReadableColors() {
    if (!selectedDiagram) return;
    const readable = ensureReadableActivityColors(selectedDiagram.source);
    if (readable === selectedDiagram.source) {
      setStatus(t("snippet.diagram.readableColorsApplied"));
      return;
    }
    onUpsertDiagram({ ...selectedDiagram, source: readable, updatedAt: Date.now() });
    setStatus(t("snippet.diagram.readableColorsApplied"));
  }

  async function handleExportSvg() {
    if (!selectedDiagram || !rendered || rendered.diagramId !== selectedDiagram.id) return;
    try {
      const targetPath = await saveDialog({
        defaultPath: buildExportFileName(selectedDiagram, t("snippet.diagram.typeLogicalStructure")),
        filters: [{ name: "SVG", extensions: ["svg"] }],
      });
      if (!targetPath) return;
      const path = targetPath.toLowerCase().endsWith(".svg") ? targetPath : `${targetPath}.svg`;
      await invoke("write_local_text_file", { path, content: rendered.svg });
      setStatus(t("snippet.diagram.exportSaved", { path }));
    } catch (error) {
      setStatus(t("snippet.diagram.exportFailed", { error: String(error) }));
    }
  }

  function handleDelete() {
    if (!selectedDiagram) return;
    onDeleteDiagram(selectedDiagram.id);
    setStatus(null);
  }

  if (!open) return null;

  const scriptLabel = t("snippet.diagram.script", {
    name: snippetName.trim() || t("snippet.diagram.unnamed"),
  });

  return (
    <div className="fixed inset-0 z-[80] flex items-center justify-center bg-black/50">
      <div className="flex h-[80vh] w-[900px] max-w-[95vw] flex-col rounded-lg border border-kortty-border bg-kortty-surface shadow-2xl">
        <div className="flex items-center justify-between border-b border-kortty-border px-4 py-3">
          <h3 className="text-sm font-semibold text-kortty-text">{t("snippet.diagram.title")}</h3>
          <button className="text-kortty-text-dim hover:text-kortty-text" onClick={onClose} type="button">
            <X className="h-4 w-4" />
          </button>
        </div>

        <div className="flex min-h-0 flex-1">
          {/* Diagram list */}
          <div className="flex w-[200px] flex-col border-r border-kortty-border">
            <div className="border-b border-kortty-border p-2 text-xs font-medium text-kortty-text">
              {scriptLabel}
            </div>
            <div className="flex-1 overflow-y-auto p-2">
              {diagrams.length === 0 ? (
                <div className="p-2 text-xs text-kortty-text-dim">{t("snippet.diagram.empty")}</div>
              ) : (
                diagrams.map((diagram) => (
                  <button
                    key={diagram.id}
                    type="button"
                    className={`mb-1 block w-full truncate rounded px-2 py-1.5 text-left text-xs ${
                      selectedDiagramId === diagram.id
                        ? "bg-kortty-accent/10 text-kortty-accent"
                        : "text-kortty-text hover:bg-kortty-panel"
                    }`}
                    onClick={() => setSelectedDiagramId(diagram.id)}
                    title={diagramListLabel(diagram, t("snippet.diagram.typeLogicalStructure"))}
                  >
                    {diagramListLabel(diagram, t("snippet.diagram.typeLogicalStructure"))}
                  </button>
                ))
              )}
            </div>
          </div>

          {/* Diagram view */}
          <div className="flex min-w-0 flex-1 flex-col p-3">
            <div className="mb-2 flex flex-wrap items-center gap-2">
              <button
                type="button"
                className="flex items-center gap-1 rounded bg-kortty-accent px-3 py-1.5 text-xs text-kortty-bg transition-colors hover:bg-kortty-accent-hover disabled:opacity-50"
                disabled={generating || !content.trim()}
                onClick={() => void handleGenerate(selectedDiagram)}
                title={t("snippet.diagram.instructionsHint")}
              >
                <Bot className="h-3 w-3" />
                {selectedDiagram ? t("snippet.diagram.regenerate") : t("snippet.diagram.generate")}
              </button>
              <button
                type="button"
                className="flex items-center gap-1 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border disabled:opacity-40"
                disabled={!selectedDiagram}
                onClick={() => void handleCopyPlantUml()}
              >
                <ClipboardCopy className="h-3 w-3" />
                {t("snippet.diagram.copyPlantUml")}
              </button>
              <button
                type="button"
                className="flex items-center gap-1 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border disabled:opacity-40"
                disabled={!selectedDiagram}
                onClick={handleApplyReadableColors}
              >
                <Paintbrush className="h-3 w-3" />
                {t("snippet.diagram.readableColors")}
              </button>
              <button
                type="button"
                className="flex items-center gap-1 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-text transition-colors hover:bg-kortty-border disabled:opacity-40"
                disabled={!selectedDiagram || !rendered || rendering}
                onClick={() => void handleExportSvg()}
              >
                <Download className="h-3 w-3" />
                {t("snippet.diagram.saveSvg")}
              </button>
              <button
                type="button"
                className="flex items-center gap-1 rounded bg-kortty-panel px-3 py-1.5 text-xs text-kortty-error transition-colors hover:bg-kortty-border disabled:opacity-40"
                disabled={!selectedDiagram}
                onClick={handleDelete}
              >
                <Trash2 className="h-3 w-3" />
                {t("snippet.diagram.delete")}
              </button>
              <div className="ml-auto flex items-center gap-1.5">
                <label className="text-xs text-kortty-text-dim" htmlFor="snippet-diagram-background">
                  {t("snippet.diagram.backgroundColor")}
                </label>
                <input
                  id="snippet-diagram-background"
                  type="color"
                  className="h-6 w-9 cursor-pointer rounded border border-kortty-border bg-transparent p-0"
                  value={backgroundColor}
                  onChange={(event) => changeBackgroundColor(event.target.value)}
                />
              </div>
            </div>

            <div className="mb-2 flex items-center gap-2">
              <label className="whitespace-nowrap text-xs text-kortty-text-dim" htmlFor="snippet-diagram-instructions">
                {t("snippet.diagram.instructions")}
              </label>
              <input
                id="snippet-diagram-instructions"
                className="input-field flex-1 text-xs"
                value={customInstructions}
                placeholder={t("snippet.diagram.instructionsHint")}
                onChange={(event) => setCustomInstructions(event.target.value)}
              />
            </div>

            {selectedDiagram && (
              <div className="mb-2 text-xs">
                {stale ? (
                  <span className="rounded border border-kortty-error/50 bg-kortty-error/10 px-2 py-0.5 text-kortty-error">
                    {t("snippet.diagram.stale")}
                  </span>
                ) : (
                  <span className="rounded border border-kortty-border bg-kortty-panel/50 px-2 py-0.5 text-kortty-text-dim">
                    {t("snippet.diagram.current")}
                  </span>
                )}
              </div>
            )}

            <div
              className="flex-1 overflow-auto rounded border border-kortty-border"
              style={{ background: backgroundColor }}
            >
              {rendered && selectedDiagram && rendered.diagramId === selectedDiagram.id ? (
                // Inline SVG in a sandboxed container; rendered.svg has already
                // been DOMPurify-sanitized (no <script>/event handlers) so it is
                // safe to inject even for attacker-controlled snippet content.
                <div
                  ref={svgContainerRef}
                  className="min-h-full min-w-full p-2 [&_svg]:h-auto [&_svg]:max-w-none"
                  dangerouslySetInnerHTML={{ __html: rendered.svg }}
                />
              ) : (
                <div className="p-4 text-xs text-kortty-text-dim">
                  {selectedDiagram
                    ? rendering
                      ? t("snippet.diagram.rendering")
                      : ""
                    : t("snippet.diagram.empty")}
                </div>
              )}
            </div>

            {status && (
              <div className="mt-2 rounded border border-kortty-border bg-kortty-panel/50 px-3 py-2 text-xs text-kortty-text-dim">
                {status}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
