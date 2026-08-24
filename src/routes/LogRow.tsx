import { createSignal, For, Show } from "solid-js";
import { levelClass, type ParsedLine } from "../utils/parseLine";

export function LogRow(props: {
  line: ParsedLine;
  activeSource: string;
  onSourceClick: (key: string) => void;
}) {
  const [open, setOpen] = createSignal(false);
  const fieldEntries = () => Object.entries(props.line.fields);
  const hasFields = () => fieldEntries().length > 0;
  const hasJson = () => props.line.json !== null;
  const source = () => props.line.source;
  const isActive = () => props.activeSource === source().key;
  return (
    <div class="log-line">
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
          <Show when={props.line.span}><span class="log-span">{props.line.span}</span></Show>
          <Show when={hasFields()}>
            <span class="log-fields">
              <For each={fieldEntries()}>
                {([k, v]) => <span class="log-kv"><span class="log-k">{k}</span><span class="log-v">{v}</span></span>}
              </For>
            </span>
          </Show>
          <span class="log-msg">{props.line.msg}</span>
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
