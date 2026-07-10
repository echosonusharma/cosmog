import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [solid()],
  clearScreen: false,
  define: {
    global: "globalThis",
  },
  optimizeDeps: {
    include: [
      "exceljs",
      "@codemirror/view",
      "@codemirror/state",
      "@codemirror/commands",
      "@codemirror/language",
      "@codemirror/lint",
      "@codemirror/autocomplete",
      "@codemirror/search",
      "@codemirror/lang-json",
      "@codemirror/lang-yaml",
      "@codemirror/lang-javascript",
      "@codemirror/lang-css",
      "@codemirror/lang-html",
      "@codemirror/lang-markdown",
      "@codemirror/lang-xml",
      "@codemirror/lang-python",
      "js-yaml",
      "@codemirror/legacy-modes/mode/shell",
      "@codemirror/legacy-modes/mode/toml",
      "@codemirror/legacy-modes/mode/sql",
      "@codemirror/legacy-modes/mode/dockerfile",
      "@codemirror/legacy-modes/mode/nginx",
      "@codemirror/legacy-modes/mode/properties",
    ],
  },
  build: {
    target: ["es2022", "chrome105", "safari15"],
  },

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
}));
