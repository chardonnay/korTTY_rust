export interface GuiLanguageSettingsLike {
  language?: string;
  autoDetectLanguage?: boolean;
}

export interface AiLanguageOption {
  value: string;
  label: string;
}

export const DEFAULT_AI_LANGUAGE_CODE = "en";

export const AI_LANGUAGE_OPTIONS: AiLanguageOption[] = [
  { value: "en", label: "English" },
  { value: "de", label: "Deutsch" },
  { value: "it", label: "Italiano" },
  { value: "es", label: "Español" },
  { value: "pt", label: "Português" },
  { value: "fr", label: "Français" },
  { value: "hr", label: "Hrvatski" },
  { value: "nl", label: "Nederlands" },
];

export function normalizeLanguageCode(value?: string | null): string {
  const trimmed = value?.trim();
  if (!trimmed) {
    return DEFAULT_AI_LANGUAGE_CODE;
  }

  const [primaryPart] = trimmed.split(/[-_]/);
  const normalized = primaryPart?.trim().toLowerCase();
  return normalized || DEFAULT_AI_LANGUAGE_CODE;
}

export function getBrowserLanguageCode(): string {
  if (typeof navigator === "undefined") {
    return DEFAULT_AI_LANGUAGE_CODE;
  }
  return normalizeLanguageCode(navigator.language);
}

export function resolveGuiLanguageCode(
  settings?: GuiLanguageSettingsLike | null,
  browserLanguage?: string,
): string {
  if (settings?.autoDetectLanguage) {
    return normalizeLanguageCode(browserLanguage ?? getBrowserLanguageCode());
  }
  if (settings?.language) {
    return normalizeLanguageCode(settings.language);
  }
  return normalizeLanguageCode(browserLanguage ?? getBrowserLanguageCode());
}

export function getAiLanguageLabel(languageCode: string): string {
  const normalized = normalizeLanguageCode(languageCode);
  return (
    AI_LANGUAGE_OPTIONS.find((option) => option.value === normalized)?.label ??
    normalized.toUpperCase()
  );
}
