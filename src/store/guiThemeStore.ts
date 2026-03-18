import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface GuiThemeData {
  id: string;
  name: string;
  bg: string;
  surface: string;
  panel: string;
  border: string;
  text: string;
  textDim: string;
  accent: string;
  accentHover: string;
  success: string;
  warning: string;
  error: string;
  terminal: string;
}

interface GuiThemeStore {
  theme: GuiThemeData;
  loading: boolean;
  loadActiveGuiTheme: () => Promise<void>;
}

const DEFAULT_GUI_THEME: GuiThemeData = {
  id: "builtin-catppuccin-mocha",
  name: "Catppuccin Mocha",
  bg: "#1e1e2e",
  surface: "#252536",
  panel: "#2a2a3c",
  border: "#3a3a4c",
  text: "#cdd6f4",
  textDim: "#6c7086",
  accent: "#89b4fa",
  accentHover: "#74a8fc",
  success: "#a6e3a1",
  warning: "#f9e2af",
  error: "#f38ba8",
  terminal: "#11111b",
};

function hexToRgbChannels(hex: string): string {
  const h = hex.replace("#", "");
  const r = parseInt(h.substring(0, 2), 16);
  const g = parseInt(h.substring(2, 4), 16);
  const b = parseInt(h.substring(4, 6), 16);
  if (isNaN(r) || isNaN(g) || isNaN(b)) return "0 0 0";
  return `${r} ${g} ${b}`;
}

function applyGuiThemeToCss(theme: GuiThemeData) {
  const root = document.documentElement;
  root.style.setProperty("--kortty-bg", hexToRgbChannels(theme.bg));
  root.style.setProperty("--kortty-surface", hexToRgbChannels(theme.surface));
  root.style.setProperty("--kortty-panel", hexToRgbChannels(theme.panel));
  root.style.setProperty("--kortty-border", hexToRgbChannels(theme.border));
  root.style.setProperty("--kortty-text", hexToRgbChannels(theme.text));
  root.style.setProperty("--kortty-text-dim", hexToRgbChannels(theme.textDim));
  root.style.setProperty("--kortty-accent", hexToRgbChannels(theme.accent));
  root.style.setProperty("--kortty-accent-hover", hexToRgbChannels(theme.accentHover));
  root.style.setProperty("--kortty-success", hexToRgbChannels(theme.success));
  root.style.setProperty("--kortty-warning", hexToRgbChannels(theme.warning));
  root.style.setProperty("--kortty-error", hexToRgbChannels(theme.error));
  root.style.setProperty("--kortty-terminal", hexToRgbChannels(theme.terminal));
}

export const useGuiThemeStore = create<GuiThemeStore>((set) => ({
  theme: DEFAULT_GUI_THEME,
  loading: false,

  loadActiveGuiTheme: async () => {
    set({ loading: true });
    try {
      const activeId = await invoke<string>("get_active_gui_theme_id");
      const all = await invoke<GuiThemeData[]>("get_gui_themes");
      const active = all.find((t) => t.id === activeId) ?? all[0] ?? DEFAULT_GUI_THEME;
      applyGuiThemeToCss(active);
      set({ theme: active, loading: false });
    } catch (err) {
      console.error("Failed to load GUI theme:", err);
      set({ loading: false });
    }
  },
}));
