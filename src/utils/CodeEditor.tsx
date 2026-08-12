import { onMount, onCleanup, createEffect, createSignal, Show } from "solid-js";
import { confirmDialog } from "../state/confirm";
import { toast } from "../state/toast";
import { EditorView, keymap, lineNumbers, highlightActiveLineGutter, drawSelection, dropCursor, rectangularSelection, crosshairCursor, highlightActiveLine } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { indentOnInput, bracketMatching, foldGutter, foldKeymap } from "@codemirror/language";
import { lintKeymap, linter, lintGutter } from "@codemirror/lint";
import { closeBrackets, autocompletion, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import type { Extension } from "@codemirror/state";
import type { Diagnostic } from "@codemirror/lint";
import { editorHighlightTheme, type EditorHighlightThemeId } from "../state/editorTheme";
import { loadEditorTheme } from "./codemirrorThemes";

// ── language loader (lazy) ────────────────────────────────────────────────────

async function langExtension(ext: string): Promise<Extension> {
  switch (ext) {
    case "json":
    case "jsonc": {
      const { json, jsonParseLinter } = await import("@codemirror/lang-json");
      return [json(), linter(jsonParseLinter())];
    }
    case "yaml":
    case "yml": {
      const { yaml } = await import("@codemirror/lang-yaml");
      const { load } = await import("js-yaml");
      const yamlLinter = linter((view): Diagnostic[] => {
        try { load(view.state.doc.toString()); return []; }
        catch (e: any) {
          const line = e.mark?.line ?? 0;
          const from = view.state.doc.line(Math.min(line + 1, view.state.doc.lines)).from;
          return [{ from, to: from, severity: "error", message: e.reason ?? String(e) }];
        }
      });
      return [yaml(), yamlLinter];
    }
    case "js":
    case "jsx":
    case "ts":
    case "tsx": {
      const { javascript } = await import("@codemirror/lang-javascript");
      return javascript({ typescript: ext === "ts" || ext === "tsx", jsx: ext === "jsx" || ext === "tsx" });
    }
    case "css": { const { css } = await import("@codemirror/lang-css"); return css(); }
    case "html":
    case "htm": { const { html } = await import("@codemirror/lang-html"); return html(); }
    case "md": { const { markdown } = await import("@codemirror/lang-markdown"); return markdown(); }
    case "xml": { const { xml } = await import("@codemirror/lang-xml"); return xml(); }
    case "py": { const { python } = await import("@codemirror/lang-python"); return python(); }
    case "sh":
    case "bash":
    case "zsh": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { shell } = await import("@codemirror/legacy-modes/mode/shell");
      return StreamLanguage.define(shell);
    }
    case "toml": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { toml } = await import("@codemirror/legacy-modes/mode/toml");
      return StreamLanguage.define(toml);
    }
    case "sql": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { standardSQL } = await import("@codemirror/legacy-modes/mode/sql");
      return StreamLanguage.define(standardSQL);
    }
    case "dockerfile": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { dockerFile } = await import("@codemirror/legacy-modes/mode/dockerfile");
      return StreamLanguage.define(dockerFile);
    }
    case "nginx": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { nginx } = await import("@codemirror/legacy-modes/mode/nginx");
      return StreamLanguage.define(nginx);
    }
    case "env":
    case "properties": {
      const { StreamLanguage } = await import("@codemirror/language");
      const { properties } = await import("@codemirror/legacy-modes/mode/properties");
      return StreamLanguage.define(properties);
    }
    default: return [];
  }
}

// ── component ────────────────────────────────────────────────────────────────

export function CodeEditor(props: {
  value: string;
  ext: string;
  readOnly?: boolean;
  dark?: boolean;
  gutters?: boolean;   // line numbers + fold gutter; default false
  onChange?: (v: string) => void;
}) {
  let container!: HTMLDivElement;
  let view: EditorView | null = null;
  const langComp = new Compartment();
  const roComp   = new Compartment();
  const editorThemeComp = new Compartment();

  let destroyed = false;
  let langGen = 0;
  let themeGen = 0;
  const [editorReady, setEditorReady] = createSignal(false);

  function loadTheme(dark: boolean, themeId: EditorHighlightThemeId) {
    const gen = ++themeGen;
    void (async () => {
      try {
        const theme = await loadEditorTheme(themeId, dark);
        if (destroyed || !view || gen !== themeGen) return;
        view.dispatch({ effects: editorThemeComp.reconfigure(theme) });
      } catch (err) {
        console.warn("[CodeEditor] theme load failed:", err);
      }
    })();
  }

  function loadLang(ext: string) {
    const gen = ++langGen;
    void (async () => {
      try {
        const lang = await langExtension(ext);
        if (destroyed || !view || gen !== langGen) return;
        view.dispatch({ effects: langComp.reconfigure(lang) });
      } catch (err) {
        console.warn("[CodeEditor] language load failed:", err);
      }
    })();
  }

  onMount(() => {
    try {
      const showGutters = props.gutters ?? false;

      const gutterExts = showGutters
        ? [lineNumbers(), lintGutter(), highlightActiveLineGutter(), foldGutter()]
        : [lintGutter()];

      const state = EditorState.create({
        doc: props.value,
        extensions: [
          editorThemeComp.of([]),
          langComp.of([]),
          roComp.of(EditorState.readOnly.of(props.readOnly ?? false)),
          ...gutterExts,
          drawSelection(),
          dropCursor(),
          rectangularSelection(),
          crosshairCursor(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          history(),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          autocompletion(),
          keymap.of([
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...searchKeymap,
            ...historyKeymap,
            ...foldKeymap,
            ...completionKeymap,
            ...lintKeymap,
            indentWithTab,
          ]),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) props.onChange?.(update.state.doc.toString());
          }),
          EditorView.lineWrapping,
        ],
      });

      view = new EditorView({ state, parent: container });
      setEditorReady(true);
      loadLang(props.ext);
    } catch (err) {
      console.warn("[CodeEditor] mount failed:", err);
    }
  });

  // Swap syntax/lint mode when the file extension changes (preview pane keeps
  // one editor mounted while switching between text files).
  createEffect(() => {
    const ext = props.ext;
    if (!view) return;
    loadLang(ext);
  });

  // Sync readOnly changes (guard: view may not be ready yet)
  createEffect(() => {
    view?.dispatch({ effects: roComp.reconfigure(EditorState.readOnly.of(props.readOnly ?? false)) });
  });

  // Sync editor theme when app light/dark or highlight theme changes
  createEffect(() => {
    if (!editorReady()) return;
    const dark = props.dark ?? true;
    const themeId = editorHighlightTheme();
    loadTheme(dark, themeId);
  });

  // Replace content when file changes
  createEffect(() => {
    const v = props.value;
    if (!view) return;
    if (view.state.doc.toString() !== v) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });

  onCleanup(() => { destroyed = true; setEditorReady(false); view?.destroy(); view = null; });

  return <div ref={container} class="code-editor-host" />;
}

// ── format helpers ────────────────────────────────────────────────────────────

async function formatCode(ext: string, code: string): Promise<string> {
  try {
    switch (ext) {
      case "json":
      case "jsonc":
        return JSON.stringify(JSON.parse(code), null, 2);
      case "yaml":
      case "yml": {
        const { load, dump } = await import("js-yaml");
        return dump(load(code) as object, { indent: 2, lineWidth: 120 });
      }
      default:
        return code;
    }
  } catch { return code; }
}

// ── editor modal ──────────────────────────────────────────────────────────────

export function EditorModal(props: {
  value: string;
  ext: string;
  filename: string;
  dark?: boolean;
  onSave: (v: string) => Promise<void>;
  onClose: () => void;
}) {
  const [content, setContent] = createSignal(props.value);
  const [saving, setSaving] = createSignal(false);
  const [formatting, setFormatting] = createSignal(false);
  const canFormat = ["json", "jsonc", "yaml", "yml"].includes(props.ext);
  const isDirty = () => content() !== props.value;

  async function requestClose() {
    if (isDirty()) {
      const action = await confirmDialog({
        title: "Unsaved changes",
        body: "Save changes before closing?",
        confirmLabel: "Save",
        cancelLabel: "Discard",
        dismissLabel: "Keep editing",
      });
      if (action === null) return;
      // Failed save keeps the modal open so the edits are not lost.
      if (action === true && !(await doSave())) return;
    }
    props.onClose();
  }

  async function handleFormat() {
    setFormatting(true);
    const formatted = await formatCode(props.ext, content());
    setContent(formatted);
    setFormatting(false);
  }

  async function doSave(): Promise<boolean> {
    setSaving(true);
    try { await props.onSave(content()); return true; }
    catch (e) { toast.err(e); return false; }
    finally { setSaving(false); }
  }

  async function handleSave() {
    const ok = await confirmDialog({ title: "Save changes", body: `Save changes to ${props.filename}?`, confirmLabel: "Save", cancelLabel: "Cancel" });
    if (!ok) return;
    if (await doSave()) props.onClose();
  }

  function onKeyDown(e: KeyboardEvent) {
    if (e.key === "Escape") requestClose();
    if ((e.metaKey || e.ctrlKey) && e.key === "s") { e.preventDefault(); handleSave(); }
  }

  onMount(() => { document.addEventListener("keydown", onKeyDown); });
  onCleanup(() => { document.removeEventListener("keydown", onKeyDown); });

  return (
    <div class="editor-modal-backdrop" onClick={requestClose}>
      <div class="editor-modal" onClick={(e) => e.stopPropagation()}>
        <div class="editor-modal-header">
          <span class="editor-modal-title">{props.filename}</span>
          <div class="editor-modal-actions">
            <Show when={canFormat}>
              <button class="btn-ghost text-xs" disabled={formatting()} onClick={handleFormat}>
                {formatting() ? "Formatting…" : "Format"}
              </button>
            </Show>
            <button class="btn-secondary text-xs editor-modal-btn" onClick={requestClose}>Cancel</button>
            <button class="btn-primary text-xs editor-modal-btn" disabled={saving()} onClick={handleSave}>
              {saving() ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
        <div class="editor-modal-body">
          <CodeEditor
            value={content()}
            ext={props.ext}
            readOnly={false}
            dark={props.dark}
            gutters={true}
            onChange={setContent}
          />
        </div>
        <div class="editor-modal-footer">
          <span class="muted text-xxs">Ctrl+S to save · Esc to close</span>
        </div>
      </div>
    </div>
  );
}
