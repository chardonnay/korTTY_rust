// Local helpers for persisted snippet PlantUML diagrams.
// Port of de.kortty.core.SnippetDiagramSupport (hashing, normalization,
// readable activity colors, background color, fallback diagram and validated
// code references) plus the GenerateSnippetPlantUml AI workflow pieces from
// de.kortty.core.SnippetAiWorkflowSupport / SnippetAiResponseSupport.

import { invoke } from "@tauri-apps/api/core";
import type { AiExecutionResult } from "../types/ai";
import type { SnippetCodeReference, SnippetDiagram } from "../types/snippet";
import {
  type SnippetAiSession,
  lineNumberedTextBlock,
  toSafeTextCodeBlock,
} from "./snippetAiWorkflows";

const COLOR_SETUP = "#EAF7EF";
const COLOR_MAIN = "#EAF4FF";
const COLOR_FAILURE = "#FDECEC";

const ACTIVITY_LABEL_PATTERN = /^\s*(?:#[A-Fa-f0-9]{6})?:(.*?)\s*;\s*(?:<<\s*#[A-Fa-f0-9]{6}\s*>>)?\s*$/;
const DECISION_LABEL_PATTERN = /^\s*if\s*\((.*?)\)\s*then\s*\([^)]*\)\s*$/i;
const HEX_COLOR_PATTERN = /^#[A-Fa-f0-9]{6}$/;
const SKINPARAM_BACKGROUND_PATTERN = /^\s*skinparam\s+backgroundColor\s+\S+\s*(?:\r?\n|$)/im;
const ACTIVITY_COLOR_STEREOTYPE_PATTERN = /.*<<\s*#[A-Fa-f0-9]{6}\s*>>\s*$/;
const DEPRECATED_COLORED_ACTIVITY_PATTERN = /^#[A-Fa-f0-9]{6}:.*/;

export const DEFAULT_DIAGRAM_BACKGROUND_COLOR = "#FFFFFF";

/** A code reference resolved against the current snippet content. */
export interface DiagramCodeReference {
  id: string;
  label: string;
  startLine: number;
  endLine: number;
  excerpt: string;
}

// ---- hashing / staleness ----

/** SHA-256 hex digest of the snippet content (async Web Crypto port of contentHash). */
export async function contentHash(content: string | null | undefined): Promise<string> {
  const bytes = new TextEncoder().encode(content ?? "");
  const digest = await crypto.subtle.digest("SHA-256", bytes);
  return [...new Uint8Array(digest)].map((value) => value.toString(16).padStart(2, "0")).join("");
}

/** Port of SnippetDiagramSupport.isStale: true when the saved hash no longer matches. */
export async function isDiagramStale(
  diagram: SnippetDiagram | null | undefined,
  currentContent: string,
): Promise<boolean> {
  if (!diagram) return false;
  const savedHash = diagram.contentHash;
  if (!savedHash || !savedHash.trim()) return false;
  return savedHash !== (await contentHash(currentContent));
}

// ---- normalization ----

/** Port of SnippetDiagramSupport.normalizePlantUml. */
export function normalizePlantUml(source: string | null | undefined): string {
  let value = (source ?? "").trim();
  if (!value) return "";
  value = value
    .replace(/^```(?:plantuml|puml)?\s*/, "")
    .replace(/```\s*$/, "")
    .trim();
  if (!value.startsWith("@startuml")) {
    value = `@startuml\n${value}`;
  }
  if (!value.endsWith("@enduml")) {
    value = `${value}\n@enduml`;
  }
  return value.trim();
}

/** Port of SnippetDiagramSupport.isRenderablePlantUml. */
export function isRenderablePlantUml(source: string | null | undefined): boolean {
  const value = (source ?? "").trim();
  return value.startsWith("@startuml") && value.endsWith("@enduml");
}

/** Port of SnippetDiagramSupport.normalizeHexColor. */
export function normalizeHexColor(
  color: string | null | undefined,
  fallback: string | null | undefined,
): string {
  const value = (color ?? "").trim();
  if (HEX_COLOR_PATTERN.test(value)) return value.toUpperCase();
  const fallbackValue = (fallback ?? "").trim();
  if (HEX_COLOR_PATTERN.test(fallbackValue)) return fallbackValue.toUpperCase();
  return DEFAULT_DIAGRAM_BACKGROUND_COLOR;
}

/** Port of SnippetDiagramSupport.applyBackgroundColor. */
export function applyBackgroundColor(
  source: string | null | undefined,
  backgroundColor: string | null | undefined,
): string {
  const value = normalizePlantUml(source);
  if (!value) return value;
  const color = normalizeHexColor(backgroundColor, DEFAULT_DIAGRAM_BACKGROUND_COLOR);
  const backgroundLine = `skinparam backgroundColor ${color}\n`;
  if (SKINPARAM_BACKGROUND_PATTERN.test(value)) {
    return value.replace(SKINPARAM_BACKGROUND_PATTERN, backgroundLine).trim();
  }
  const firstLineEnd = value.indexOf("\n");
  if (firstLineEnd < 0) {
    return `${value}\n${backgroundLine.trim()}`;
  }
  return (value.slice(0, firstLineEnd + 1) + backgroundLine + value.slice(firstLineEnd + 1)).trim();
}

// ---- readable activity colors ----

type FlowBranch = "neutral" | "success" | "failure";

function containsAny(value: string | null | undefined, ...needles: string[]): boolean {
  const lower = (value ?? "").toLowerCase();
  return needles.some((needle) => lower.includes(needle));
}

function leadingWhitespace(value: string): string {
  const match = value.match(/^\s*/);
  return match ? match[0] : "";
}

function hasActivityColorStereotype(trimmedActivityLine: string): boolean {
  return ACTIVITY_COLOR_STEREOTYPE_PATTERN.test(trimmedActivityLine);
}

function colorizedActivityLine(trimmedActivityLine: string, color: string): string {
  if (hasActivityColorStereotype(trimmedActivityLine)) {
    return trimmedActivityLine;
  }
  return `${trimmedActivityLine} <<${color}>>`;
}

function isDeprecatedColoredActivityLine(trimmedLine: string): boolean {
  return DEPRECATED_COLORED_ACTIVITY_PATTERN.test(trimmedLine);
}

function convertDeprecatedColoredActivityLine(trimmedLine: string): string {
  const separatorIndex = trimmedLine.indexOf(":");
  const color = trimmedLine.slice(0, separatorIndex);
  const activity = trimmedLine.slice(separatorIndex);
  return colorizedActivityLine(activity, color);
}

function isActivityDiagram(source: string): boolean {
  if (!isRenderablePlantUml(source)) return false;
  const lines = source.split(/\r\n|\r|\n/);
  const hasStart = lines.some((line) => line.trim() === "start");
  const hasStop = lines.some((line) => line.trim() === "stop");
  const hasActivity = lines.some(
    (line) => line.trim().startsWith(":") || /^#[A-Za-z0-9_]+:.*/.test(line.trim()),
  );
  return hasStart && hasStop && hasActivity;
}

function nextFlowBranch(trimmed: string, currentBranch: FlowBranch): FlowBranch {
  const lower = trimmed.toLowerCase();
  if (lower.startsWith("else")) {
    return lower.includes("no") || lower.includes("fail") || lower.includes("error")
      ? "failure"
      : "neutral";
  }
  if (lower.startsWith("if ") && lower.includes("then")) {
    return lower.includes("yes") || lower.includes("success") || lower.includes("ok")
      ? "success"
      : "neutral";
  }
  if (lower.startsWith("endif")) {
    return "neutral";
  }
  return currentBranch;
}

function activityColor(trimmedActivityLine: string, flowBranch: FlowBranch): string {
  if (
    flowBranch === "failure" ||
    containsAny(trimmedActivityLine, "fail", "error", "no-result", "no result")
  ) {
    return COLOR_FAILURE;
  }
  if (flowBranch === "success" || containsAny(trimmedActivityLine, "success", "complete", "ok")) {
    return COLOR_SETUP;
  }
  if (
    containsAny(
      trimmedActivityLine,
      "config",
      "configuration",
      "option",
      "argument",
      "parse",
      "load",
      "read",
      "init",
    )
  ) {
    return COLOR_SETUP;
  }
  return COLOR_MAIN;
}

/**
 * Port of SnippetDiagramSupport.ensureReadableActivityColors: colorizes plain
 * activity lines with the semantic palette and migrates the deprecated
 * "#RRGGBB:label;" prefix syntax to "<<#RRGGBB>>" stereotypes.
 */
export function ensureReadableActivityColors(source: string | null | undefined): string {
  const value = normalizePlantUml(source);
  if (!value || !isActivityDiagram(value)) {
    return value;
  }
  const result: string[] = [];
  let flowBranch: FlowBranch = "neutral";
  for (const line of value.split(/\r\n|\r|\n/)) {
    const trimmed = line.trim();
    if (trimmed.startsWith(":")) {
      result.push(leadingWhitespace(line) + colorizedActivityLine(trimmed, activityColor(trimmed, flowBranch)));
    } else if (isDeprecatedColoredActivityLine(trimmed)) {
      result.push(leadingWhitespace(line) + convertDeprecatedColoredActivityLine(trimmed));
    } else {
      result.push(line);
    }
    flowBranch = nextFlowBranch(trimmed, flowBranch);
  }
  return result.join("\n").trim();
}

// ---- fallback diagram ----

function safeActivityLabel(label: string | null | undefined): string {
  let value = label ?? "Run step";
  value = value.replace(/\n/g, " ").replace(/\r/g, " ");
  value = value.replace(/:/g, "-").replace(/;/g, ",");
  return value.trim() ? value.trim() : "Run step";
}

function appendColoredActivity(lines: string[], color: string, label: string, indent = ""): void {
  lines.push(`${indent}:${safeActivityLabel(label)}; <<${color}>>`);
}

function hasAssignments(content: string): boolean {
  return content.split(/\r\n|\r|\n/).some((line) => /^\s*[A-Za-z_][A-Za-z0-9_]*=.*/.test(line));
}

function hasConditionalFlow(lowerContent: string): boolean {
  return (
    lowerContent.includes("\nif ") ||
    lowerContent.startsWith("if ") ||
    lowerContent.includes("\ncase ") ||
    lowerContent.includes(" else")
  );
}

function successAction(lowerContent: string): string {
  return lowerContent.includes("mail") ? "Send success notification" : "Handle success path";
}

function failureAction(lowerContent: string): string {
  return lowerContent.includes("mail") ? "Send failure notification" : "Handle failure path";
}

/**
 * Port of SnippetDiagramSupport.buildFallbackLogicalStructurePlantUml: a
 * minimal renderable activity diagram used when the AI result is unusable.
 */
export function buildFallbackLogicalStructurePlantUml(content: string | null | undefined): string {
  const normalizedContent = content ?? "";
  const lowerContent = normalizedContent.toLowerCase();
  const actions: string[] = [];
  if (hasAssignments(normalizedContent)) {
    actions.push("Read configured values");
  }
  actions.push("Run main snippet logic");

  const lines: string[] = ["@startuml", "start"];
  for (const action of actions) {
    const color = action === "Run main snippet logic" ? COLOR_MAIN : COLOR_SETUP;
    appendColoredActivity(lines, color, action);
  }
  if (hasConditionalFlow(lowerContent)) {
    lines.push("if (Main command succeeds?) then (yes)");
    appendColoredActivity(lines, COLOR_SETUP, successAction(lowerContent), "  ");
    lines.push("else (no)");
    appendColoredActivity(lines, COLOR_FAILURE, failureAction(lowerContent), "  ");
    lines.push("endif");
  }
  lines.push("stop", "@enduml");
  return lines.join("\n");
}

// ---- code references ----

/** Port of SnippetDiagramSupport.normalizeDiagramLabel. */
export function normalizeDiagramLabel(label: string | null | undefined): string {
  let value = label ?? "";
  value = value.replace(/\\n/g, " ");
  value = value.replace(/\s+/g, " ").trim();
  return value;
}

/** Normalizes SVG text content for hotspot label matching. */
export function normalizeSvgText(value: string | null | undefined): string {
  return (value ?? "").replace(/\s+/g, " ").trim();
}

/** Port of SnippetDiagramSupport.extractDiagramLabels (labels only). */
export function extractDiagramLabels(plantUmlSource: string | null | undefined): string[] {
  const normalized = normalizePlantUml(plantUmlSource);
  if (!normalized) return [];
  const labels: string[] = [];
  for (const line of normalized.split(/\r\n|\r|\n/)) {
    const trimmed = line.trim();
    const activityMatch = trimmed.match(ACTIVITY_LABEL_PATTERN);
    if (activityMatch) {
      const label = normalizeDiagramLabel(activityMatch[1]);
      if (label) labels.push(label);
      continue;
    }
    const decisionMatch = trimmed.match(DECISION_LABEL_PATTERN);
    if (decisionMatch) {
      const label = normalizeDiagramLabel(decisionMatch[1]);
      if (label) labels.push(label);
    }
  }
  return labels;
}

function formatExcerpt(lines: string[], startLine: number, endLine: number): string {
  const safeStart = Math.max(1, Math.min(startLine, lines.length));
  const safeEnd = Math.max(safeStart, Math.min(endLine, lines.length));
  const displayedEnd = Math.min(safeEnd, safeStart + 7);
  const width = String(displayedEnd).length;
  const parts: string[] = [];
  for (let lineNumber = safeStart; lineNumber <= displayedEnd; lineNumber++) {
    parts.push(`${String(lineNumber).padStart(width, " ")} | ${lines[lineNumber - 1]}`);
  }
  let excerpt = parts.join("\n");
  if (displayedEnd < safeEnd) {
    excerpt += "\n...";
  }
  return excerpt;
}

function isValidCodeReferenceRange(lines: string[], startLine: number, endLine: number): boolean {
  if (lines.length === 0 || startLine < 1 || endLine < startLine || endLine > lines.length) {
    return false;
  }
  for (let lineNumber = startLine; lineNumber <= endLine; lineNumber++) {
    if (lines[lineNumber - 1].trim()) return true;
  }
  return false;
}

/**
 * Port of SnippetDiagramSupport.buildValidatedCodeReferences: keeps only the
 * persisted code references whose label appears in the PlantUML source and
 * whose line range points at non-blank lines of the current content.
 */
export function buildValidatedCodeReferences(
  plantUmlSource: string | null | undefined,
  content: string | null | undefined,
  sourceReferences: SnippetCodeReference[] | null | undefined,
): DiagramCodeReference[] {
  const normalizedContent = content ?? "";
  if (!normalizedContent.trim() || !sourceReferences || sourceReferences.length === 0) {
    return [];
  }
  const lines = normalizedContent.split(/\r\n|\r|\n/);
  const diagramLabels = new Set(extractDiagramLabels(plantUmlSource).map(normalizeDiagramLabel));
  if (diagramLabels.size === 0) return [];

  const references: DiagramCodeReference[] = [];
  for (const sourceReference of sourceReferences) {
    if (!sourceReference) continue;
    const label = normalizeDiagramLabel(sourceReference.label);
    if (!label || !diagramLabels.has(label)) continue;
    const startLine = sourceReference.startLine;
    const endLine = Math.max(startLine, sourceReference.endLine);
    if (!isValidCodeReferenceRange(lines, startLine, endLine)) continue;
    references.push({
      id: `ref-${references.length}`,
      label,
      startLine,
      endLine,
      excerpt: formatExcerpt(lines, startLine, endLine),
    });
  }
  return references;
}

// ---- AI generation (GenerateSnippetPlantUml workflow) ----

/** Parsed AI response for the GenerateSnippetPlantUml action. */
export interface PlantUmlDiagramResponse {
  title: string;
  plantUml: string;
  codeReferences: SnippetCodeReference[];
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function firstString(object: Record<string, unknown> | null, ...names: string[]): string {
  if (!object) return "";
  for (const name of names) {
    const value = object[name];
    if (typeof value === "string") return value;
    if (typeof value === "number" || typeof value === "boolean") return String(value);
  }
  return "";
}

function firstInt(object: Record<string, unknown>, ...names: string[]): number | null {
  for (const name of names) {
    const value = object[name];
    if (typeof value === "number" && Number.isFinite(value)) return Math.trunc(value);
    if (typeof value === "string" && value.trim() && Number.isFinite(Number(value))) {
      return Math.trunc(Number(value));
    }
  }
  return null;
}

function extractJsonObject(responseText: string | null | undefined): Record<string, unknown> | null {
  if (!responseText || !responseText.trim()) return null;
  const match = responseText.match(/\{[\s\S]*\}/);
  if (!match) return null;
  try {
    return asRecord(JSON.parse(match[0]));
  } catch {
    return null;
  }
}

/** Port of SnippetAiResponseSupport.parsePlantUmlDiagram (tolerant key names). */
export function parsePlantUmlDiagramResponse(
  responseText: string | null | undefined,
): PlantUmlDiagramResponse {
  const empty: PlantUmlDiagramResponse = { title: "", plantUml: "", codeReferences: [] };
  const object = extractJsonObject(responseText);
  if (!object) return empty;

  const rawTitle = firstString(object, "title", "name").trim();
  const plantUml = ensureReadableActivityColors(
    firstString(object, "plantUml", "plantuml", "source", "diagram"),
  );
  const codeReferences: SnippetCodeReference[] = [];
  for (const name of ["codeReferences", "sourceMap", "codeMap", "mappings"]) {
    const array = object[name];
    if (!Array.isArray(array)) continue;
    for (const element of array) {
      const reference = asRecord(element);
      if (!reference) continue;
      const label = firstString(reference, "label", "diagramLabel", "node", "activity", "decision").trim();
      const startLine = firstInt(reference, "startLine", "lineStart", "line");
      const endLine = firstInt(reference, "endLine", "lineEnd") ?? startLine;
      if (label && startLine !== null && endLine !== null) {
        codeReferences.push({ label, startLine, endLine });
      }
    }
    break;
  }
  if (!isRenderablePlantUml(plantUml)) return empty;
  return {
    title: rawTitle || "Snippet structure",
    plantUml,
    codeReferences,
  };
}

/** Port of SnippetAiWorkflowSupport.buildPlantUmlContext. */
export function buildPlantUmlContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Diagram label language: ${fallbackLanguageCode}\n` +
    "Generate one compact logical-structure PlantUML diagram for this snippet. " +
    "Use only relationships visible in the code. " +
    "For scripts and imperative code, generate only a simple activity diagram with start, activity lines, if/else branches, and stop. " +
    "Use a small semantic HEX color palette to distinguish setup, main work, success, and failure paths. " +
    "Activity lines may use :Action label; <<#RRGGBB>> syntax. " +
    "Do not use gradients or large style blocks. " +
    "Do not use component/package/class/object/actor/usecase blocks for script variables or commands. " +
    "Do not copy raw source lines into PlantUML; summarize them as activity labels.\n" +
    "Every action line between start and stop must use :Action label; or :Action label; <<#RRGGBB>> syntax.\n" +
    "Also return codeReferences. Each entry must map one visible activity label or decision text exactly to a small relevant source range. " +
    "Create one codeReferences entry for every visible activity and decision; exclude only start, stop, arrows, and merge nodes. " +
    "Use only the 1-based line numbers shown in the line-numbered snippet. " +
    "When one diagram element summarizes a block, use the smallest source range that covers that block.\n" +
    "Line-numbered snippet:\n" +
    lineNumberedTextBlock(fullContent) +
    "\n" +
    "Full snippet:\n" +
    toSafeTextCodeBlock(fullContent)
  );
}

/**
 * Runs the GenerateSnippetPlantUml AI action and parses the response.
 * Mirrors SnippetAiWorkflowSupport.generateSnippetPlantUml.
 */
export async function generateSnippetPlantUml(
  session: SnippetAiSession,
  fullContent: string,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<PlantUmlDiagramResponse> {
  const result = await invoke<AiExecutionResult>("execute_ai_action", {
    request: {
      action: "GenerateSnippetPlantUml",
      profileId: session.profileId,
      selectedText: fullContent,
      responseLanguageCode: session.languageCode,
      userPrompt: additionalInstructions?.trim() || undefined,
      conversationContext: buildPlantUmlContext(fullContent, snippetLanguage, session.languageCode),
    },
    requestId: crypto.randomUUID(),
  });
  return parsePlantUmlDiagramResponse(result.content);
}
