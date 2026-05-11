export type TerminalEffectPluginSource = "Bundled" | "Imported";

export interface TerminalEffectPluginManifest {
  id: string;
  name: string;
  version?: string;
  description?: string;
  entry: string;
  css?: string;
  assets: string[];
}

export interface TerminalEffectPluginEntry {
  id: string;
  name: string;
  version?: string;
  description?: string;
  entryPath: string;
  cssPath?: string;
  packagePath: string;
  source: TerminalEffectPluginSource;
  enabled: boolean;
  bundled: boolean;
}

export interface TerminalEffectPluginBundle {
  manifest: TerminalEffectPluginManifest;
  entryJs: string;
  css?: string;
}

export const TERMINAL_EFFECT_SPEED_MINIMUM = 1;
export const TERMINAL_EFFECT_SPEED_DEFAULT = 1;
export const TERMINAL_EFFECT_SPEED_SLIDER_MAXIMUM = 10;
export const TERMINAL_EFFECT_SPEED_MAXIMUM = 99;

export function normalizeTerminalEffectSpeed(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return TERMINAL_EFFECT_SPEED_DEFAULT;
  return Math.min(TERMINAL_EFFECT_SPEED_MAXIMUM, Math.max(TERMINAL_EFFECT_SPEED_MINIMUM, value));
}
