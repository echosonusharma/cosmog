import { t, tp } from "../../i18n";

export function BulkBar(props: {
  count: number;
  onClear: () => void;
  onDelete: () => void;
}) {
  return (
    <div class="bulk-bar">
      <span class="bulk-count">{tp("browse.bulk.selected", props.count)}</span>
      <button class="btn-ghost" onClick={props.onClear}>{t("browse.bulk.clear")}</button>
      <div class="bulk-actions">
        <button class="btn-ghost btn-danger-text" onClick={props.onDelete}>{t("common.delete")}</button>
      </div>
    </div>
  );
}
