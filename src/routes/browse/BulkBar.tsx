export function BulkBar(props: {
  count: number;
  onClear: () => void;
  onDelete: () => void;
}) {
  return (
    <div class="bulk-bar">
      <span class="bulk-count">{props.count} selected</span>
      <button class="btn-ghost" onClick={props.onClear}>Clear</button>
      <div class="bulk-actions">
        <button class="btn-ghost" onClick={props.onDelete} style="color:var(--err)">Delete</button>
      </div>
    </div>
  );
}
