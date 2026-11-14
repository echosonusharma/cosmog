import { Index, Show } from "solid-js";
import { activeTransfers, setCurrentView } from "../../state/app";
import { tp } from "../../i18n";
import { formatBytes, basename } from "../../utils/fmt";
import { pct } from "../transfers/helpers";
import { IconArrowUpLine, IconArrowDownLine, IconFolder } from "../../utils/icons";
import { movingFolderJobs } from "../../state/folderJobs";
import { jobName, jobPct } from "../transfers/folderJobHelpers";

export function ActiveTransfersBar() {
  const list = () => activeTransfers();
  // A folder job counts as one item, however many files it holds.
  const jobs = () => movingFolderJobs();

  return (
    <Show when={list().length + jobs().length > 0}>
      <div class="active-xfer-bar" role="button" tabIndex={0} onClick={() => setCurrentView("transfers")}>
        <div class="active-xfer-summary">
          <span class="active-xfer-count">
            {tp("shell.xfer.count", list().length + jobs().length)}
          </span>
        </div>

        {/* Index, not For: the poll publishes freshly-created objects every
            second, so For would tear down and recreate every row per tick. */}
        <div class="active-xfer-list">
          <Index each={jobs()}>
            {(j) => (
              <div class="active-xfer-item">
                <span class="active-xfer-dir upload"><IconFolder size={12} /></span>
                <div class="active-xfer-item-body">
                  <div class="active-xfer-item-line-1">
                    <span class="active-xfer-name truncate">{jobName(j())}</span>
                  </div>
                  <div class="progress-track active-xfer-progress">
                    <div class="progress-fill" style={{ width: `${jobPct(j())}%` }} />
                  </div>
                  <div class="active-xfer-item-line-2">
                    <span class="active-xfer-item-size">{`${formatBytes(j().bytes_done)} / ${formatBytes(j().total_bytes)}`}</span>
                  </div>
                </div>
              </div>
            )}
          </Index>
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
