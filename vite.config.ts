import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    rollupOptions: {
      output: {
        manualChunks(id) {
          const normalizedId = id.split(path.sep).join("/");
          if (normalizedId.includes("/src/components/dialogs/SnippetManager.tsx")) {
            return "dialog-snippet-manager";
          }
          if (!id.includes("node_modules")) {
            return undefined;
          }
          if (id.includes("@uiw/react-codemirror")) {
            return "vendor-codemirror-core";
          }
          if (id.includes("@codemirror/lang-")) {
            const match = id.match(/@codemirror\/(lang-[^/]+)/);
            return match ? `vendor-codemirror-${match[1]}` : "vendor-codemirror-langs";
          }
          if (id.includes("@codemirror/legacy-modes")) {
            return "vendor-codemirror-legacy";
          }
          if (id.includes("@lezer/")) {
            const match = id.match(/@lezer\/([^/]+)/);
            const packageName = match?.[1] ?? "core";
            if (["common", "highlight", "lr"].includes(packageName)) {
              return "vendor-codemirror-core";
            }
            return `vendor-lezer-${packageName}`;
          }
          if (
            id.includes("@codemirror") ||
            id.includes("style-mod") ||
            id.includes("w3c-keyname") ||
            id.includes("crelt") ||
            id.includes("@marijn/find-cluster-break")
          ) {
            return "vendor-codemirror-core";
          }
          if (id.includes("@xterm")) {
            return "vendor-xterm";
          }
          if (id.includes("react") || id.includes("react-dom") || id.includes("scheduler")) {
            return "vendor-react";
          }
          if (id.includes("@tauri-apps")) {
            return "vendor-tauri";
          }
          if (id.includes("lucide-react")) {
            return "vendor-icons";
          }
          if (id.includes("yaml")) {
            return "vendor-yaml";
          }
          return undefined;
        },
      },
    },
  },
}));
