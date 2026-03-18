import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export type TeamworkSourceType = "Git" | "SharedFile";

export interface TeamworkSourceConfig {
  id: string;
  sourceType: TeamworkSourceType;
  location: string;
  checkIntervalMinutes: number;
  readOnly: boolean;
  enabled: boolean;
}

export interface GlobalSettings {
  language: string;
  autoDetectLanguage: boolean;
  defaultFontFamily: string;
  defaultFontSize: number;
  defaultColumns: number;
  defaultRows: number;
  defaultScrollbackLines: number;
  defaultSshKeepaliveEnabled: boolean;
  defaultSshKeepaliveInterval: number;
  defaultConnectionTimeout: number;
  defaultRetryCount: number;
  storeWindowGeometry: boolean;
  storeDashboardState: boolean;
  backupEncryptionType: "Password" | "GPG";
  backupCredentialId?: string;
  backupGpgKeyId?: string;
  maxBackups: number;
  translationProvider?: "Google" | "DeepL" | "LibreTranslate" | "Microsoft" | "Yandex";
  translationApiKey?: string;
  translationApiUrl?: string;
  teamworkSources: TeamworkSourceConfig[];
  teamworkDefaultCheckIntervalMinutes: number;
  teamworkDefaultCredentialId?: string;
  teamworkDefaultSshKeyId?: string;
  teamworkDefaultUsername?: string;
  teamworkUseTemporaryKey: boolean;
  defaultCommandTimestampsEnabled: boolean;
  defaultPromptHookEnabled: boolean;
}

interface SettingsStore {
  settings: GlobalSettings;
  loading: boolean;
  loadSettings: () => Promise<void>;
  saveSettings: (settings: GlobalSettings) => Promise<void>;
}

export const useSettingsStore = create<SettingsStore>((set) => ({
  settings: {
    language: "en",
    autoDetectLanguage: true,
    defaultFontFamily: "JetBrains Mono",
    defaultFontSize: 14,
    defaultColumns: 80,
    defaultRows: 24,
    defaultScrollbackLines: 10000,
    defaultSshKeepaliveEnabled: true,
    defaultSshKeepaliveInterval: 60,
    defaultConnectionTimeout: 15,
    defaultRetryCount: 4,
    storeWindowGeometry: true,
    storeDashboardState: true,
    backupEncryptionType: "Password",
    maxBackups: 10,
    teamworkSources: [],
    teamworkDefaultCheckIntervalMinutes: 15,
    teamworkUseTemporaryKey: false,
    defaultCommandTimestampsEnabled: false,
    defaultPromptHookEnabled: true,
  },
  loading: false,

  loadSettings: async () => {
    set({ loading: true });
    try {
      const settings = await invoke<GlobalSettings>("get_settings");
      set({ settings, loading: false });
    } catch (err) {
      console.error("Failed to load settings:", err);
      set({ loading: false });
    }
  },

  saveSettings: async (settings) => {
    try {
      await invoke("save_settings", { settings });
      set({ settings });
    } catch (err) {
      console.error("Failed to save settings:", err);
    }
  },
}));
