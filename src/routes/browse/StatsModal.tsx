import { Show, For, createSignal, createResource, createMemo } from "solid-js";
import { bucketStats } from "../../api/search";
import { formatBytes } from "../../utils/fmt";
import { IconInfo } from "../../utils/icons";
import { Donut } from "./charts/Donut";
import { TimeSeriesChart } from "./charts/TimeSeriesChart";
import type { BucketStats } from "../../types";

// Distinct hues for the donut / file-type legend. Last is the "Other" bucket.
const PALETTE = ["#a45cf0", "#4f9dff", "#37c9a8", "#f0b429", "#ff7a5c", "#e05cc0", "#8a94a6"];

export function StatsModal(props: {
  accountId: string;
  bucket: string;
  onClose: () => void;
}) {
  const [stats] = createResource<BucketStats>(() => bucketStats(props.accountId, props.bucket));

  const maxExtBytes = createMemo(() =>
    Math.max(1, ...(stats()?.by_extension ?? []).map((e) => e.total_bytes)),
  );

  // Top 6 file types by size + a folded "Other" slice for the donut. "Other"
  // is derived from the true total so it covers every type past the top 6,
  // including any beyond the by_extension cap.
  const donutSegments = createMemo(() => {
    const s = stats();
    const top = (s?.by_extension ?? []).slice(0, 6);
    const restBytes = (s?.total_bytes ?? 0) - top.reduce((sum, e) => sum + e.total_bytes, 0);
    const segs = top.map((e, i) => ({ label: e.extension, value: e.total_bytes, color: PALETTE[i] }));
    if (restBytes > 0) segs.push({ label: "other", value: restBytes, color: PALETTE[6] });
    return segs;
  });

  // by_extension is capped by the backend; how many types aren't shown as bars.
  const hiddenTypes = () => Math.max(0, (stats()?.extension_count ?? 0) - (stats()?.by_extension.length ?? 0));

  // Cumulative storage growth: x = month start (unix secs), y = running bytes.
  const growth = createMemo<[number[], number[]]>(() => {
    const months = stats()?.by_month ?? [];
    const xs: number[] = [];
    const ys: number[] = [];
    let acc = 0;
    for (const m of months) {
      acc += m.total_bytes;
      xs.push(Date.parse(`${m.month}-01T00:00:00Z`) / 1000);
      ys.push(acc);
    }
    return [xs, ys];
  });

  const total = () => stats()?.total_bytes ?? 1;
  const [showInfo, setShowInfo] = createSignal(false);

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal stats-modal" onClick={(e) => e.stopPropagation()}>
        <div class="stats-head">
          <div class="modal-title">Storage analytics</div>
          <button
            class="icon-btn stats-info-btn"
            classList={{ active: showInfo() }}
            title="How is this built?"
            onClick={() => setShowInfo((v) => !v)}
          >
            <IconInfo size={15} />
          </button>
        </div>
        <div class="modal-sub">{props.bucket}</div>
        <Show when={showInfo()}>
          <div class="stats-note">
            Built entirely from this bucket's <strong>indexed object metadata</strong> (key, size,
            last-modified, storage class) held in the local cache. No live bucket calls are made, so
            figures reflect the last index sync. Re-index to refresh.
          </div>
        </Show>

        <Show when={stats.loading}>
          <div class="loading-row"><span class="spinner" /> Reading index…</div>
        </Show>

        <Show when={stats()}>
          {(s) => (
            <div class="stats-scroll">
              <div class="stats-summary">
                <div class="stats-figure">
                  <span class="stats-figure-val">{formatBytes(s().total_bytes)}</span>
                  <span class="stats-figure-lbl">total size</span>
                </div>
                <div class="stats-figure">
                  <span class="stats-figure-val">{s().object_count.toLocaleString()}</span>
                  <span class="stats-figure-lbl">objects</span>
                </div>
                <div class="stats-figure">
                  <span class="stats-figure-val">{s().extension_count.toLocaleString()}</span>
                  <span class="stats-figure-lbl">file types</span>
                </div>
              </div>

              <Show when={growth()[0].length > 1}>
                <div class="stats-section-title">Storage growth</div>
                <TimeSeriesChart data={growth()} />
              </Show>

              <div class="stats-cols">
                <div class="stats-col">
                  <div class="stats-section-title">Size by type</div>
                  <div class="stats-donut-wrap">
                    <Donut segments={donutSegments()} />
                    <div class="stats-legend">
                      <For each={donutSegments()}>
                        {(seg) => (
                          <div class="stats-legend-row">
                            <span class="stats-legend-dot" style={{ background: seg.color }} />
                            <span class="stats-legend-name truncate">{seg.label}</span>
                            <span class="stats-legend-pct">{Math.round((seg.value / total()) * 100)}%</span>
                          </div>
                        )}
                      </For>
                    </div>
                  </div>
                </div>

                <div class="stats-col">
                  <div class="stats-section-title">By file type</div>
                  <div class="stats-bars">
                    <For each={s().by_extension} fallback={<span class="muted">No objects indexed</span>}>
                      {(e) => (
                        <div class="stats-bar-row">
                          <span class="stats-bar-label truncate">{e.extension}</span>
                          <div class="stats-bar-track">
                            <div class="stats-bar-fill" style={{ width: `${(e.total_bytes / maxExtBytes()) * 100}%` }} />
                          </div>
                          <span class="stats-bar-val">{formatBytes(e.total_bytes)}</span>
                          <span class="stats-bar-count">{e.object_count.toLocaleString()}</span>
                        </div>
                      )}
                    </For>
                    <Show when={hiddenTypes() > 0}>
                      <div class="stats-bar-more">+{hiddenTypes().toLocaleString()} more type{hiddenTypes() === 1 ? "" : "s"}</div>
                    </Show>
                  </div>
                </div>
              </div>

              <Show when={s().largest.length > 0}>
                <div class="stats-section-title">Largest objects</div>
                <div class="stats-largest">
                  <For each={s().largest}>
                    {(o) => (
                      <div class="stats-largest-row" title={o.key}>
                        <span class="stats-largest-name truncate">{o.basename || o.key}</span>
                        <span class="stats-bar-val">{formatBytes(o.size)}</span>
                      </div>
                    )}
                  </For>
                </div>
              </Show>

              <Show when={s().by_storage_class.length > 1}>
                <div class="stats-section-title">By storage class</div>
                <div class="stats-class-list">
                  <For each={s().by_storage_class}>
                    {(c) => (
                      <div class="stats-class-row">
                        <span class="stats-class-name truncate">{c.storage_class || "STANDARD"}</span>
                        <span class="stats-class-val">{formatBytes(c.total_bytes)}</span>
                        <span class="stats-bar-count">{c.object_count.toLocaleString()}</span>
                      </div>
                    )}
                  </For>
                </div>
              </Show>
            </div>
          )}
        </Show>

        <div class="btn-row stats-actions">
          <button class="btn-secondary" onClick={props.onClose}>Close</button>
        </div>
      </div>
    </div>
  );
}
