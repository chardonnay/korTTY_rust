import type { Config } from "tailwindcss";

function withOpacity(varName: string) {
  return `rgb(var(${varName}) / <alpha-value>)`;
}

export default {
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  darkMode: "class",
  theme: {
    extend: {
      colors: {
        kortty: {
          bg: withOpacity("--kortty-bg"),
          surface: withOpacity("--kortty-surface"),
          panel: withOpacity("--kortty-panel"),
          border: withOpacity("--kortty-border"),
          text: withOpacity("--kortty-text"),
          "text-dim": withOpacity("--kortty-text-dim"),
          accent: withOpacity("--kortty-accent"),
          "accent-hover": withOpacity("--kortty-accent-hover"),
          success: withOpacity("--kortty-success"),
          warning: withOpacity("--kortty-warning"),
          error: withOpacity("--kortty-error"),
          terminal: withOpacity("--kortty-terminal"),
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
