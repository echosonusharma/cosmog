import { createMemo, createResource, type JSX } from "solid-js";
import { HighlightStyle } from "@codemirror/language";
import { jsonLanguage } from "@codemirror/lang-json";
import { highlightCode } from "@lezer/highlight";
import { editorHighlightTheme, type EditorHighlightThemeId } from "../state/editorTheme";
import { resolvedTheme } from "../state/theme";
import { loadEditorTagStyles } from "./codemirrorThemes";

// Above this size, skip parsing and render plain text.
const MAX_LEN = 200_000;

const styleCache = new Map<string, Promise<HighlightStyle>>();

// One HighlightStyle per theme+mode; its generated CSS is injected once.
function loadStyle(id: EditorHighlightThemeId, dark: boolean): Promise<HighlightStyle> {
  const cacheKey = `${id}:${dark}`;
  let hit = styleCache.get(cacheKey);
  if (!hit) {
    hit = loadEditorTagStyles(id, dark).then((specs) => {
      const style = HighlightStyle.define(specs);
      if (style.module) {
        const el = document.createElement("style");
        el.textContent = style.module.getRules();
        document.head.appendChild(el);
      }
      return style;
    });
    hit.catch(() => styleCache.delete(cacheKey));
    styleCache.set(cacheKey, hit);
  }
  return hit;
}

/** Static JSON highlighted with the editor theme. Plain text until the theme loads. */
export function JsonCode(props: { text: string; class?: string }) {
  const [style] = createResource(
    () => [editorHighlightTheme(), resolvedTheme() === "dark"] as const,
    ([id, dark]) => loadStyle(id, dark),
  );
  const content = createMemo<JSX.Element>(() => {
    const hl = style.error ? undefined : style.latest;
    const text = props.text;
    if (!hl || text.length > MAX_LEN) return text;
    const out: JSX.Element[] = [];
    highlightCode(
      text,
      jsonLanguage.parser.parse(text),
      hl,
      (code, classes) => out.push(classes ? <span class={classes}>{code}</span> : code),
      () => out.push("\n"),
    );
    return out;
  });
  return <pre class={props.class}>{content()}</pre>;
}
