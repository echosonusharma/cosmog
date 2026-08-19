import { onMount, onCleanup, createEffect, createSignal, Show } from "solid-js";
import { EditorView, keymap, drawSelection, highlightActiveLine } from "@codemirror/view";
import { EditorState, Compartment } from "@codemirror/state";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { linter, lintGutter } from "@codemirror/lint";
import type { Diagnostic } from "@codemirror/lint";
import { editorHighlightTheme } from "../../state/editorTheme";
import { resolvedTheme } from "../../state/theme";
import { loadEditorTheme, editorShellTheme } from "../../utils/codemirrorThemes";
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
  const shellComp = new Compartment();
  const themeComp = new Compartment();
  let destroyed = false;
  let themeGen = 0;
  let langGen = 0;
  const [ready, setReady] = createSignal(false);
  const [langReady, setLangReady] = createSignal(false);
  const [themeReady, setThemeReady] = createSignal(false);
  const showEditorLoader = () => ready() && (!langReady() || !themeReady());

  function loadTheme() {
    const gen = ++themeGen;
    const dark = resolvedTheme() === "dark";
    const themeId = editorHighlightTheme();
    setThemeReady(false);
    view?.dispatch({ effects: shellComp.reconfigure(editorShellTheme(dark)) });
    void (async () => {
      try {
        const theme = await loadEditorTheme(themeId, dark);
        if (destroyed || !view || gen !== themeGen) return;
        view.dispatch({ effects: themeComp.reconfigure(theme) });
        setThemeReady(true);
      } catch (err) {
        console.warn("[IniEditor] theme load failed:", err);
        if (!destroyed && gen === themeGen) setThemeReady(true);
      }
    })();
  }

  function loadLang() {
    const gen = ++langGen;
    setLangReady(false);
    void (async () => {
      try {
        const lang = await iniLanguage();
        if (destroyed || !view || gen !== langGen) return;
        view.dispatch({ effects: langComp.reconfigure(lang) });
        setLangReady(true);
      } catch (err) {
        console.warn("[IniEditor] language load failed:", err);
        if (!destroyed && gen === langGen) setLangReady(true);
      }
    })();
  }

  onMount(() => {
    const dark = resolvedTheme() === "dark";
    const state = EditorState.create({
      doc: props.value,
      extensions: [
        shellComp.of(editorShellTheme(dark)),
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
    loadLang();
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
    setLangReady(false);
    setThemeReady(false);
    view?.destroy();
    view = null;
  });

  return (
    <div class="ini-editor-wrap rel">
      <div ref={container} class="ini-editor-host" />
      <Show when={showEditorLoader()}>
        <div class="preview-switching-overlay">
          <span class="spinner spinner-lg" />
        </div>
      </Show>
    </div>
  );
}
