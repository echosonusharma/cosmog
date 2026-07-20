import { Index, Show } from "solid-js";
import { activeTransfers, setCurrentView } from "../../state/app";
import { formatBytes, basename } from "../../utils/fmt";
import { pct } from "../transfers/helpers";
import { IconArrowUpLine, IconArrowDownLine } from "../../utils/icons";

/**
 * Sticky, always-on progress strip shown on every view whenever at least one
 * transfer is in flight. Each row surfaces the filename, direction, and bytes
 * done vs total. Tap the strip to jump to the full Transfers view.
 */
export function ActiveTransfersBar() {
  const list = () => activeTransfers();

  return (
    <Show when={list().length > 0}>
      <div class="active-xfer-bar" role="button" tabIndex={0} onClick={() => setCurrentView("transfers")}>
        <div class="active-xfer-summary">
          <span class="active-xfer-count">
            {list().length} transfer{list().length === 1 ? "" : "s"}
          </span>
        </div>

        {/* Index, not For: the poll publishes freshly-created objects every
            second, so For would tear down and recreate every row per tick. */}
        <div class="active-xfer-list">
          <Index each={list()}>
            {(t) => {
              const sizeLabel = () => {
                const row = t();
                return row.bytes_total
                  ? `${formatBytes(row.bytes_done)} / ${formatBytes(row.bytes_total)}`
                  : row.bytes_done > 0
                  ? formatBytes(row.bytes_done)
                  : "";
              };
              return (
                <div class="active-xfer-item">
                  <span class={`active-xfer-dir ${t().direction}`}>
                    {t().direction === "upload" ? <IconArrowUpLine size={12} /> : <IconArrowDownLine size={12} />}
                  </span>
                  <div class="active-xfer-item-body">
                    <div class="active-xfer-item-line-1">
                      <span class="active-xfer-name truncate">{basename(t().key)}</span>
                    </div>
                    <div class="progress-track active-xfer-progress">
                      <div class="progress-fill" style={{ width: `${pct(t())}%` }} />
                    </div>
                    <div class="active-xfer-item-line-2">
                      <span class="active-xfer-item-size">{sizeLabel()}</span>
                    </div>
                  </div>
                </div>
              );
            }}
          </Index>
        </div>
      </div>
    </Show>
  );
}
