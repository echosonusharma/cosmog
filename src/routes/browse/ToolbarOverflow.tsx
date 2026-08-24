import { Show, createSignal } from "solid-js";
import { IconMore, IconActivity, IconLock, IconLockOpen, IconPlus } from "../../utils/icons";

// Mobile overflow menu for browse-toolbar actions that don't warrant a visible
// button; hidden on desktop via CSS.
export function ToolbarOverflow(props: {
  indexed: boolean;
  encryptionEnabled: boolean;
  onAnalytics: () => void;
  onOpenEncryption: () => void;
  onNewFolder: () => void;
}) {
  const [open, setOpen] = createSignal(false);
  const run = (fn: () => void) => () => { setOpen(false); fn(); };

  return (
    <div class="toolbar-overflow">
      <button class="icon-btn toolbar-kebab" title="More" onClick={() => setOpen((v) => !v)}>
        <IconMore size={18} />
      </button>
      <Show when={open()}>
        <div class="toolbar-overflow-backdrop" onClick={() => setOpen(false)} />
        <div class="context-menu toolbar-overflow-menu" onClick={(e) => e.stopPropagation()}>
          <Show when={props.indexed}>
            <button class="context-item" onClick={run(props.onAnalytics)}>
              <span class="context-item-icon"><IconActivity size={14} /></span> Storage analytics
            </button>
          </Show>
          <button class="context-item" onClick={run(props.onOpenEncryption)}>
            <span class="context-item-icon">
              <Show when={props.encryptionEnabled} fallback={<IconLockOpen size={14} />}><IconLock size={14} /></Show>
            </span>
            Encryption{props.encryptionEnabled ? " (on)" : ""}
          </button>
          <div class="context-sep" />
          <button class="context-item" onClick={run(props.onNewFolder)}>
            <span class="context-item-icon"><IconPlus size={14} /></span> New folder
          </button>
        </div>
      </Show>
    </div>
  );
}
