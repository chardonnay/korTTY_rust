// Validates terminal selections that should be opened as remote text files
// and resolves remote "save as" sibling file names.
// Port of de.kortty.core.RemoteTextFileSelectionSupport plus the file-name
// validation half of de.kortty.core.SftpFileTransferService.

/** Thrown when a terminal selection is not a single plain file name. */
export class InvalidRemoteFileSelectionError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "InvalidRemoteFileSelectionError";
  }
}

function stripMatchingQuotes(text: string): string {
  if (text.length < 2) return text;
  const first = text[0];
  const last = text[text.length - 1];
  if ((first === "'" && last === "'") || (first === '"' && last === '"')) {
    return text.slice(1, -1).trim();
  }
  return text;
}

/**
 * Port of RemoteTextFileSelectionSupport.normalizeSelectedFileName: trims the
 * selection, strips one pair of matching quotes and rejects selections that
 * are empty, span multiple lines, contain NUL bytes, are '.'/'..' or contain
 * path separators.
 */
export function normalizeSelectedFileName(selectedText: string | null | undefined): string {
  let normalized = (selectedText ?? "").trim();
  normalized = stripMatchingQuotes(normalized);
  if (!normalized) {
    throw new InvalidRemoteFileSelectionError("Selected text must contain one file name");
  }
  if (normalized.includes("\0") || normalized.includes("\n") || normalized.includes("\r")) {
    throw new InvalidRemoteFileSelectionError("Selected text must contain one file name");
  }
  if (normalized === "." || normalized === "..") {
    throw new InvalidRemoteFileSelectionError("Selected text must be a file name");
  }
  if (normalized.includes("/") || normalized.includes("\\")) {
    throw new InvalidRemoteFileSelectionError(
      "Selected text must be a file name in the current directory",
    );
  }
  return normalized;
}

function trimTrailingSlash(path: string): string {
  let result = path;
  while (result.length > 1 && result.endsWith("/")) {
    result = result.slice(0, -1);
  }
  return result;
}

function normalizedDirectoryOrCurrent(directory: string | null | undefined): string {
  const normalized = (directory ?? "").trim();
  return normalized ? trimTrailingSlash(normalized) : ".";
}

/**
 * Remote paths are always POSIX and must be absolute. Any directory that did
 * not resolve to an absolute "/"-rooted path (a relative tracked working dir,
 * a "."/empty fallback, etc.) is anchored at the POSIX root so the resulting
 * SFTP path is never relative.
 */
function toAbsoluteRemoteDirectory(directory: string): string {
  if (directory.startsWith("/")) {
    return directory;
  }
  if (!directory || directory === ".") {
    return "/";
  }
  return trimTrailingSlash(`/${directory.replace(/^\.\//, "")}`);
}

function appendRemotePath(basePath: string, relativePath: string): string {
  const base = normalizedDirectoryOrCurrent(basePath);
  const relative = (relativePath ?? "").trim();
  if (!relative) return base;
  if (base === "/") return `/${relative}`;
  return base.endsWith("/") ? `${base}${relative}` : `${base}/${relative}`;
}

function resolveRemoteDirectory(
  workingDirectory: string | null | undefined,
  sftpStartDirectory: string | null | undefined,
): string {
  const fallback = normalizedDirectoryOrCurrent(sftpStartDirectory);
  if (!workingDirectory || !workingDirectory.trim()) {
    return fallback;
  }
  const tracked = workingDirectory.trim();
  if (tracked.startsWith("/")) {
    return trimTrailingSlash(tracked);
  }
  if (tracked === "~") {
    return fallback;
  }
  if (tracked.startsWith("~/")) {
    const relativeToHome = tracked.slice(2);
    return relativeToHome.trim() ? appendRemotePath(fallback, relativeToHome) : fallback;
  }
  return trimTrailingSlash(tracked);
}

/**
 * Resolves the absolute remote path for a selected file name against the
 * tracked terminal working directory, falling back to the SFTP start (home)
 * directory for "~"-relative paths.
 * Port of RemoteTextFileSelectionSupport.resolveRemoteFilePath.
 */
export function resolveRemoteFilePath(
  workingDirectory: string | null | undefined,
  selectedFileName: string,
  sftpStartDirectory?: string | null,
): string {
  const fileName = normalizeSelectedFileName(selectedFileName);
  const directory = toAbsoluteRemoteDirectory(
    resolveRemoteDirectory(workingDirectory, sftpStartDirectory),
  );
  if (directory === "/") return `/${fileName}`;
  return directory.endsWith("/") ? `${directory}${fileName}` : `${directory}/${fileName}`;
}

/** Validation outcome for the remote "save as" file-name dialog. */
export type RemoteSiblingFileNameError = "empty" | "dots" | "separators";

/**
 * Frontend mirror of the Rust/Java validateRemoteSiblingFileName rules,
 * returning a stable error code for localized live validation.
 */
export function validateRemoteSiblingFileName(
  newFileName: string | null | undefined,
): { fileName: string } | { error: RemoteSiblingFileNameError } {
  const normalized = (newFileName ?? "").trim();
  if (!normalized) return { error: "empty" };
  if (normalized === "." || normalized === "..") return { error: "dots" };
  if (normalized.includes("/") || normalized.includes("\\")) return { error: "separators" };
  return { fileName: normalized };
}
