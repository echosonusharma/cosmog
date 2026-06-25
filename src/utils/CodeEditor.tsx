import { onMount, onCleanup, createEffect, createSignal, Show } from "solid-js";
import { confirmDialog } from "../state/confirm";
import { EditorView, keymap, lineNumbers, highlightActiveLineGutter, drawSelection, dropCursor, rectangularSelection, crosshairCursor, highlightActiveLine } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { indentOnInput, syntaxHighlighting, defaultHighlightStyle, bracketMatching, foldGutter, foldKeymap } from "@codemirror/language";
import { lintKeymap, linter, lintGutter } from "@codemirror/lint";
import { closeBrackets, autocompletion, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { searchKeymap, highlightSelectionMatches } from "@codemirror/search";
import type { Extension } from "@codemirror/state";
import type { Diagnostic } from "@codemirror/lint";

// ── app-theme CodeMirror theme ────────────────────────────────────────────────
// Reads from CSS custom properties at creation time so it follows dark/light.

function appTheme(dark: boolean): Extension {
  const s = getComputedStyle(document.documentElement);
  const v = (name: string) => s.getPropertyValue(name).trim();

  return EditorView.theme({
    "&": {
      fontSize: "12.5px",
      fontFamily: "var(--font-mono, monospace)",
      background: v("--bg") || (dark ? "#0f1117" : "#ffffff"),
      color: v("--text") || (dark ? "#e2e8f0" : "#1a202c"),
      height: "100%",
      borderRadius: "6px",
    },
    ".cm-content": { padding: "8px 0", caretColor: v("--accent") || "#8b5cf6" },
    ".cm-cursor": { borderLeftColor: v("--accent") || "#8b5cf6" },
    ".cm-scroller": { fontFamily: "inherit", overflow: "auto" },
    ".cm-gutters": {
      background: v("--panel") || (dark ? "#161b22" : "#f6f8fa"),
      color: v("--muted") || "#6b7280",
      border: "none",
      borderRight: `1px solid ${v("--border") || "#2d3748"}`,
    },
    ".cm-lineNumbers .cm-gutterElement": { padding: "0 8px" },
    ".cm-activeLineGutter": { background: v("--panel-2") || (dark ? "#1e2530" : "#edf2f7") },
    ".cm-activeLine": { background: v("--panel-2") || (dark ? "#1e2530" : "#edf2f7") },
    ".cm-selectionBackground, ::selection": { background: `${v("--accent") || "#8b5cf6"}33` },
    ".cm-focused .cm-selectionBackground": { background: `${v("--accent") || "#8b5cf6"}44` },
    ".cm-matchingBracket": { color: v("--accent") || "#8b5cf6", fontWeight: "bold" },
    ".cm-foldGutter .cm-gutterElement": { cursor: "pointer" },
    ".cm-tooltip": {
      background: v("--panel") || "#1e2530",
      border: `1px solid ${v("--border") || "#2d3748"}`,
      borderRadius: "6px",
    },
    ".cm-diagnostic": { padding: "2px 6px" },
    ".cm-diagnostic-error": { borderLeft: "3px solid var(--err, #ef4444)" },
    ".cm-diagnostic-warning": { borderLeft: "3px solid var(--warn, #f59e0b)" },
    ".cm-lintRange-error": { backgroundImage: "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' width='6' height='3'><path d='m0 3 l2 -2 l1 0 l2 2 l1 0' stroke='%23ef4444' fill='none' stroke-width='1.2'/></svg>\")" },
    ".cm-lintRange-warning": { backgroundImage: "url(\"data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' width='6' height='3'><path d='m0 3 l2 -2 l1 0 l2 2 l1 0' stroke='%23f59e0b' fill='none' stroke-width='1.2'/></svg>\")" },
  }, { dark });
}

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
  const themeComp = new Compartment();

  let destroyed = false;

  onMount(async () => {
    try {
      const dark = props.dark ?? true;
      const showGutters = props.gutters ?? false;
      const lang = await langExtension(props.ext);

      // Component may have unmounted while awaiting the language pack
      if (destroyed || !container.isConnected) return;

      const gutterExts = showGutters
        ? [lineNumbers(), lintGutter(), highlightActiveLineGutter(), foldGutter()]
        : [lintGutter()];

      const state = EditorState.create({
        doc: props.value,
        extensions: [
          themeComp.of(appTheme(dark)),
          langComp.of(lang),
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
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
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
    } catch (err) {
      console.warn("[CodeEditor] mount failed:", err);
    }
  });

  // Sync readOnly changes (guard: view may not be ready yet)
  createEffect(() => {
    view?.dispatch({ effects: roComp.reconfigure(EditorState.readOnly.of(props.readOnly ?? false)) });
  });

  // Sync theme changes
  createEffect(() => {
    const dark = props.dark ?? true;
    view?.dispatch({ effects: themeComp.reconfigure(appTheme(dark)) });
  });

  // Replace content when file changes
  createEffect(() => {
    const v = props.value;
    if (!view) return;
    if (view.state.doc.toString() !== v) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });

  onCleanup(() => { destroyed = true; view?.destroy(); view = null; });

  return <div ref={container} style="height:100%;overflow:auto;border-radius:6px" />;
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
      if (action === true) await doSave();
    }
    props.onClose();
  }

  async function handleFormat() {
    setFormatting(true);
    const formatted = await formatCode(props.ext, content());
    setContent(formatted);
    setFormatting(false);
  }

  async function doSave() {
    setSaving(true);
    try { await props.onSave(content()); }
    finally { setSaving(false); }
  }

  async function handleSave() {
    const ok = await confirmDialog({ title: "Save changes", body: `Save changes to ${props.filename}?`, confirmLabel: "Save", cancelLabel: "Cancel" });
    if (!ok) return;
    await doSave();
    props.onClose();
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
          <div style="display:flex;align-items:center;gap:6px;margin-left:auto">
            <Show when={canFormat}>
              <button class="btn-ghost" style="font-size:12px" disabled={formatting()} onClick={handleFormat}>
                {formatting() ? "Formatting…" : "Format"}
              </button>
            </Show>
            <button class="btn-secondary" style="font-size:12px;padding:5px 14px" onClick={requestClose}>Cancel</button>
            <button class="btn-primary" style="font-size:12px;padding:5px 14px" disabled={saving()} onClick={handleSave}>
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
          <span class="muted" style="font-size:11px">Ctrl+S to save · Esc to close</span>
        </div>
      </div>
    </div>
  );
}
