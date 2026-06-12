import { create } from "zustand";
import type { AppDesign } from "./settingsStore";
import {
  GuiThemeData,
  setGuiThemeDesignOverride,
} from "./guiThemeStore";

/**
 * Color palettes for the built-in app designs, derived from the Java
 * design stylesheets (styles/matrix-terminal.css, holographic.css,
 * tactical.css, elegant.css). Every palette provides the full set of
 * fields expected by applyGuiThemeToCss. The `terminal` value is only a
 * fallback — the design override always keeps the terminal background of
 * the active GUI theme so terminal sessions keep their own colors.
 */
export const DESIGN_PALETTES: Record<Exclude<AppDesign, "normal">, GuiThemeData> = {
  "matrix-terminal": {
    id: "app-design-matrix-terminal",
    name: "Matrix Terminal",
    bg: "#080c09",
    surface: "#0d1b0f",
    panel: "#0d1b0f",
    border: "#00c96e",
    text: "#00ff88",
    textDim: "#00a05a",
    accent: "#00ff88",
    accentHover: "#00c96e",
    success: "#00ff88",
    warning: "#ffd200",
    error: "#ff3c5a",
    terminal: "#080c09",
  },
  "holographic-interface": {
    id: "app-design-holographic-interface",
    name: "Holographic Interface",
    bg: "#000000",
    surface: "#040d16",
    panel: "#061520",
    border: "#006a80",
    text: "#5cd6f2",
    textDim: "#007a99",
    accent: "#00d4ff",
    accentHover: "#33ddff",
    success: "#27ae60",
    warning: "#ff8c00",
    error: "#ff3c5a",
    terminal: "#000000",
  },
  "klingon-tactical": {
    id: "app-design-klingon-tactical",
    name: "Klingon Tactical",
    bg: "#0d0906",
    surface: "#140c08",
    panel: "#1a0808",
    border: "#5e1722",
    text: "#e8b0b0",
    textDim: "#8a5050",
    accent: "#ff3c5a",
    accentHover: "#ff647e",
    success: "#ffd200",
    warning: "#ff8c00",
    error: "#ff3c5a",
    terminal: "#0d0906",
  },
  "elegant-dark": {
    id: "app-design-elegant-dark",
    name: "Elegant Dark",
    bg: "#1a1c20",
    surface: "#212428",
    panel: "#272b30",
    border: "#33373d",
    text: "#e2e4e8",
    textDim: "#8b9099",
    accent: "#c8a96e",
    accentHover: "#d4b87f",
    success: "#27ae60",
    warning: "#e0b341",
    error: "#e74c3c",
    terminal: "#1a1c20",
  },
};

export const APP_DESIGN_IDS: AppDesign[] = [
  "normal",
  "matrix-terminal",
  "holographic-interface",
  "klingon-tactical",
  "elegant-dark",
];

export function normalizeAppDesign(raw: string | undefined | null): AppDesign {
  if (!raw) return "normal";
  const candidate = raw.trim().toLowerCase().replace(/_/g, "-");
  return (APP_DESIGN_IDS as string[]).includes(candidate)
    ? (candidate as AppDesign)
    : "normal";
}

/**
 * Applies the given app design to the document: sets the scoping
 * data attribute used by the design stylesheets and overrides the GUI
 * theme palette. "normal" removes the design layer and re-applies the
 * active GUI theme.
 */
export function applyAppDesign(id: AppDesign) {
  const design = normalizeAppDesign(id);
  const root = document.documentElement;
  if (design === "normal") {
    delete root.dataset.appDesign;
    setGuiThemeDesignOverride(null);
  } else {
    root.dataset.appDesign = design;
    setGuiThemeDesignOverride(DESIGN_PALETTES[design]);
  }
  useAppDesignStore.setState({ design });
}

interface AppDesignStore {
  design: AppDesign;
  applyAppDesign: (id: AppDesign) => void;
}

export const useAppDesignStore = create<AppDesignStore>(() => ({
  design: "normal",
  applyAppDesign,
}));
