// Snippet-editor AI workflow helpers: prompt-context builders, editable text
// segment extraction, one-liner output validation, and invoke wrappers around
// the execute_ai_action Tauri command.
// Port of de.kortty.core.SnippetAiWorkflowSupport plus the segment half of
// de.kortty.core.SnippetAiTextSupport.

import { invoke } from "@tauri-apps/api/core";
import type { AiExecutionResult, AiProfile } from "../types/ai";
import type { GlobalSettings } from "../store/settingsStore";
import { resolvePreferredAiProfileId } from "./aiProfiles";
import { DEFAULT_AI_LANGUAGE_CODE, resolveGuiLanguageCode } from "./aiLanguage";
import { detectSnippetLanguage } from "./snippetLanguageComments";
import {
  type AlternativeSolution,
  type CodeImprovement,
  type CodeReviewFinding,
  type CompletionSuggestion,
  type OneLinerSuggestion,
  type SecurityFinding,
  isUsableOneLiner,
  normalizePlainText,
  parseAlternativeSolutions,
  parseCodeImprovement,
  parseCodeReviewFindings,
  parseCompletionSuggestion,
  parseOneLinerSuggestion,
  parseSecurityFindings,
  parseSegmentReplacements,
} from "./snippetAiResponse";

// ---- prompt building blocks ----

/** Port of AiPromptBuilder.toSafeTextCodeBlock. */
export function toSafeTextCodeBlock(text: string | null | undefined): string {
  const content = text ?? "";
  let fence = "```";
  while (content.includes(fence)) {
    fence += "`";
  }
  return `${fence}text\n${content}\n${fence}`;
}

export function lineNumberedTextBlock(text: string | null | undefined): string {
  const value = text ?? "";
  const lines = value.split(/\r\n|\r|\n/);
  const width = String(Math.max(1, lines.length)).length;
  let block = "```text\n";
  for (let index = 0; index < lines.length; index++) {
    block += `${String(index + 1).padStart(width, " ")} | ${lines[index]}\n`;
  }
  block += "```";
  return block;
}

export function mergeAdditionalInstructions(
  additionalInstructions?: string | null,
  extraLine?: string | null,
): string | undefined {
  let merged = "";
  if (additionalInstructions && additionalInstructions.trim()) {
    merged = additionalInstructions.trim();
  }
  if (extraLine && extraLine.trim()) {
    merged = merged ? `${merged}\n${extraLine.trim()}` : extraLine.trim();
  }
  return merged || undefined;
}

export interface CursorLocation {
  offset: number;
  line: number;
  column: number;
}

/** Port of SnippetEditDialog.cursorLocation. */
export function cursorLocation(content: string | null | undefined, cursorOffset: number): CursorLocation {
  const value = content ?? "";
  const safeOffset = Math.max(0, Math.min(cursorOffset, value.length));
  let line = 1;
  let column = 1;
  for (let index = 0; index < safeOffset; index++) {
    const character = value[index];
    if (character === "\n") {
      line++;
      column = 1;
    } else if (character !== "\r") {
      column++;
    }
  }
  return { offset: safeOffset, line, column };
}

// ---- context builders (ports of the SnippetAiWorkflowSupport builders) ----

export function buildCompletionContext(
  fullContent: string | null | undefined,
  cursorOffset: number,
  snippetLanguage: string,
): string {
  const content = fullContent ?? "";
  const safeOffset = Math.max(0, Math.min(cursorOffset, content.length));
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Cursor offset: ${safeOffset}\n` +
    `Text before cursor:\n${toSafeTextCodeBlock(content.slice(0, safeOffset))}\n` +
    `Text after cursor:\n${toSafeTextCodeBlock(content.slice(safeOffset))}`
  );
}

export function buildAssistantContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
  cursorOffset: number,
  cursorLine: number,
  cursorColumn: number,
): string {
  const content = fullContent ?? "";
  const safeOffset = Math.max(0, Math.min(cursorOffset, content.length));
  const safeLine = Math.max(1, cursorLine);
  const safeColumn = Math.max(1, cursorColumn);
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Natural language for summary: ${fallbackLanguageCode}\n` +
    `Cursor offset: ${safeOffset}\n` +
    `Cursor line: ${safeLine}\n` +
    `Cursor column: ${safeColumn}\n` +
    "The cursor marks the user's focal point. Apply the user instruction to the whole snippet when needed, but avoid unrelated rewrites.\n" +
    `Full snippet:\n${toSafeTextCodeBlock(content)}`
  );
}

export function buildAlternativeContext(
  fullContent: string | null | undefined,
  targetText: string | null | undefined,
  wholeSnippet: boolean,
  snippetLanguage: string,
  fallbackLanguageCode: string,
  maxSolutions: number,
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Alternative target scope: ${wholeSnippet ? "full snippet" : "selected code region"}\n` +
    `Return at most ${maxSolutions} solutions.\n` +
    "Keep the generated code in the same programming language as the snippet language.\n" +
    "Each solution code must replace exactly the target scope, not any surrounding context.\n" +
    `If you add comments or user-facing strings, use the natural language already dominant in the snippet when it is clear; otherwise use fallback language ${fallbackLanguageCode}.\n` +
    `Target scope to replace:\n${toSafeTextCodeBlock(targetText ?? "")}\n` +
    `Full snippet for context:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

export function buildSelectedCodeContext(
  fullContent: string | null | undefined,
  selectedText: string | null | undefined,
  wholeSnippet: boolean,
  snippetLanguage: string,
  fallbackLanguageCode: string,
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Natural language for report text: ${fallbackLanguageCode}\n` +
    `Scope: ${wholeSnippet ? "full snippet" : "selected code region"}\n` +
    `Full snippet for context:\n${toSafeTextCodeBlock(fullContent ?? "")}\n` +
    `Selected code region:\n${toSafeTextCodeBlock(wholeSnippet ? fullContent ?? "" : selectedText ?? "")}`
  );
}

export function buildSecurityContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Natural language for the security report: ${fallbackLanguageCode}\n` +
    "Use secure-by-default guidance for the snippet language, but only report issues supported by this code.\n" +
    `Full snippet:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

export function buildSecurityFixContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
  selectedFindings: SecurityFinding[],
): string {
  const findingsText = selectedFindings
    .map(
      (finding) =>
        `${finding.id} [${finding.severity}] ${finding.title}\nImpact: ${finding.impact}\nRecommendation: ${finding.recommendation}`,
    )
    .join("\n\n");
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Natural language for the summary: ${fallbackLanguageCode}\n` +
    `Selected security findings to fix:\n${toSafeTextCodeBlock(findingsText)}\n` +
    `Full snippet to update:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

export function buildOneLinerContext(fullContent: string | null | undefined, snippetLanguage: string): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    "Generate a compact one-liner, not an embedded/base64 wrapper. " +
    "Use only the provided snippet content. Do not download code, do not reference external URLs, and do not invent files or endpoints. " +
    "For shell snippets, use shell syntax on one line. " +
    "For Python, Perl, or Ruby snippets, use an interpreter command such as python3 -c, perl -e, or ruby -e when needed. " +
    "Preserve behavior and quote safely.\n" +
    `Full snippet:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

export function buildDescriptionContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Fallback natural language for the description: ${fallbackLanguageCode}\n` +
    "Use the natural language already dominant in existing comments or user-facing strings when it is clear; otherwise use the fallback language.\n" +
    `Full snippet for context:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

function buildSelectionTransformContext(
  fullContent: string | null | undefined,
  snippetLanguage: string,
  fallbackLanguageCode: string,
  segments: EditableTextSegment[],
): string {
  return (
    `Snippet language: ${snippetLanguage}\n` +
    `Fallback natural language for comments and user-facing strings: ${fallbackLanguageCode}\n` +
    "Use the natural language already dominant in existing comments or user-facing strings when it is clear; otherwise use the fallback language.\n" +
    `Editable text segments JSON:\n${toSegmentsJson(segments)}\n` +
    `Full snippet for context only:\n${toSafeTextCodeBlock(fullContent ?? "")}`
  );
}

// ---- one-liner output validation (port of isAllowedGeneratedOneLiner) ----

const URL_PATTERN = /https?:\/\/[^\s'"<>]+/gi;
const DOWNLOAD_COMMAND_PATTERN = /(?:^|[\s;&|()])(?:curl|wget)(?:\s|$)/i;
const TEMP_FILE_PATTERN = /(?:\/tmp\/|\$TMPDIR\b|\$\{TMPDIR\}|\bmktemp\b)/i;

function containsIntroducedPattern(pattern: RegExp, command: string, source: string): boolean {
  if (!pattern.test(command)) return false;
  return !pattern.test(source);
}

/**
 * Rejects generated one-liners that introduce heredocs, downloads, temp
 * files, or URLs that are not part of the original snippet.
 */
export function isAllowedGeneratedOneLiner(
  suggestion: OneLinerSuggestion | null | undefined,
  fullContent: string | null | undefined,
): boolean {
  if (!isUsableOneLiner(suggestion)) return false;
  const command = suggestion!.command;
  const source = fullContent ?? "";
  if (command.includes("<<")) return false;
  if (
    containsIntroducedPattern(DOWNLOAD_COMMAND_PATTERN, command, source) ||
    containsIntroducedPattern(TEMP_FILE_PATTERN, command, source)
  ) {
    return false;
  }
  const matches = command.match(URL_PATTERN) ?? [];
  for (const url of matches) {
    if (!source.includes(url)) {
      return false;
    }
  }
  return true;
}

// ---- editable text segments (port of SnippetAiTextSupport segments) ----

export type SegmentType = "comment" | "string" | "xml_comment";

export interface EditableTextSegment {
  start: number;
  end: number;
  originalText: string;
  type: SegmentType;
}

const C_STYLE_BLOCK_COMMENT_PATTERN = /\/\*([\s\S]*?)\*\//g;
const XML_COMMENT_PATTERN = /<!--([\s\S]*?)-->/g;
const DOUBLE_QUOTED_PATTERN = /"((?:[^"\\]|\\[\s\S])*)"/g;
const SINGLE_QUOTED_PATTERN = /'((?:[^'\\]|\\[\s\S])*)'/g;
const BACKTICK_QUOTED_PATTERN = /`((?:[^`\\]|\\[\s\S])*)`/g;
const PYTHON_TRIPLE_DOUBLE_PATTERN = /"""([\s\S]*?)"""/g;
const PYTHON_TRIPLE_SINGLE_PATTERN = /'''([\s\S]*?)'''/g;

function leadingWhitespace(value: string): string {
  const match = value.match(/^\s*/);
  return match ? match[0] : "";
}

function trailingWhitespace(value: string): string {
  const match = value.match(/\s*$/);
  return match ? match[0] : "";
}

export function segmentCoreText(segment: EditableTextSegment): string {
  return segment.originalText.trim();
}

function segmentApplyReplacement(segment: EditableTextSegment, replacement: string | null | undefined): string {
  const normalizedReplacement = replacement ? replacement.trim() : "";
  if (!normalizedReplacement) {
    return segment.originalText;
  }
  return leadingWhitespace(segment.originalText) + normalizedReplacement + trailingWhitespace(segment.originalText);
}

function looksLikeCodeOnlyString(originalText: string): boolean {
  const coreText = originalText.trim();
  if (!coreText) return true;
  if (coreText.includes("$") || coreText.includes("${") || coreText.includes("%")) {
    return true;
  }
  return /^[~./A-Za-z0-9_-]+(\/[~./A-Za-z0-9_.-]+)+$/.test(coreText);
}

/**
 * Collects group-1 matches as editable segments. `suffixLength` is the fixed
 * number of pattern characters after group 1 (closing quote/comment marker),
 * which makes the group offsets exact without RegExp match indices.
 */
function addGroupSegments(
  text: string,
  segments: EditableTextSegment[],
  pattern: RegExp,
  type: SegmentType,
  suffixLength: number,
): void {
  pattern.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = pattern.exec(text)) !== null) {
    if (match[0].length === 0) {
      pattern.lastIndex++;
      continue;
    }
    const group = match[1];
    if (group === undefined || group.length === 0) continue;
    const end = match.index + match[0].length - suffixLength;
    const start = end - group.length;
    if (start < 0 || end <= start) continue;
    const originalText = text.slice(start, end);
    if (!originalText.trim()) continue;
    if (type === "string" && looksLikeCodeOnlyString(originalText)) continue;
    segments.push({ start, end, originalText, type });
  }
}

function extractHashStyleSegments(text: string, segments: EditableTextSegment[], language: string): void {
  const commentPattern =
    language === "properties"
      ? /^[ \t]*[;#](.*)$/gm
      : /^[^\S\n]*#(?!!)(.*)$/gm;
  addGroupSegments(text, segments, commentPattern, "comment", 0);
  addGroupSegments(text, segments, DOUBLE_QUOTED_PATTERN, "string", 1);
  addGroupSegments(text, segments, SINGLE_QUOTED_PATTERN, "string", 1);
  if (language === "python") {
    addGroupSegments(text, segments, PYTHON_TRIPLE_DOUBLE_PATTERN, "string", 3);
    addGroupSegments(text, segments, PYTHON_TRIPLE_SINGLE_PATTERN, "string", 3);
  }
}

function extractCStyleSegments(text: string, segments: EditableTextSegment[], includeBackticks: boolean): void {
  addGroupSegments(text, segments, /^[^\S\n]*\/\/(.*)$/gm, "comment", 0);
  addGroupSegments(text, segments, C_STYLE_BLOCK_COMMENT_PATTERN, "comment", 2);
  addGroupSegments(text, segments, DOUBLE_QUOTED_PATTERN, "string", 1);
  addGroupSegments(text, segments, SINGLE_QUOTED_PATTERN, "string", 1);
  if (includeBackticks) {
    addGroupSegments(text, segments, BACKTICK_QUOTED_PATTERN, "string", 1);
  }
}

function extractSqlSegments(text: string, segments: EditableTextSegment[]): void {
  addGroupSegments(text, segments, /^[^\S\n]*--(.*)$/gm, "comment", 0);
  addGroupSegments(text, segments, C_STYLE_BLOCK_COMMENT_PATTERN, "comment", 2);
  addGroupSegments(text, segments, SINGLE_QUOTED_PATTERN, "string", 1);
}

function extractXmlSegments(text: string, segments: EditableTextSegment[]): void {
  addGroupSegments(text, segments, XML_COMMENT_PATTERN, "xml_comment", 3);
}

export function extractEditableSegments(
  selectedText: string | null | undefined,
  language: string | null | undefined,
): EditableTextSegment[] {
  const text = selectedText ?? "";
  if (!text.trim()) return [];
  const normalizedLanguage = detectSnippetLanguage(language, text);
  const segments: EditableTextSegment[] = [];
  switch (normalizedLanguage) {
    case "bash":
    case "python":
    case "perl":
    case "ruby":
    case "powershell":
    case "dockerfile":
    case "yaml":
    case "properties":
      extractHashStyleSegments(text, segments, normalizedLanguage);
      break;
    case "java":
    case "javascript":
    case "groovy":
      extractCStyleSegments(text, segments, true);
      break;
    case "sql":
      extractSqlSegments(text, segments);
      break;
    case "xml":
    case "html":
      extractXmlSegments(text, segments);
      break;
    default:
      return [];
  }
  segments.sort((left, right) => left.start - right.start);
  return segments;
}

export function applySegmentReplacements(
  selectedText: string | null | undefined,
  segments: EditableTextSegment[],
  replacements: string[],
): string {
  const text = selectedText ?? "";
  if (!text || segments.length === 0) return text;
  let result = text;
  const count = Math.min(segments.length, replacements.length);
  for (let index = count - 1; index >= 0; index--) {
    const segment = segments[index];
    const replacement = replacements[index];
    if (!segment || replacement === null || replacement === undefined || !replacement.trim()) {
      continue;
    }
    result = result.slice(0, segment.start) + segmentApplyReplacement(segment, replacement) + result.slice(segment.end);
  }
  return result;
}

export function toSegmentsJson(segments: EditableTextSegment[]): string {
  return JSON.stringify(
    segments.map((segment, index) => ({
      index,
      type: segment.type,
      text: segmentCoreText(segment),
    })),
  );
}

// ---- invoke wrappers around execute_ai_action ----

/** Mirrors the backend AiRequestPayload (camelCase serde). */
interface SnippetAiRequestPayload {
  action: string;
  profileId: string;
  selectedText: string;
  connectionDisplayName?: string;
  responseLanguageCode?: string;
  userPrompt?: string;
  conversationContext?: string;
  includeAiSkills?: boolean;
}

export interface SnippetAiSession {
  profileId: string;
  languageCode: string;
  skillsAvailable: boolean;
}

/**
 * Resolves the AI profile and response language the snippet workflows should
 * use (same resolution as the snippet metadata flow in SnippetManager).
 * Returns null when no AI profile is configured.
 */
export async function resolveSnippetAiSession(): Promise<SnippetAiSession | null> {
  const [profiles, settings] = await Promise.all([
    invoke<AiProfile[]>("get_ai_profiles"),
    invoke<GlobalSettings>("get_settings").catch(() => null),
  ]);
  const profileId = resolvePreferredAiProfileId(profiles, settings?.defaultAiProfileId);
  if (!profileId) return null;
  const languageCode = resolveGuiLanguageCode(settings) || DEFAULT_AI_LANGUAGE_CODE;
  const skillsAvailable = !!settings?.aiSkills?.some(
    (skill) =>
      skill.enabled !== false &&
      (skill.target === "Chat" || skill.target === "Both") &&
      !!skill.content?.trim(),
  );
  return { profileId, languageCode, skillsAvailable };
}

async function executeSnippetAiAction(
  session: SnippetAiSession,
  action: string,
  selectedText: string,
  userPrompt?: string,
  conversationContext?: string,
  includeAiSkills?: boolean,
): Promise<string> {
  const request: SnippetAiRequestPayload = {
    action,
    profileId: session.profileId,
    selectedText,
    responseLanguageCode: session.languageCode,
    userPrompt,
    conversationContext,
    includeAiSkills,
  };
  const result = await invoke<AiExecutionResult>("execute_ai_action", {
    request,
    requestId: crypto.randomUUID(),
  });
  return result.content;
}

export async function completeSnippetCode(
  session: SnippetAiSession,
  fullContent: string,
  cursorOffset: number,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<CompletionSuggestion> {
  const content = await executeSnippetAiAction(
    session,
    "CompleteSnippetCode",
    fullContent,
    additionalInstructions,
    buildCompletionContext(fullContent, cursorOffset, snippetLanguage),
  );
  return parseCompletionSuggestion(content);
}

export async function reviewSnippetCode(
  session: SnippetAiSession,
  fullContent: string,
  selectedText: string,
  wholeSnippet: boolean,
  snippetLanguage: string,
  reviewTheme?: string,
  additionalInstructions?: string,
): Promise<CodeReviewFinding[]> {
  const content = await executeSnippetAiAction(
    session,
    "ReviewSnippetCode",
    wholeSnippet ? fullContent : selectedText,
    mergeAdditionalInstructions(reviewTheme, additionalInstructions),
    buildSelectedCodeContext(fullContent, selectedText, wholeSnippet, snippetLanguage, session.languageCode),
  );
  return parseCodeReviewFindings(content);
}

export async function improveSnippetCode(
  session: SnippetAiSession,
  fullContent: string,
  selectedText: string,
  snippetLanguage: string,
  improvementTheme?: string,
  additionalInstructions?: string,
  allowPlainTextFallback = false,
): Promise<CodeImprovement> {
  const content = await executeSnippetAiAction(
    session,
    "ImproveSnippetCode",
    selectedText,
    mergeAdditionalInstructions(improvementTheme, additionalInstructions),
    buildSelectedCodeContext(fullContent, selectedText, false, snippetLanguage, session.languageCode),
  );
  return parseCodeImprovement(content, allowPlainTextFallback);
}

export async function assistSnippetCode(
  session: SnippetAiSession,
  fullContent: string,
  snippetLanguage: string,
  cursor: CursorLocation,
  userInstruction: string,
  includeAiSkills: boolean,
  additionalInstructions?: string,
): Promise<CodeImprovement> {
  const content = await executeSnippetAiAction(
    session,
    "AssistSnippetCode",
    fullContent ?? "",
    mergeAdditionalInstructions(userInstruction, additionalInstructions),
    buildAssistantContext(fullContent, snippetLanguage, session.languageCode, cursor.offset, cursor.line, cursor.column),
    includeAiSkills,
  );
  return parseCodeImprovement(content);
}

export async function reviewSnippetSecurity(
  session: SnippetAiSession,
  fullContent: string,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<SecurityFinding[]> {
  const content = await executeSnippetAiAction(
    session,
    "SecurityReviewSnippetCode",
    fullContent,
    additionalInstructions,
    buildSecurityContext(fullContent, snippetLanguage, session.languageCode),
  );
  return parseSecurityFindings(content);
}

export async function applySnippetSecurityFixes(
  session: SnippetAiSession,
  fullContent: string,
  snippetLanguage: string,
  selectedFindings: SecurityFinding[],
  additionalInstructions?: string,
): Promise<CodeImprovement> {
  const content = await executeSnippetAiAction(
    session,
    "ApplySnippetSecurityFixes",
    fullContent,
    additionalInstructions,
    buildSecurityFixContext(fullContent, snippetLanguage, session.languageCode, selectedFindings),
  );
  return parseCodeImprovement(content);
}

export async function generateAlternativeSolutions(
  session: SnippetAiSession,
  fullContent: string,
  targetText: string,
  wholeSnippet: boolean,
  snippetLanguage: string,
  maxSolutions: number,
  additionalInstructions?: string,
): Promise<AlternativeSolution[]> {
  const content = await executeSnippetAiAction(
    session,
    "GenerateSnippetAlternatives",
    targetText,
    additionalInstructions,
    buildAlternativeContext(fullContent, targetText, wholeSnippet, snippetLanguage, session.languageCode, maxSolutions),
  );
  return parseAlternativeSolutions(content, maxSolutions);
}

export async function describeSnippet(
  session: SnippetAiSession,
  fullContent: string,
  selectedText: string,
  wholeSnippet: boolean,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<string> {
  const content = await executeSnippetAiAction(
    session,
    wholeSnippet ? "DescribeSnippetFull" : "DescribeSnippetSelection",
    selectedText,
    additionalInstructions,
    buildDescriptionContext(fullContent, snippetLanguage, session.languageCode),
  );
  return normalizePlainText(content);
}

export async function generateCompactOneLiner(
  session: SnippetAiSession,
  fullContent: string,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<OneLinerSuggestion> {
  const content = await executeSnippetAiAction(
    session,
    "GenerateSnippetOneLiner",
    fullContent,
    additionalInstructions,
    buildOneLinerContext(fullContent, snippetLanguage),
  );
  const suggestion = parseOneLinerSuggestion(content);
  return isAllowedGeneratedOneLiner(suggestion, fullContent) ? suggestion : { command: "" };
}

async function transformSelectedText(
  session: SnippetAiSession,
  action: "CorrectSnippetSelectionText" | "TranslateSnippetSelectionText",
  fullContent: string,
  selectedText: string,
  snippetLanguage: string,
  responseLanguageCode: string,
  additionalInstructions?: string,
): Promise<string> {
  const segments = extractEditableSegments(selectedText, snippetLanguage);
  if (segments.length === 0) {
    return selectedText ?? "";
  }
  const request: SnippetAiRequestPayload = {
    action,
    profileId: session.profileId,
    selectedText,
    responseLanguageCode,
    userPrompt: additionalInstructions,
    conversationContext: buildSelectionTransformContext(fullContent, snippetLanguage, responseLanguageCode, segments),
  };
  const result = await invoke<AiExecutionResult>("execute_ai_action", {
    request,
    requestId: crypto.randomUUID(),
  });
  const replacements = parseSegmentReplacements(result.content, segments.length);
  if (replacements.length === 0) {
    return selectedText ?? "";
  }
  return applySegmentReplacements(selectedText, segments, replacements);
}

export async function correctSelectionText(
  session: SnippetAiSession,
  fullContent: string,
  selectedText: string,
  snippetLanguage: string,
  additionalInstructions?: string,
): Promise<string> {
  return transformSelectedText(
    session,
    "CorrectSnippetSelectionText",
    fullContent,
    selectedText,
    snippetLanguage,
    session.languageCode,
    additionalInstructions,
  );
}

export async function translateSelectionText(
  session: SnippetAiSession,
  fullContent: string,
  selectedText: string,
  snippetLanguage: string,
  targetLanguageCode: string,
  additionalInstructions?: string,
): Promise<string> {
  const fallbackNote = session.languageCode
    ? `Source-language hint for existing comments and user-facing strings: ${session.languageCode}`
    : "";
  return transformSelectedText(
    session,
    "TranslateSnippetSelectionText",
    fullContent,
    selectedText,
    snippetLanguage,
    targetLanguageCode && targetLanguageCode.trim() ? targetLanguageCode.trim() : session.languageCode,
    mergeAdditionalInstructions(additionalInstructions, fallbackNote),
  );
}
