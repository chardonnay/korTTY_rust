import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        kortty: {
          bg: "#1e1e2e",
          surface: "#252536",
          panel: "#2a2a3c",
          border: "#3a3a4c",
          text: "#cdd6f4",
          "text-dim": "#6c7086",
          accent: "#89b4fa",
          "accent-hover": "#74a8fc",
          success: "#a6e3a1",
          warning: "#f9e2af",
          error: "#f38ba8",
          terminal: "#11111b",
        },
      },
      fontFamily: {
        mono: [
          "JetBrains Mono",
          "Cascadia Code",
          "Fira Code",
          "Menlo",
          "monospace",
        ],
        sans: ["Inter", "system-ui", "sans-serif"],
      },
    },
  },
  plugins: [],
} satisfies Config;
