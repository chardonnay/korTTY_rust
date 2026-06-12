// Maps a snippet editor color profile to a Monaco theme. Mirrors the token
// mapping of the Java Monaco host (comment/string/number/keyword/variable plus
// key→attribute.name, section→type and brace→delimiter) together with the
// cursor style semantics from EditorSettingsHelper (LINE/UNDERSCORE/BLOCK).

import { monaco } from "./monacoSetup";
import type { SnippetEditorProfile } from "../types/snippet";
import { hexColor, normalizeProfile } from "./snippetEditorProfiles";

export type MonacoCursorStyle = "line" | "block" | "underline";

export function toMonacoCursorStyle(cursorStyle?: string | null): MonacoCursorStyle {
  switch ((cursorStyle ?? "").trim().toUpperCase()) {
    case "LINE":
      return "line";
    case "UNDERSCORE":
    case "UNDERLINE":
      return "underline";
    default:
      return "block";
  }
}

export function profileThemeName(profile: Pick<SnippetEditorProfile, "id">): string {
  const sanitized = (profile.id || "default").replace(/[^a-zA-Z0-9-]/g, "-");
  return `kortty-snippet-${sanitized}`;
}

function withoutHash(color: string): string {
  return color.startsWith("#") ? color.slice(1) : color;
}

function isLightBackground(background: string): boolean {
  const hex = withoutHash(background);
  const r = parseInt(hex.slice(0, 2), 16);
  const g = parseInt(hex.slice(2, 4), 16);
  const b = parseInt(hex.slice(4, 6), 16);
  return 0.299 * r + 0.587 * g + 0.114 * b > 140;
}

/**
 * Defines (or redefines) a Monaco theme for the given snippet editor profile
 * and returns the theme name. Redefining the currently active theme makes
 * Monaco re-apply it, so live profile edits update open editors.
 */
export function ensureProfileTheme(profile: SnippetEditorProfile): string {
  const normalized = normalizeProfile(profile);
  const themeName = profileThemeName(normalized);
  const foreground = hexColor(normalized.foregroundColor, "#D4D4D4");
  const background = hexColor(normalized.backgroundColor, "#1E1E1E");
  const comment = hexColor(normalized.commentColor, foreground);
  const string = hexColor(normalized.stringColor, foreground);
  const number = hexColor(normalized.numberColor, foreground);
  const keyword = hexColor(normalized.keywordColor, foreground);
  const variable = hexColor(normalized.variableColor, foreground);
  const key = hexColor(normalized.keyColor, foreground);
  const section = hexColor(normalized.sectionColor, foreground);
  const brace = hexColor(normalized.braceColor, foreground);
  const cursor = hexColor(normalized.cursorColor, foreground);

  monaco.editor.defineTheme(themeName, {
    base: isLightBackground(background) ? "vs" : "vs-dark",
    inherit: true,
    rules: [
      { token: "", foreground: withoutHash(foreground) },
      { token: "comment", foreground: withoutHash(comment) },
      { token: "string", foreground: withoutHash(string) },
      { token: "number", foreground: withoutHash(number) },
      { token: "keyword", foreground: withoutHash(keyword) },
      { token: "variable", foreground: withoutHash(variable) },
      { token: "key", foreground: withoutHash(key) },
      { token: "attribute.name", foreground: withoutHash(key) },
      { token: "type", foreground: withoutHash(section) },
      { token: "delimiter", foreground: withoutHash(brace) },
    ],
    colors: {
      "editor.background": background,
      "editor.foreground": foreground,
      "editorCursor.foreground": cursor,
    },
  });
  return themeName;
}
