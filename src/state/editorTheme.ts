import { createSignal } from "solid-js";
import { getPref, setPref } from "./prefs";

export type EditorHighlightThemeId =
  | "dracula"
  | "monokai"
  | "nord"
  | "solarized"
  | "github"
  | "vscode"
  | "tokyo-night"
  | "atomone";

export const DEFAULT_EDITOR_HIGHLIGHT_THEME: EditorHighlightThemeId = "github";

export const EDITOR_HIGHLIGHT_THEMES: { id: EditorHighlightThemeId; label: string }[] = [
  { id: "github", label: "GitHub" },
  { id: "dracula", label: "Dracula" },
  { id: "monokai", label: "Monokai" },
  { id: "nord", label: "Nord" },
  { id: "solarized", label: "Solarized" },
  { id: "vscode", label: "VS Code" },
  { id: "tokyo-night", label: "Tokyo Night" },
  { id: "atomone", label: "One Dark" },
];

const PREF_KEY = "editor.highlightTheme";

function normalizeTheme(id: unknown): EditorHighlightThemeId {
  if (typeof id === "string" && EDITOR_HIGHLIGHT_THEMES.some((t) => t.id === id)) {
    return id as EditorHighlightThemeId;
  }
  return DEFAULT_EDITOR_HIGHLIGHT_THEME;
}

const [editorHighlightTheme, setEditorHighlightThemeSignal] = createSignal<EditorHighlightThemeId>(
  DEFAULT_EDITOR_HIGHLIGHT_THEME,
);

export { editorHighlightTheme };

export function initEditorTheme() {
  setEditorHighlightThemeSignal(normalizeTheme(getPref(PREF_KEY, DEFAULT_EDITOR_HIGHLIGHT_THEME)));
}

export function setEditorHighlightTheme(id: EditorHighlightThemeId) {
  setEditorHighlightThemeSignal(id);
  setPref(PREF_KEY, id);
}
