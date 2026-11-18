import { Show } from "solid-js";
import { language, t, type TKey } from "../../i18n";
import { basename } from "../../utils/fmt";
import { IconTrash, IconX } from "../../utils/icons";
import { cancelDeleteOp, isDeleteOpActive, type DeleteOp } from "../../state/deleteOps";

// One running folder or multi-select delete in the bottom bar.
export function DeleteOpBarItem(props: { op: DeleteOp }) {
  const op = () => props.op;
  const num = (n: number) => n.toLocaleString(language());
  const name = () => op().kind === "folder"
    ? t("browse.deleting.folder", { name: basename(op().prefix.replace(/\/$/, "")) || op().prefix })
    : t("browse.deleting.selection");
  const pct = () => {
    const total = op().total;
    return total ? Math.min(100, ((op().deleted + op().failed) / total) * 100) : 0;
  };
  const indeterminate = () => op().total == null && isDeleteOpActive(op());
  const count = () => {
    const { total, found, deleted } = op();
    if (total != null) return t("browse.deleting.countOf", { done: num(deleted), total: num(total) });
    if (found != null) return t("browse.deleting.foundDeleted", { found: num(found), done: num(deleted) });
    return t("browse.deleting.count", { done: num(deleted) });
  };

  function cancel(e: MouseEvent) {
    e.stopPropagation();
    cancelDeleteOp(op().id);
  }

  // Deletes are not listed on the Transfers page, so the row does not navigate there.
  return (
    <div class="active-xfer-item del-bar-item" onClick={(e) => e.stopPropagation()}>
      <span class="active-xfer-dir delete"><IconTrash size={12} /></span>
      <div class="active-xfer-item-body">
        <div class="active-xfer-item-line-1">
          <span class="active-xfer-name truncate" title={op().prefix || undefined}>{name()}</span>
          <span class="del-bar-bucket truncate">{op().bucket}</span>
          <Show when={op().state !== "running"}>
            <span class="del-bar-state">{t(`browse.deleting.state.${op().state}` as TKey)}</span>
          </Show>
          <Show when={op().cancelable && (op().state === "starting" || op().state === "running")}>
            <button class="icon-btn del-bar-btn" title={t("common.cancel")}
                    aria-label={t("common.cancel")} onClick={cancel}><IconX size={12} /></button>
          </Show>
        </div>
        <div class="progress-track active-xfer-progress" classList={{ "del-bar-indeterminate": indeterminate() }}>
          <Show when={!indeterminate()}>
            <div class="progress-fill" classList={{ done: op().state === "done", failed: op().state === "failed" || op().state === "canceled" }}
                 style={{ width: `${op().total == null ? 100 : pct()}%` }} />
          </Show>
        </div>
        <div class="active-xfer-item-line-2">
          <span>{count()}</span>
          <Show when={op().failed > 0}>
            <span class="del-bar-sep">·</span>
            <span class="del-bar-failed">{t("browse.deleting.failedCount", { failed: num(op().failed) })}</span>
          </Show>
        </div>
      </div>
    </div>
  );
}
