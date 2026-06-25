import { createMemo, For, Show } from "solid-js";
import { setBrowseState, navigateToPrefix } from "../../state/app";
import { IconHome, IconChevronR } from "../../utils/icons";

// ── path bar ───────────────────────────────────────────────────────────────────

export function PathBar(props: {
  accountName: string;
  bucket: string | null;
  prefix: string;
  onAccountSelect: () => void;
  onBucketSelect: () => void;
}) {
  const parts = createMemo(() => {
    if (!props.bucket) return [{ label: props.accountName, click: () => {} }];
    const segs = props.prefix.split("/").filter(Boolean);
    const out: { label: string; click: () => void }[] = [
      { label: props.accountName, click: props.onAccountSelect },
      { label: props.bucket, click: () => setBrowseState({ prefix: "" }) },
    ];
    let acc = "";
    for (const seg of segs) {
      acc += seg + "/";
      const p = acc;
      out.push({ label: seg, click: () => navigateToPrefix(p) });
    }
    return out;
  });

  return (
    <div class="path-bar">
      <span class="path-icon"><IconHome size={14} /></span>
      <div class="breadcrumb">
        <For each={parts()}>
          {(crumb, i) => (
            <>
              <Show when={i() > 0}><span class="breadcrumb-sep"><IconChevronR size={12} /></span></Show>
              <Show when={i() < parts().length - 1}
                    fallback={<span class="breadcrumb-current">{crumb.label}</span>}>
                <button class="breadcrumb-link" onClick={crumb.click}>{crumb.label}</button>
              </Show>
            </>
          )}
        </For>
      </div>
    </div>
  );
}
