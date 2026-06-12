// Built-in and user-defined snippet editor color profiles.
// Port of de.kortty.core.SnippetEditorProfileSupport — the built-in profile
// ids, names and color values must stay identical to the Java implementation
// so that profile selections survive a migration in both directions.

import type { SnippetEditorCursorStyle, SnippetEditorProfile } from "../types/snippet";
import type { GlobalSettings } from "../store/settingsStore";

export const CURRENT_SETTINGS_PROFILE_ID = "current-settings";

const DEFAULT_CURSOR_STYLE: SnippetEditorCursorStyle = "BLOCK";
const HEX_COLOR = /^#[0-9A-Fa-f]{6}$/;

function profile(
  id: string,
  name: string,
  builtIn: boolean,
  foreground: string,
  background: string,
  cursorStyle: string,
  cursorColor: string,
  comment: string,
  string_: string,
  number: string,
  bool: string,
  key: string,
  keyword: string,
  section: string,
  variable: string,
  brace: string,
): SnippetEditorProfile {
  return {
    id,
    name,
    builtIn,
    formatterArgs: [],
    tabSize: 4,
    insertSpaces: true,
    foregroundColor: foreground,
    backgroundColor: background,
    cursorStyle: cursorStyle as SnippetEditorCursorStyle,
    cursorColor,
    commentColor: comment,
    stringColor: string_,
    numberColor: number,
    booleanColor: bool,
    keyColor: key,
    keywordColor: keyword,
    sectionColor: section,
    variableColor: variable,
    braceColor: brace,
  };
}

// Exact color values from SnippetEditorProfileSupport.BUILT_IN_PROFILES.
const BUILT_IN_PROFILES: readonly SnippetEditorProfile[] = [
  profile("preset-intellij-light", "IntelliJ Light", true,
    "#080808", "#FFFFFF", "LINE", "#000000", "#808080", "#008000", "#0000FF", "#000080", "#660E7A", "#000080", "#0033B3", "#660E7A", "#000000"),
  profile("preset-darcula", "Darcula", true,
    "#A9B7C6", "#2B2B2B", "BLOCK", "#BBBBBB", "#808080", "#6A8759", "#6897BB", "#CC7832", "#9876AA", "#CC7832", "#FFC66D", "#9876AA", "#A9B7C6"),
  profile("preset-high-contrast", "High Contrast", true,
    "#FFFFFF", "#000000", "BLOCK", "#FFFF00", "#A8A8A8", "#7CFF7C", "#81D4FA", "#FFB74D", "#FF80AB", "#82B1FF", "#FFFF00", "#FF80AB", "#FFFFFF"),
  profile("preset-new-ui-dark", "New UI Dark", true,
    "#CED0D6", "#1E1F22", "LINE", "#CED0D6", "#7A7E85", "#6AAB73", "#2AACB8", "#CF8E6D", "#C77DBB", "#CF8E6D", "#56A8F5", "#C77DBB", "#CED0D6"),
  profile("preset-warm-light", "Warm Light", true,
    "#1F2328", "#FAFAF7", "LINE", "#1F2328", "#7C7C78", "#067D17", "#1750EB", "#0033B3", "#871094", "#000080", "#7A3E9D", "#871094", "#1F2328"),
  profile("preset-blue-light", "Blue Light", true,
    "#1B1F2A", "#F4F8FF", "LINE", "#1B1F2A", "#6E7B8B", "#067D17", "#1D65C1", "#003B8E", "#8A1C7C", "#003B8E", "#265D9E", "#8A1C7C", "#1B1F2A"),
  profile("preset-graphite-dark", "Graphite Dark", true,
    "#D4D4D4", "#252526", "BLOCK", "#D4D4D4", "#858585", "#CE9178", "#B5CEA8", "#4EC9B0", "#9CDCFE", "#569CD6", "#C586C0", "#9CDCFE", "#D4D4D4"),
  profile("preset-night-owl", "Night Owl", true,
    "#D6DEEB", "#011627", "BLOCK", "#80CBC4", "#637777", "#ECC48D", "#F78C6C", "#FFCB8B", "#C792EA", "#82AAFF", "#7FDBCA", "#C792EA", "#D6DEEB"),
  profile("preset-solarized-light", "Solarized Light", true,
    "#586E75", "#FDF6E3", "LINE", "#586E75", "#93A1A1", "#2AA198", "#268BD2", "#B58900", "#D33682", "#859900", "#6C71C4", "#D33682", "#586E75"),
  profile("preset-solarized-dark", "Solarized Dark", true,
    "#839496", "#002B36", "BLOCK", "#93A1A1", "#586E75", "#2AA198", "#268BD2", "#B58900", "#D33682", "#859900", "#6C71C4", "#D33682", "#839496"),
];

export function builtInProfiles(): SnippetEditorProfile[] {
  return BUILT_IN_PROFILES.map((entry) => ({ ...entry, formatterArgs: [...entry.formatterArgs] }));
}

export function builtInProfile(id?: string | null): SnippetEditorProfile | undefined {
  if (!id || !id.trim()) {
    return undefined;
  }
  const found = BUILT_IN_PROFILES.find((entry) => entry.id === id);
  return found ? { ...found, formatterArgs: [...found.formatterArgs] } : undefined;
}

export function customProfiles(settings?: Pick<GlobalSettings, "snippetEditorProfiles"> | null): SnippetEditorProfile[] {
  if (!settings?.snippetEditorProfiles) {
    return [];
  }
  return settings.snippetEditorProfiles
    .filter((entry): entry is SnippetEditorProfile => !!entry && !entry.builtIn)
    .map((entry) => normalizeProfile(entry));
}

export function allProfiles(
  settings?: Pick<GlobalSettings, "snippetEditorProfiles"> | null,
): SnippetEditorProfile[] {
  return [...customProfiles(settings), ...builtInProfiles()];
}

export type SnippetEditorAppearanceSettings = Pick<
  GlobalSettings,
  | "snippetEditorProfiles"
  | "selectedSnippetEditorProfileId"
  | "snippetForegroundColor"
  | "snippetBackgroundColor"
  | "snippetCursorStyle"
  | "snippetCursorColor"
>;

export function resolveActiveProfile(settings?: SnippetEditorAppearanceSettings | null): SnippetEditorProfile {
  const selectedId = settings?.selectedSnippetEditorProfileId;
  if (selectedId && selectedId.trim()) {
    const selected = allProfiles(settings).find((entry) => entry.id === selectedId);
    if (selected) {
      return normalizeProfile(selected);
    }
  }
  return fromCurrentSettings(
    settings?.snippetForegroundColor,
    settings?.snippetBackgroundColor,
    settings?.snippetCursorStyle,
    settings?.snippetCursorColor,
  );
}

export function fromCurrentSettings(
  foregroundColor?: string,
  backgroundColor?: string,
  cursorStyle?: string,
  cursorColor?: string,
): SnippetEditorProfile {
  return normalizeProfile({
    id: CURRENT_SETTINGS_PROFILE_ID,
    name: "Custom colors",
    builtIn: false,
    formatterArgs: [],
    tabSize: 4,
    insertSpaces: true,
    foregroundColor,
    backgroundColor,
    cursorStyle: cursorStyle as SnippetEditorCursorStyle | undefined,
    cursorColor,
    commentColor: "#888888",
    stringColor: "#008800",
    numberColor: "#0066CC",
    booleanColor: "#CC00CC",
    keyColor: "#CC0000",
    keywordColor: "#0000CC",
    sectionColor: "#9900CC",
    variableColor: "#CC6600",
    braceColor: "#CC6600",
  });
}

export function normalizeProfile(source?: Partial<SnippetEditorProfile> | null): SnippetEditorProfile {
  const fallback = BUILT_IN_PROFILES[1];
  const base = source ?? fallback;
  return {
    id: nonBlank(base.id, crypto.randomUUID()),
    name: nonBlank(base.name, "Snippet editor profile"),
    language: base.language,
    builtIn: !!base.builtIn,
    formatterCommand: base.formatterCommand,
    formatterArgs: base.formatterArgs ? [...base.formatterArgs] : [],
    tabSize: base.tabSize && base.tabSize > 0 ? base.tabSize : 4,
    insertSpaces: base.insertSpaces ?? true,
    foregroundColor: hexColor(base.foregroundColor, fallback.foregroundColor),
    backgroundColor: hexColor(base.backgroundColor, fallback.backgroundColor),
    cursorStyle: normalizeCursorStyle(base.cursorStyle),
    cursorColor: hexColor(base.cursorColor, fallback.cursorColor),
    commentColor: hexColor(base.commentColor, fallback.commentColor),
    stringColor: hexColor(base.stringColor, fallback.stringColor),
    numberColor: hexColor(base.numberColor, fallback.numberColor),
    booleanColor: hexColor(base.booleanColor, fallback.booleanColor),
    keyColor: hexColor(base.keyColor, fallback.keyColor),
    keywordColor: hexColor(base.keywordColor, fallback.keywordColor),
    sectionColor: hexColor(base.sectionColor, fallback.sectionColor),
    variableColor: hexColor(base.variableColor, fallback.variableColor),
    braceColor: hexColor(base.braceColor, fallback.braceColor),
  };
}

export function hexColor(value?: string | null, fallback?: string | null): string {
  if (value && HEX_COLOR.test(value.trim())) {
    return value.trim().toUpperCase();
  }
  if (fallback && HEX_COLOR.test(fallback.trim())) {
    return fallback.trim().toUpperCase();
  }
  return "#000000";
}

export function normalizeCursorStyle(value?: string | null): SnippetEditorCursorStyle {
  if (!value || !value.trim()) {
    return DEFAULT_CURSOR_STYLE;
  }
  const normalized = value.trim().toUpperCase();
  switch (normalized) {
    case "LINE":
    case "UNDERSCORE":
    case "BLOCK":
      return normalized;
    default:
      return DEFAULT_CURSOR_STYLE;
  }
}

function nonBlank(value: string | undefined | null, fallback: string): string {
  return value && value.trim() ? value.trim() : fallback;
}
