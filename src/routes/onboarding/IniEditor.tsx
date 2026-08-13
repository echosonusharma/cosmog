import { onMount, onCleanup, createEffect, createSignal } from "solid-js";
import { EditorView, keymap, drawSelection, highlightActiveLine } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { linter, lintGutter } from "@codemirror/lint";
import type { Diagnostic } from "@codemirror/lint";
import { editorHighlightTheme } from "../../state/editorTheme";
import { resolvedTheme } from "../../state/theme";
import { loadEditorTheme } from "../../utils/codemirrorThemes";
import { validateAwsCredentialsIni } from "../../utils/parseAwsCredentialsIni";

function awsIniLinter() {
  return linter((view): Diagnostic[] => {
    const text = view.state.doc.toString();
    if (!text.trim()) return [];

    return validateAwsCredentialsIni(text).map((err) => {
      const line = view.state.doc.line(Math.min(err.line, view.state.doc.lines));
      return {
        from: line.from,
        to: line.to,
        severity: "error" as const,
        message: err.message,
      };
    });
  });
}

async function iniLanguage() {
  const { StreamLanguage } = await import("@codemirror/language");
  const { properties } = await import("@codemirror/legacy-modes/mode/properties");
  return StreamLanguage.define(properties);
}

export function IniEditor(props: {
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
}) {
  let container!: HTMLDivElement;
  let view: EditorView | null = null;
  const langComp = new Compartment();
  const roComp = new Compartment();
  const themeComp = new Compartment();
  let destroyed = false;
  let themeGen = 0;
  const [ready, setReady] = createSignal(false);

  function loadTheme() {
    const gen = ++themeGen;
    const dark = resolvedTheme() === "dark";
    const themeId = editorHighlightTheme();
    void (async () => {
      try {
        const theme = await loadEditorTheme(themeId, dark);
        if (destroyed || !view || gen !== themeGen) return;
        view.dispatch({ effects: themeComp.reconfigure(theme) });
      } catch (err) {
        console.warn("[IniEditor] theme load failed:", err);
      }
    })();
  }

  onMount(() => {
    const state = EditorState.create({
      doc: props.value,
      extensions: [
        themeComp.of([]),
        langComp.of([]),
        roComp.of(EditorState.readOnly.of(props.disabled ?? false)),
        lintGutter(),
        awsIniLinter(),
        drawSelection(),
        highlightActiveLine(),
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) props.onChange(update.state.doc.toString());
        }),
        EditorView.lineWrapping,
      ],
    });

    view = new EditorView({ state, parent: container });
    setReady(true);

    void (async () => {
      const lang = await iniLanguage();
      if (destroyed || !view) return;
      view.dispatch({ effects: langComp.reconfigure(lang) });
    })();
    loadTheme();
  });

  createEffect(() => {
    if (!ready()) return;
    loadTheme();
  });

  createEffect(() => {
    view?.dispatch({
      effects: roComp.reconfigure(EditorState.readOnly.of(props.disabled ?? false)),
    });
  });

  createEffect(() => {
    const v = props.value;
    if (!view) return;
    if (view.state.doc.toString() !== v) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: v } });
    }
  });

  onCleanup(() => {
    destroyed = true;
    setReady(false);
    view?.destroy();
    view = null;
  });

  return <div ref={container} class="ini-editor-host" />;
}
