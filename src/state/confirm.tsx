import { createSignal, Show } from "solid-js";

interface ConfirmReq {
  title: string;
  body: string;
  confirmLabel?: string;
  cancelLabel?: string;
  dismissLabel?: string;
  danger?: boolean;
  resolve: (result: boolean | null) => void;
}

const [pending, setPending] = createSignal<ConfirmReq | null>(null);

/** Returns true=confirm, false=cancel, null=dismissed (backdrop/X) */
export function confirmDialog(opts: Omit<ConfirmReq, "resolve">): Promise<boolean | null> {
  return new Promise((resolve) => {
    setPending({ ...opts, resolve });
  });
}

export function ConfirmHost() {
  function finish(result: boolean | null) {
    const p = pending();
    setPending(null);
    p?.resolve(result);
  }
  return (
    <Show when={pending()}>
      {(p) => (
        <div class="modal-backdrop" onClick={() => finish(null)}>
          <div class="modal" style="max-width:380px" onClick={(e) => e.stopPropagation()}>
            <div class="modal-title">{p().title}</div>
            <div class="modal-sub" style="white-space:pre-wrap;word-break:normal;overflow-wrap:anywhere;line-height:1.5">{p().body}</div>
            <div class="btn-row mt-3">
              <Show when={p().dismissLabel}>
                <button class="btn-ghost" style="flex:1" onClick={() => finish(null)}>
                  {p().dismissLabel}
                </button>
              </Show>
              <button class="btn-secondary" style="flex:1" onClick={() => finish(false)}>
                {p().cancelLabel ?? "Cancel"}
              </button>
              <button
                class={p().danger ? "btn-danger" : "btn-primary"}
                style="flex:1"
                onClick={() => finish(true)}
              >
                {p().confirmLabel ?? "Confirm"}
              </button>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
