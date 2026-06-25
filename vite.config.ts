import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [solid()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
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

  // 2. tauri expects a fixed port, fail if that port is not available
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
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
