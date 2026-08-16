import { createSignal, Show } from "solid-js";

interface ConfirmReq {
  title: string;
  body: string;
  confirmLabel?: string;
  cancelLabel?: string;
  dismissLabel?: string;
  danger?: boolean;
  cancelDanger?: boolean;
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
          <div class="modal modal-confirm" onClick={(e) => e.stopPropagation()}>
            <div class="modal-title">{p().title}</div>
            <div class="modal-sub modal-sub-body">{p().body}</div>
            <div class={`modal-confirm-actions${p().dismissLabel ? "" : " modal-confirm-actions--end"}`}>
              <Show when={p().dismissLabel}>
                <button type="button" class="btn-ghost modal-confirm-dismiss" onClick={() => finish(null)}>
                  {p().dismissLabel}
                </button>
              </Show>
              <div class="modal-confirm-btns">
                <button
                  type="button"
                  class={p().cancelDanger ? "btn-danger" : "btn-secondary"}
                  onClick={() => finish(false)}
                >
                  {p().cancelLabel ?? "Cancel"}
                </button>
                <button
                  type="button"
                  class={p().danger ? "btn-danger" : "btn-primary"}
                  onClick={() => finish(true)}
                >
                  {p().confirmLabel ?? "Confirm"}
                </button>
              </div>
            </div>
          </div>
        </div>
      )}
    </Show>
  );
}
