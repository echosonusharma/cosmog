import { createSignal, For, Show } from "solid-js";
import { errMsg } from "../utils/errors";

export { errMsg } from "../utils/errors";

type Kind = "info" | "ok" | "err" | "warn";
interface Toast { id: number; kind: Kind; msg: string; }

const [toasts, setToasts] = createSignal<Toast[]>([]);
let next = 1;

function push(kind: Kind, msg: string, ttl = 3500) {
  const id = next++;
  setToasts((cur) => [...cur, { id, kind, msg }]);
  setTimeout(() => setToasts((cur) => cur.filter((t) => t.id !== id)), ttl);
}

export const toast = {
  info: (m: string) => push("info", m),
  ok:   (m: string) => push("ok", m),
  err:  (e: unknown) => push("err", errMsg(e)),
  warn: (m: string) => push("warn", m),
};

export function ToastStack() {
  return (
    <Show when={toasts().length > 0}>
      <div class="toast-stack">
        <For each={toasts()}>
          {(t) => (
            <div class={`toast ${t.kind}`}>
              <span class="toast-msg">{t.msg}</span>
              <button
                class="toast-x"
                onClick={() => setToasts((cur) => cur.filter((x) => x.id !== t.id))}
              >
                ✕
              </button>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}
