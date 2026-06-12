// Tolerant parsing of structured snippet AI responses.
// Port of de.kortty.core.SnippetAiResponseSupport.

export interface CompletionSuggestion {
  insertText: string;
  summary: string;
}

export interface CodeReviewFinding {
  id: string;
  severity: string;
  title: string;
  detail: string;
  recommendation: string;
  line?: number;
}

export interface CodeImprovement {
  replacement: string;
  summary: string;
}

export interface AlternativeSolution {
  title: string;
  code: string;
  summary: string;
}

export interface SecurityFinding {
  id: string;
  severity: string;
  title: string;
  impact: string;
  recommendation: string;
}

export interface OneLinerSuggestion {
  command: string;
}

const JSON_OBJECT_PATTERN = /\{[\s\S]*\}/;
const JSON_ARRAY_PATTERN = /\[[\s\S]*\]/;
const MARKDOWN_CODE_BLOCK_PATTERN = /```[A-Za-z0-9_+.#-]*\r?\n([\s\S]*?)\r?\n?```/;

type JsonValue = unknown;
type JsonObject = Record<string, unknown>;

function isRecord(value: unknown): value is JsonObject {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function asTrimmedString(value: unknown): string | null {
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  return null;
}

function firstString(object: JsonObject | null, ...names: string[]): string {
  if (!object) return "";
  for (const name of names) {
    const value = object[name];
    const text = asTrimmedString(value);
    if (value !== null && value !== undefined && text !== null) {
      return text;
    }
  }
  return "";
}

function firstArray(object: JsonObject | null, ...names: string[]): unknown[] | null {
  if (!object) return null;
  for (const name of names) {
    const value = object[name];
    if (Array.isArray(value)) {
      return value;
    }
  }
  return null;
}

function nonBlank(value: string | null | undefined, fallback: string): string {
  return value && value.trim() ? value.trim() : fallback;
}

function parseJsonElement(candidate: string | null | undefined): JsonValue | null {
  if (!candidate || !candidate.trim()) return null;
  try {
    return JSON.parse(candidate);
  } catch {
    return null;
  }
}

function extractJsonPayload(responseText: string | null | undefined): string | null {
  if (!responseText || !responseText.trim()) return null;
  const objectMatch = responseText.match(JSON_OBJECT_PATTERN);
  if (objectMatch) return objectMatch[0];
  const arrayMatch = responseText.match(JSON_ARRAY_PATTERN);
  return arrayMatch ? arrayMatch[0] : null;
}

function parseJsonObject(responseText: string | null | undefined): JsonObject | null {
  const candidate = extractJsonPayload(responseText);
  if (!candidate) return null;
  const root = parseJsonElement(candidate);
  return isRecord(root) ? root : null;
}

function parseArrayFieldRoot(root: JsonValue | null, fieldName: string): unknown[] | null {
  if (root === null || root === undefined) return null;
  if (Array.isArray(root)) return root;
  if (isRecord(root) && Array.isArray(root[fieldName])) {
    return root[fieldName] as unknown[];
  }
  return null;
}

function parseArrayField(responseText: string | null | undefined, fieldName: string): unknown[] | null {
  let array = parseArrayFieldRoot(parseJsonElement(responseText ?? null), fieldName);
  if (array) return array;
  array = parseArrayFieldRoot(parseJsonElement(extractJsonPayload(responseText)), fieldName);
  if (array) return array;
  const arrayMatch = responseText ? responseText.match(JSON_ARRAY_PATTERN) : null;
  return arrayMatch ? parseArrayFieldRoot(parseJsonElement(arrayMatch[0]), fieldName) : null;
}

/** Port of SnippetAiTextSupport.normalizePlainText: strips Markdown fences. */
export function normalizePlainText(text: string | null | undefined): string {
  if (!text) return "";
  return text
    .replace(/\r/g, "\n")
    .replace(/```(?:\w+)?\n?/g, "")
    .trim();
}

// ---- completion ----

export function isUsableCompletion(suggestion: CompletionSuggestion | null | undefined): boolean {
  return !!suggestion && !!suggestion.insertText && suggestion.insertText.trim().length > 0;
}

export function parseCompletionSuggestion(responseText: string | null | undefined): CompletionSuggestion {
  const object = parseJsonObject(responseText);
  if (!object) {
    return { insertText: "", summary: "" };
  }
  const suggestion: CompletionSuggestion = {
    insertText: firstString(object, "insertText", "completion", "text", "code"),
    summary: firstString(object, "summary", "description").trim(),
  };
  return isUsableCompletion(suggestion) ? suggestion : { insertText: "", summary: "" };
}

// ---- code review findings ----

function isUsableReviewFinding(finding: CodeReviewFinding): boolean {
  return !!(finding.title.trim() || finding.detail.trim() || finding.recommendation.trim());
}

function parseCodeReviewFinding(element: unknown, fallbackIndex: number): CodeReviewFinding | null {
  if (!isRecord(element)) return null;
  let line: number | undefined;
  const rawLine = element["line"];
  if (typeof rawLine === "number" && Number.isFinite(rawLine)) {
    line = Math.trunc(rawLine);
  } else if (typeof rawLine === "string" && /^\d+$/.test(rawLine.trim())) {
    line = Number.parseInt(rawLine.trim(), 10);
  }
  const finding: CodeReviewFinding = {
    id: nonBlank(firstString(element, "id"), `R${fallbackIndex}`),
    severity: nonBlank(firstString(element, "severity"), "info"),
    title: firstString(element, "title").trim(),
    detail: firstString(element, "detail", "impact", "description").trim(),
    recommendation: firstString(element, "recommendation", "fix", "suggestion").trim(),
    line,
  };
  return isUsableReviewFinding(finding) ? finding : null;
}

export function parseCodeReviewFindings(responseText: string | null | undefined): CodeReviewFinding[] {
  const findings = parseArrayField(responseText, "findings");
  if (!findings) return [];
  const parsed: CodeReviewFinding[] = [];
  let fallbackIndex = 1;
  for (const element of findings) {
    const finding = parseCodeReviewFinding(element, fallbackIndex++);
    if (finding) parsed.push(finding);
  }
  return parsed;
}

// ---- code improvement ----

export function isUsableImprovement(improvement: CodeImprovement | null | undefined): boolean {
  return !!improvement && !!improvement.replacement && improvement.replacement.trim().length > 0;
}

/** Escapes regex metacharacters so a dynamic field name matches literally
 * (mirrors Java Pattern.quote in SnippetAiResponseSupport). */
function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function extractLenientJsonStringField(text: string, fieldName: string): string | null {
  const fieldPattern = new RegExp(`"${escapeRegExp(fieldName)}"\\s*:\\s*"`);
  const match = fieldPattern.exec(text);
  if (!match) return null;
  const start = match.index + match[0].length;
  let value = "";
  let escaping = false;
  for (let index = start; index < text.length; index++) {
    const character = text[index];
    if (escaping) {
      switch (character) {
        case "n":
          value += "\n";
          break;
        case "r":
          value += "\r";
          break;
        case "t":
          value += "\t";
          break;
        case "b":
          value += "\b";
          break;
        case "f":
          value += "\f";
          break;
        default:
          value += character;
      }
      escaping = false;
      continue;
    }
    if (character === "\\") {
      escaping = true;
      continue;
    }
    if (character === '"' && looksLikeFieldTerminator(text, index + 1)) {
      return value;
    }
    value += character;
  }
  return value;
}

function looksLikeFieldTerminator(text: string, offset: number): boolean {
  let index = offset;
  while (index < text.length && /\s/.test(text[index])) {
    index++;
  }
  return index >= text.length || text[index] === "," || text[index] === "}" || text[index] === "]";
}

function parseLenientCodeImprovement(responseText: string | null | undefined): CodeImprovement | null {
  const value = responseText ? responseText.trim() : "";
  if (!value || !value.includes('"replacement"')) return null;
  const replacement = extractLenientJsonStringField(value, "replacement");
  if (!replacement || !replacement.trim()) return null;
  const summary = extractLenientJsonStringField(value, "summary") ?? "";
  return { replacement, summary: summary.trim() };
}

function extractPlainCodeFallback(responseText: string | null | undefined): string {
  const sanitized = responseText ? responseText.trim() : "";
  if (!sanitized) return "";
  const lenient = parseLenientCodeImprovement(sanitized);
  if (lenient && isUsableImprovement(lenient)) {
    return lenient.replacement;
  }
  const codeBlock = sanitized.match(MARKDOWN_CODE_BLOCK_PATTERN);
  return codeBlock ? codeBlock[1] : sanitized;
}

function parseNestedCodeImprovement(replacement: string, outerSummary: string): CodeImprovement | null {
  if (!replacement || !replacement.trim()) return null;
  const nestedObject = parseJsonObject(replacement);
  if (nestedObject) {
    const nested: CodeImprovement = {
      replacement: firstString(nestedObject, "replacement", "code", "content", "text"),
      summary: nonBlank(firstString(nestedObject, "summary", "description"), outerSummary),
    };
    return isUsableImprovement(nested) ? nested : null;
  }
  const lenient = parseLenientCodeImprovement(replacement);
  if (lenient && isUsableImprovement(lenient)) {
    return { replacement: lenient.replacement, summary: nonBlank(lenient.summary, outerSummary) };
  }
  return null;
}

export function parseCodeImprovement(
  responseText: string | null | undefined,
  allowPlainTextFallback = false,
): CodeImprovement {
  const object = parseJsonObject(responseText);
  if (!object && !allowPlainTextFallback) {
    return { replacement: "", summary: "" };
  }
  if (!object) {
    let fallback = parseLenientCodeImprovement(responseText);
    if (!fallback || !isUsableImprovement(fallback)) {
      fallback = { replacement: extractPlainCodeFallback(responseText), summary: "" };
    }
    return isUsableImprovement(fallback) ? fallback : { replacement: "", summary: "" };
  }
  const replacement = firstString(object, "replacement", "code", "content", "text");
  const summary = firstString(object, "summary", "description").trim();
  const nested = parseNestedCodeImprovement(replacement, summary);
  if (nested && isUsableImprovement(nested)) {
    return nested;
  }
  const improvement: CodeImprovement = { replacement, summary };
  return isUsableImprovement(improvement) ? improvement : { replacement: "", summary: "" };
}

// ---- alternative solutions ----

export function isUsableAlternative(solution: AlternativeSolution | null | undefined): boolean {
  return !!solution && !!solution.code && solution.code.trim().length > 0;
}

function parseAlternativeSolution(element: unknown, fallbackIndex: number): AlternativeSolution | null {
  if (element === null || element === undefined) return null;
  if (typeof element === "string") {
    const solution: AlternativeSolution = {
      title: `Alternative ${fallbackIndex}`,
      code: element.trim(),
      summary: "",
    };
    return isUsableAlternative(solution) ? solution : null;
  }
  if (!isRecord(element)) return null;
  const solution: AlternativeSolution = {
    title: nonBlank(firstString(element, "title"), `Alternative ${fallbackIndex}`),
    code: firstString(element, "code", "replacement", "content", "text", "solution").trim(),
    summary: firstString(element, "summary", "description", "explanation").trim(),
  };
  return isUsableAlternative(solution) ? solution : null;
}

export function parseAlternativeSolutions(
  responseText: string | null | undefined,
  maxSolutions: number,
): AlternativeSolution[] {
  const limit = Math.max(1, maxSolutions);
  let root = parseJsonElement(responseText ?? null);
  if (root === null) {
    root = parseJsonElement(extractJsonPayload(responseText));
  }
  if (root === null) return [];
  let solutions: unknown[] | null = null;
  if (isRecord(root)) {
    solutions = firstArray(root, "solutions", "alternatives", "alternativeSolutions", "results");
    if (!solutions) {
      const single = parseAlternativeSolution(root, 1);
      return single ? [single] : [];
    }
  } else if (Array.isArray(root)) {
    solutions = root;
  }
  if (!solutions) return [];
  const parsed: AlternativeSolution[] = [];
  for (const element of solutions) {
    if (parsed.length >= limit) break;
    const solution = parseAlternativeSolution(element, parsed.length + 1);
    if (solution) parsed.push(solution);
  }
  return parsed;
}

// ---- security findings ----

function isUsableSecurityFinding(finding: SecurityFinding): boolean {
  return !!(finding.title.trim() || finding.impact.trim() || finding.recommendation.trim());
}

function parseSecurityFinding(element: unknown, fallbackIndex: number): SecurityFinding | null {
  if (!isRecord(element)) return null;
  const finding: SecurityFinding = {
    id: nonBlank(firstString(element, "id"), `S${fallbackIndex}`),
    severity: nonBlank(firstString(element, "severity"), "info"),
    title: firstString(element, "title").trim(),
    impact: firstString(element, "impact", "detail", "description").trim(),
    recommendation: firstString(element, "recommendation", "fix", "suggestion").trim(),
  };
  return isUsableSecurityFinding(finding) ? finding : null;
}

export function parseSecurityFindings(responseText: string | null | undefined): SecurityFinding[] {
  const findings = parseArrayField(responseText, "findings");
  if (!findings) return [];
  const parsed: SecurityFinding[] = [];
  let fallbackIndex = 1;
  for (const element of findings) {
    const finding = parseSecurityFinding(element, fallbackIndex++);
    if (finding) parsed.push(finding);
  }
  return parsed;
}

// ---- one-liner ----

export function isUsableOneLiner(suggestion: OneLinerSuggestion | null | undefined): boolean {
  if (!suggestion) return false;
  const command = suggestion.command;
  return !!command && command.trim().length > 0 && !command.includes("\n") && !command.includes("\r");
}

export function parseOneLinerSuggestion(responseText: string | null | undefined): OneLinerSuggestion {
  const object = parseJsonObject(responseText);
  if (!object) return { command: "" };
  const suggestion: OneLinerSuggestion = {
    command: firstString(object, "command", "oneLiner", "one_liner", "line").trim(),
  };
  return isUsableOneLiner(suggestion) ? suggestion : { command: "" };
}

// ---- segment replacements (correct/translate selection) ----

function extractReplacementText(element: unknown): string | null {
  if (element === null || element === undefined) return null;
  if (typeof element === "string" || typeof element === "number" || typeof element === "boolean") {
    return String(element);
  }
  if (!isRecord(element)) return null;
  for (const key of ["text", "replacement", "content"]) {
    const value = element[key];
    if (value !== null && value !== undefined) {
      const text = asTrimmedString(value);
      if (text !== null) return text;
    }
  }
  return null;
}

export function parseSegmentReplacements(
  responseText: string | null | undefined,
  expectedCount: number,
): string[] {
  if (expectedCount <= 0) return [];
  const jsonCandidate = extractJsonPayload(responseText);
  if (jsonCandidate) {
    const root = parseJsonElement(jsonCandidate);
    let segments: unknown[] | null = null;
    if (isRecord(root)) {
      segments = firstArray(root, "segments", "replacements");
    } else if (Array.isArray(root)) {
      segments = root;
    }
    if (segments) {
      const replacements: string[] = [];
      for (const element of segments) {
        if (replacements.length >= expectedCount) break;
        const text = extractReplacementText(element);
        if (text === null) continue;
        replacements.push(text);
      }
      if (replacements.length > 0) {
        return replacements;
      }
    }
  }
  if (expectedCount === 1) {
    return [normalizePlainText(responseText)];
  }
  return [];
}
