import { createSignal, For, Show } from "solid-js";
import { levelClass, type ParsedLine } from "../utils/parseLine";
import { highlightText } from "../utils/highlight";

export function LogRow(props: {
  line: ParsedLine;
  activeSource: string;
  onSourceClick: (key: string) => void;
  searchQuery?: string;
  focused?: boolean;
  onHover?: () => void;
}) {
  const [open, setOpen] = createSignal(false);
  const fieldEntries = () => Object.entries(props.line.fields);
  const hasFields = () => fieldEntries().length > 0;
  const hasJson = () => props.line.json !== null;
  const source = () => props.line.source;
  const isActive = () => props.activeSource === source().key;
  return (
    <div class="log-line" classList={{ "kb-active": !!props.focused }} onMouseMove={props.onHover}>
      <span class="log-ts">{props.line.ts}</span>
      <span class={`log-level ${levelClass(props.line.level)}`}>{props.line.level}</span>
      <button
        class={`log-source${isActive() ? " active" : ""}`}
        style={{ "--src-color": source().color }}
        title={isActive() ? "Clear filter" : `Filter to ${source().label}`}
        onClick={() => props.onSourceClick(source().key)}
      >
        {source().label}
      </button>
      <div class="log-main">
        <div class="log-headline">
          <Show when={props.line.span}><span class="log-span">{highlightText(props.line.span!, props.searchQuery ?? "")}</span></Show>
          <Show when={hasFields()}>
            <span class="log-fields">
              <For each={fieldEntries()}>
                {([k, v]) => (
                  <span class="log-kv">
                    <span class="log-k">{highlightText(k, props.searchQuery ?? "")}</span>
                    <span class="log-v">{highlightText(v, props.searchQuery ?? "")}</span>
                  </span>
                )}
              </For>
            </span>
          </Show>
          <span class="log-msg">{highlightText(props.line.msg, props.searchQuery ?? "")}</span>
          <Show when={hasJson()}>
            <button class="log-json-toggle" onClick={() => setOpen(!open())}>{open() ? "−" : "+"} json</button>
          </Show>
        </div>
        <Show when={hasJson() && open()}>
          <pre class="log-json">{JSON.stringify(props.line.json, null, 2)}</pre>
        </Show>
      </div>
    </div>
  );
}
