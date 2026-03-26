import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";

export interface ConnectionSettings {
  id: string;
  name: string;
  group?: string;
  host: string;
  port: number;
  username: string;
  connectionProtocol: "TcpIp" | "Mosh";
  authMethod: "Password" | "PrivateKey";
  password?: string;
  credentialId?: string;
  sshKeyId?: string;
  privateKeyPath?: string;
  privateKeyPassphrase?: string;
  temporaryKeyContent?: string;
  temporaryKeyExpirationMinutes?: number;
  temporaryKeyPermanent: boolean;
  fontFamily: string;
  fontSize: number;
  columns: number;
  rows: number;
  scrollbackLines: number;
  foregroundColor: string;
  backgroundColor: string;
  cursorColor: string;
  cursorStyle: "Block" | "Underline" | "Bar";
  ansiColors: string[];
  sshKeepaliveEnabled: boolean;
  sshKeepaliveInterval: number;
  connectionTimeout: number;
  retryCount: number;
  terminalLogging: boolean;
  commandTimestamps: boolean;
  themeId?: string;
  jumpServer?: JumpServerConfig;
  tunnels: TunnelConfig[];
  tabGroup?: string;
  usageCount: number;
  lastUsed?: string;
  connectionSource?: "Local" | "Teamwork";
  teamworkSourceId?: string;
  teamworkVersionToken?: string;
  teamworkRole?: string;
}

export interface JumpServerConfig {
  enabled: boolean;
  host: string;
  port: number;
  username: string;
  authMethod: "Password" | "PrivateKey";
  password?: string;
  sshKeyId?: string;
  autoCommand?: string;
}

export interface TunnelConfig {
  id: string;
  tunnelType: "Local" | "Remote" | "Dynamic";
  localHost: string;
  localPort: number;
  remoteHost: string;
  remotePort: number;
  description?: string;
  enabled: boolean;
}

export interface ConnectionGroup {
  name: string;
  connections: string[];
}

interface ConnectionStore {
  connections: ConnectionSettings[];
  groups: ConnectionGroup[];
  loading: boolean;
  loadConnections: () => Promise<void>;
  saveConnection: (conn: ConnectionSettings) => Promise<void>;
  deleteConnection: (id: string) => Promise<void>;
  getDefaultConnection: () => ConnectionSettings;
}

const DEFAULT_ANSI_COLORS = [
  "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
  "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
];

export const useConnectionStore = create<ConnectionStore>((set, get) => ({
  connections: [],
  groups: [],
  loading: false,

  loadConnections: async () => {
    set({ loading: true });
    try {
      await invoke("sync_teamwork_now").catch(() => null);
      const connections = await invoke<ConnectionSettings[]>("get_connections");
      const groups = await invoke<ConnectionGroup[]>("get_connection_groups");
      set({ connections, groups, loading: false });
    } catch (err) {
      console.error("Failed to load connections:", err);
      set({ loading: false });
    }
  },

  saveConnection: async (conn) => {
    try {
      set((state) => {
        const nextConnections = [...state.connections];
        const existingIndex = nextConnections.findIndex((existing) => existing.id === conn.id);
        if (existingIndex >= 0) {
          nextConnections[existingIndex] = conn;
        } else {
          nextConnections.push(conn);
        }
        return { connections: nextConnections };
      });
      await invoke("save_connection", { connection: conn });
      await get().loadConnections();
    } catch (err) {
      console.error("Failed to save connection:", err);
      await get().loadConnections();
    }
  },

  deleteConnection: async (id) => {
    try {
      await invoke("delete_connection", { id });
      await get().loadConnections();
    } catch (err) {
      console.error("Failed to delete connection:", err);
    }
  },

  getDefaultConnection: () => ({
    id: crypto.randomUUID(),
    name: "",
    host: "",
    port: 22,
    username: "",
    connectionProtocol: "TcpIp",
    authMethod: "Password",
    temporaryKeyPermanent: false,
    fontFamily: "JetBrains Mono",
    fontSize: 14,
    columns: 80,
    rows: 24,
    scrollbackLines: 10000,
    foregroundColor: "#cdd6f4",
    backgroundColor: "#11111b",
    cursorColor: "#89b4fa",
    cursorStyle: "Block",
    ansiColors: [...DEFAULT_ANSI_COLORS],
    sshKeepaliveEnabled: true,
    sshKeepaliveInterval: 60,
    connectionTimeout: 15,
    retryCount: 4,
    terminalLogging: false,
    commandTimestamps: false,
    tunnels: [],
    usageCount: 0,
    connectionSource: "Local",
  }),
}));
