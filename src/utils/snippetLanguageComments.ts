// Snippet language normalization and comment-syntax helpers.
// Port of de.kortty.core.SnippetLanguageSupport (language detection) and the
// comment-formatting half of de.kortty.core.SnippetAiTextSupport.

export const DEFAULT_DESCRIPTION_WRAP_WIDTH = 80;

export interface CommentFormat {
  prefix: string;
  suffix: string | null;
  block: boolean;
}

export function normalizeSnippetLanguage(language?: string | null): string {
  if (!language || !language.trim()) return "plain";
  switch (language.trim().toLowerCase()) {
    case "sh":
    case "shell":
    case "zsh":
    case "bash":
      return "bash";
    case "py":
    case "python":
    case "python3":
      return "python";
    case "pl":
    case "perl":
      return "perl";
    case "rb":
    case "ruby":
      return "ruby";
    case "js":
    case "javascript":
    case "node":
    case "nodejs":
      return "javascript";
    case "ps":
    case "ps1":
    case "pwsh":
    case "powershell":
      return "powershell";
    case "groovy":
      return "groovy";
    case "java":
      return "java";
    case "json":
      return "json";
    case "yaml":
    case "yml":
      return "yaml";
    case "xml":
      return "xml";
    case "markdown":
    case "md":
      return "markdown";
    case "asciidoctor":
    case "asciidoc":
    case "adoc":
      return "asciidoctor";
    case "sql":
      return "sql";
    case "dockerfile":
      return "dockerfile";
    case "properties":
    case "ini":
      return "properties";
    case "html":
      return "html";
    case "plain":
    case "text":
    case "txt":
      return "plain";
    default:
      return language.trim().toLowerCase();
  }
}

function resolveShebangInterpreterToken(shebangCommand: string): string | null {
  if (!shebangCommand.trim()) return null;
  const tokens = shebangCommand.trim().split(/\s+/);
  if (tokens.length === 0) return null;
  const command = tokens[0];
  const normalizedCommand = command.toLowerCase();
  if (normalizedCommand === "env" || normalizedCommand.endsWith("/env")) {
    for (let index = 1; index < tokens.length; index++) {
      const token = tokens[index];
      if (!token || !token.trim() || token.startsWith("-")) continue;
      return token;
    }
    return null;
  }
  return command;
}

function detectShebangLanguage(content?: string | null): string | null {
  if (!content || !content.trim()) return null;
  const firstLine = content.split("\n", 1)[0]?.trim() ?? "";
  if (!firstLine.startsWith("#!")) return null;
  const interpreterToken = resolveShebangInterpreterToken(firstLine.slice(2).trim());
  if (!interpreterToken) return null;
  let interpreterName = interpreterToken;
  const lastSlash = interpreterName.lastIndexOf("/");
  if (lastSlash >= 0 && lastSlash + 1 < interpreterName.length) {
    interpreterName = interpreterName.slice(lastSlash + 1);
  }
  const normalized = interpreterName.toLowerCase();
  if (normalized.startsWith("python")) return "python";
  if (normalized.startsWith("perl")) return "perl";
  if (normalized.startsWith("ruby")) return "ruby";
  if (normalized === "pwsh" || normalized === "powershell") return "powershell";
  if (normalized === "bash" || normalized === "sh" || normalized === "zsh") return "bash";
  return null;
}

export function detectSnippetLanguage(language?: string | null, content?: string | null): string {
  const normalized = normalizeSnippetLanguage(language);
  if (normalized !== "plain") {
    return normalized;
  }
  return detectShebangLanguage(content) ?? "plain";
}

export function commentFormat(language?: string | null): CommentFormat | null {
  switch (normalizeSnippetLanguage(language)) {
    case "bash":
    case "python":
    case "perl":
    case "ruby":
    case "powershell":
    case "dockerfile":
    case "yaml":
    case "properties":
      return { prefix: "#", suffix: null, block: false };
    case "java":
    case "javascript":
    case "groovy":
      return { prefix: "//", suffix: null, block: false };
    case "sql":
      return { prefix: "--", suffix: null, block: false };
    case "xml":
    case "html":
      return { prefix: "<!--", suffix: "-->", block: true };
    default:
      return null;
  }
}

export function supportsCommentFormatting(language?: string | null): boolean {
  return commentFormat(language) !== null;
}

// ---- description wrapping (port of SnippetAiTextSupport text helpers) ----

function normalizePlainDescription(text?: string | null): string {
  if (!text) return "";
  return text
    .replace(/\r/g, "\n")
    .replace(/```(?:\w+)?\n?/g, "")
    .trim();
}

function looksLikeShortAbbreviation(sentenceCandidate: string): boolean {
  const lastSpace = sentenceCandidate.lastIndexOf(" ");
  const lastToken = sentenceCandidate.slice(lastSpace + 1);
  return lastToken.length === 2 && lastToken.endsWith(".");
}

/** Splits a collapsed paragraph into one line per sentence (after periods). */
function splitSentencesAfterPeriods(text: string): string[] {
  const sentences: string[] = [];
  let start = 0;
  for (let index = 0; index < text.length; index++) {
    if (text[index] !== ".") continue;
    const next = index + 1;
    if (next >= text.length || !/\s/.test(text[next])) continue;
    let following = next;
    while (following < text.length && /\s/.test(text[following])) {
      following++;
    }
    if (following >= text.length) continue;
    const candidate = text.slice(start, index + 1).trim();
    if (candidate && !looksLikeShortAbbreviation(candidate)) {
      sentences.push(candidate);
      start = following;
    }
  }
  const tail = text.slice(start).trim();
  if (tail) {
    sentences.push(tail);
  }
  return sentences;
}

function wrapSingleLine(text: string, width: number): string {
  let result = "";
  let lineLength = 0;
  for (const word of text.split(/\s+/)) {
    if (!word) continue;
    const additionalLength = lineLength === 0 ? word.length : word.length + 1;
    if (lineLength > 0 && lineLength + additionalLength > width) {
      result += `\n${word}`;
      lineLength = word.length;
    } else {
      if (lineLength > 0) {
        result += " ";
        lineLength++;
      }
      result += word;
      lineLength += word.length;
    }
  }
  return result;
}

function wrapParagraph(paragraph: string, width: number): string {
  const collapsed = paragraph.replace(/\s*\n\s*/g, " ").trim();
  if (!collapsed) return collapsed;
  const sentenceLines = splitSentencesAfterPeriods(collapsed);
  if (sentenceLines.length > 1) {
    return sentenceLines.map((sentence) => wrapSingleLine(sentence, width)).join("\n");
  }
  if (collapsed.length <= width) {
    return collapsed;
  }
  return wrapSingleLine(collapsed, width);
}

export function wrapDescriptionText(text: string | null | undefined, maxLineLength: number): string {
  const normalized = normalizePlainDescription(text);
  const width = Math.max(20, maxLineLength);
  if (!normalized) return normalized;
  const paragraphs = normalized.split(/\n\s*\n/);
  const wrappedParagraphs: string[] = [];
  for (const paragraph of paragraphs) {
    const trimmedParagraph = paragraph ? paragraph.trim() : "";
    if (!trimmedParagraph) continue;
    wrappedParagraphs.push(wrapParagraph(trimmedParagraph, width));
  }
  return wrappedParagraphs.join("\n\n");
}

function commentContentWidth(format: CommentFormat | null, indentation: string, maxLineLength: number): number {
  if (!format) return maxLineLength;
  const linePrefix = format.block ? `${indentation} ` : `${indentation}${format.prefix} `;
  return Math.max(20, maxLineLength - linePrefix.length);
}

/**
 * Formats an AI-generated description as a comment block in the snippet
 * language (port of SnippetAiTextSupport.formatDescriptionAsComment).
 */
export function formatDescriptionAsComment(
  description: string | null | undefined,
  language: string | null | undefined,
  indent: string | null | undefined,
  maxLineLength: number = DEFAULT_DESCRIPTION_WRAP_WIDTH,
): string {
  const format = commentFormat(language);
  const indentation = indent ?? "";
  const contentWidth = commentContentWidth(format, indentation, maxLineLength);
  const normalizedDescription = wrapDescriptionText(description, contentWidth);
  if (!normalizedDescription.trim() || !format) {
    return normalizedDescription;
  }
  const lines = normalizedDescription.split(/\r\n|\r|\n/);
  if (format.block) {
    let blockText = `${indentation}${format.prefix}\n`;
    for (const line of lines) {
      blockText += `${indentation} ${line.replace(/\s+$/, "")}\n`;
    }
    blockText += `${indentation}${format.suffix ?? ""}`;
    return blockText;
  }
  return lines
    .map((line) => `${indentation}${format.prefix} ${line.replace(/\s+$/, "")}`)
    .join("\n");
}

/** Port of SnippetAiTextSupport.findLineIndentation. */
export function findLineIndentation(text: string | null | undefined, offset: number): string {
  if (!text) return "";
  const safeOffset = Math.max(0, Math.min(offset, text.length));
  let lineStart = text.lastIndexOf("\n", Math.max(0, safeOffset - 1));
  lineStart = lineStart < 0 ? 0 : lineStart + 1;
  let cursor = lineStart;
  while (cursor < text.length) {
    const character = text[cursor];
    if (character !== " " && character !== "\t") break;
    cursor++;
  }
  return text.slice(lineStart, cursor);
}

/** Offset of the first non-whitespace character (Java firstContentOffset). */
export function firstContentOffset(text: string | null | undefined): number {
  if (!text) return 0;
  for (let index = 0; index < text.length; index++) {
    if (!/\s/.test(text[index])) {
      return index;
    }
  }
  return 0;
}

/** Start offset of the line containing `offset` (Java startOfLine). */
export function startOfLine(text: string | null | undefined, offset: number): number {
  const content = text ?? "";
  const safeOffset = Math.max(0, Math.min(offset, content.length));
  const lineStart = content.lastIndexOf("\n", Math.max(0, safeOffset - 1));
  return lineStart < 0 ? 0 : lineStart + 1;
}
