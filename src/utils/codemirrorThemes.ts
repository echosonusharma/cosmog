import type { Extension } from "@codemirror/state";
import type { EditorHighlightThemeId } from "../state/editorTheme";

function asExtensions(ext: Extension | readonly Extension[]): Extension[] {
  return Array.isArray(ext) ? [...ext] : [ext];
}

async function loadGithubTheme(dark: boolean): Promise<Extension[]> {
  const mod = await import("@uiw/codemirror-theme-github");
  return asExtensions(dark ? mod.githubDark : mod.githubLight);
}

export async function loadEditorTheme(id: EditorHighlightThemeId, dark: boolean): Promise<Extension[]> {
  switch (id) {
    case "dracula": {
      const { dracula } = await import("@uiw/codemirror-theme-dracula");
      return asExtensions(dracula);
    }
    case "monokai": {
      const { monokai } = await import("@uiw/codemirror-theme-monokai");
      return asExtensions(monokai);
    }
    case "nord": {
      const { nord } = await import("@uiw/codemirror-theme-nord");
      return asExtensions(nord);
    }
    case "solarized": {
      const mod = await import("@uiw/codemirror-theme-solarized");
      return asExtensions(dark ? mod.solarizedDark : mod.solarizedLight);
    }
    case "github":
      return loadGithubTheme(dark);
    case "vscode": {
      const mod = await import("@uiw/codemirror-theme-vscode");
      return asExtensions(dark ? mod.vscodeDark : mod.vscodeLight);
    }
    case "tokyo-night": {
      const { tokyoNight } = await import("@uiw/codemirror-theme-tokyo-night");
      return asExtensions(tokyoNight);
    }
    case "atomone": {
      const { atomone } = await import("@uiw/codemirror-theme-atomone");
      return asExtensions(atomone);
    }
    default:
      return loadGithubTheme(dark);
  }
}
