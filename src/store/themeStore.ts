import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface ThemeData {
  id: string;
  name: string;
  foregroundColor: string;
  backgroundColor: string;
  cursorColor: string;
  selectionColor: string;
  fontFamily: string;
  fontSize: number;
  ansiColors: string[];
}

interface ThemeStore {
  theme: ThemeData;
  loading: boolean;
  loadActiveTheme: () => Promise<void>;
}

const DEFAULT_THEME: ThemeData = {
  id: "builtin-catppuccin-mocha",
  name: "Catppuccin Mocha",
  foregroundColor: "#cdd6f4",
  backgroundColor: "#1e1e2e",
  cursorColor: "#f5e0dc",
  selectionColor: "#45475a",
  fontFamily: "JetBrains Mono",
  fontSize: 14,
  ansiColors: [
    "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
    "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
    "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
    "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
  ],
};

export const useThemeStore = create<ThemeStore>((set) => ({
  theme: DEFAULT_THEME,
  loading: false,

  loadActiveTheme: async () => {
    set({ loading: true });
    try {
      const activeId = await invoke<string>("get_active_theme_id");
      const all = await invoke<ThemeData[]>("get_themes");
      const active = all.find((t) => t.id === activeId) ?? all[0] ?? DEFAULT_THEME;
      set({ theme: active, loading: false });
    } catch (err) {
      console.error("Failed to load active theme:", err);
      set({ loading: false });
    }
  },
}));
