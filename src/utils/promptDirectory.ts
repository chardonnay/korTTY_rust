/**
 * Heuristics that extract the remote working directory from visible shell
 * prompt lines. Port of the Java statics in `de.kortty.ui.TerminalView`
 * (`extractWorkingDirectoryFromPromptLine`, `extractWorkingDirectoryCandidate`,
 * `stripPromptDirectoryDecorations`, `extractWorkingDirectoryFromVisibleScreen`)
 * verified by `TerminalViewShortcutHeuristicsTest`.
 *
 * Supported prompt shapes include colon-separated (`user@host:~/dir$`),
 * space-separated (`user@host ~/dir $`) and bracketed (`[user@host ~/dir]$`)
 * prompts; `~`-relative candidates are expanded against the home directory
 * hint when it is absolute.
 */

/** Java `TerminalView.stripTerminalControlSequences`. */
export function stripTerminalControlSequences(value: string): string {
  return (value ?? "")
    .replace(/\u001B\[[;?0-9]*[ -/]*[@-~]/g, "")
    .replace(/\u001B\][\s\S]*?(?:\u0007|\u001B\\)/g, "");
}

/** Java `TerminalView.extractLastVisibleLine`. */
export function extractLastVisibleLine(value: string): string {
  const normalized = stripTerminalControlSequences(value)
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n");
  const index = normalized.lastIndexOf("\n");
  return index >= 0 ? normalized.slice(index + 1).trim() : normalized.trim();
}

/** Java `TerminalView.looksLikeShellPrompt`. */
function looksLikeShellPrompt(line: string): boolean {
  const normalized = (line ?? "").replace(/\s+$/u, "");
  return (
    normalized.endsWith("$") ||
    normalized.endsWith("#") ||
    normalized.endsWith("%") ||
    normalized.endsWith(">") ||
    /.*\[[^\]]+\]\$$/.test(normalized)
  );
}

/** Java `TerminalView.extractPromptPrefixFromVisibleLine`. */
function extractPromptPrefixFromVisibleLine(normalizedLine: string): string {
  if (!normalizedLine || !normalizedLine.trim()) {
    return "";
  }
  if (looksLikeShellPrompt(normalizedLine)) {
    return normalizedLine;
  }
  for (let i = normalizedLine.length - 2; i >= 0; i -= 1) {
    const ch = normalizedLine[i];
    if (
      (ch === "$" || ch === "#" || ch === "%" || ch === ">") &&
      /\s/u.test(normalizedLine[i + 1] ?? "")
    ) {
      const candidate = normalizedLine.slice(0, i + 1).replace(/\s+$/u, "");
      if (looksLikeShellPrompt(candidate)) {
        return candidate;
      }
    }
  }
  return "";
}

/** Java `TerminalView.lastWhitespaceIndex`. */
function lastWhitespaceIndex(value: string): number {
  for (let i = value.length - 1; i >= 0; i -= 1) {
    if (/\s/u.test(value[i])) {
      return i;
    }
  }
  return -1;
}

/** Java `TerminalView.stripPromptDirectoryDecorations`. */
export function stripPromptDirectoryDecorations(candidate: string): string {
  let stripped = (candidate ?? "").trim();
  while (stripped.length > 0 && (stripped.endsWith("]") || stripped.endsWith(")"))) {
    stripped = stripped.slice(0, -1).replace(/\s+$/u, "");
  }
  return stripped;
}

/** Java `TerminalView.extractWorkingDirectoryCandidate`. */
export function extractWorkingDirectoryCandidate(beforePrompt: string): string {
  let normalized = (beforePrompt ?? "").trim();
  if (!normalized) {
    return "";
  }
  const separator = normalized.lastIndexOf(":");
  if (separator >= 0 && separator + 1 < normalized.length) {
    return stripPromptDirectoryDecorations(normalized.slice(separator + 1).trim());
  }
  if (normalized.endsWith("]") || normalized.endsWith(")")) {
    normalized = normalized.slice(0, -1).replace(/\s+$/u, "");
  }
  const whitespace = lastWhitespaceIndex(normalized);
  if (whitespace < 0 || whitespace + 1 >= normalized.length) {
    return "";
  }
  return stripPromptDirectoryDecorations(normalized.slice(whitespace + 1).trim());
}

/** Java `TerminalView.extractWorkingDirectoryFromPromptLine`. */
export function extractWorkingDirectoryFromPromptLine(
  line: string,
  homeDirectory?: string | null,
): string | null {
  const normalized = (line ?? "").replace(/\s+$/u, "");
  const prompt = extractPromptPrefixFromVisibleLine(normalized);
  if (!prompt.trim()) {
    return null;
  }
  const beforePrompt = prompt.slice(0, prompt.length - 1).replace(/\s+$/u, "");
  const candidate = extractWorkingDirectoryCandidate(beforePrompt);
  if (!candidate.trim()) {
    return null;
  }
  if (candidate.startsWith("/")) {
    return candidate;
  }
  if (candidate === "~") {
    return homeDirectory && homeDirectory.startsWith("/") ? homeDirectory : null;
  }
  if (candidate.startsWith("~/")) {
    return homeDirectory && homeDirectory.startsWith("/")
      ? homeDirectory + candidate.slice(1)
      : null;
  }
  return null;
}

/** Java `TerminalView.extractWorkingDirectoryFromVisibleScreen`. */
export function extractWorkingDirectoryFromVisibleScreen(
  screenLines: string,
  homeDirectory?: string | null,
): string | null {
  const normalized = (screenLines ?? "").replace(/\r\n/g, "\n").replace(/\r/g, "\n");
  const lines = normalized.split("\n");
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    const directory = extractWorkingDirectoryFromPromptLine(lines[i], homeDirectory);
    if (directory != null) {
      return directory;
    }
  }
  return null;
}
