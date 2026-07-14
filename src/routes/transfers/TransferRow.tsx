import { createMemo, Show } from "solid-js";
import { formatBytes, formatRelative, basename } from "../../utils/fmt";
import { IconArrowUpLine, IconArrowDownLine, IconRefresh, IconX } from "../../utils/icons";
import type { Transfer } from "../../types";
import { actionVerb, pct, fmtSecs, recordAndComputeSpeed, shortPath } from "./helpers";

// ── row (card style) ─────────────────────────────────────────────────────────

export function TransferRow(props: {
  t: Transfer;
  onCancel: () => void;
  onClear: () => void;
  onRetry: () => void;
  onLoadKey?: (accountId: string, bucket: string) => void;
}) {
  const t = () => props.t;
  // Backend surfaces missing-key failures via a stable message prefix
  // matching AppError::EncryptionIdentityMissing's Display impl. Detecting
  // the prefix lets the row show a "Load key" shortcut without a schema
  // change to persist the error code.
  const isKeyMissing = () => {
    const e = t().error;
    return !!e && (e.startsWith("encryption identity missing:") || e.includes("identity for bucket"));
  };
  const isActive = () => t().status === "active" || t().status === "pending";
  const isTerminal = () =>
    t().status === "done" || t().status === "failed" || t().status === "canceled";

  const speed = createMemo(() =>
    t().status === "active" ? recordAndComputeSpeed(t()) : 0,
  );

  const eta = () => {
    const sp = speed();
    if (!sp || !t().bytes_total) return null;
    return (t().bytes_total! - t().bytes_done) / sp;
  };

  const sizeLabel = () => {
    if (t().bytes_total) return `${formatBytes(t().bytes_done)} / ${formatBytes(t().bytes_total!)}`;
    if (t().bytes_done > 0) return formatBytes(t().bytes_done);
    return null;
  };

  const pctLabel = () => {
    const p = pct(t());
    return p > 0 ? `${Math.round(p)}%` : null;
  };

  return (
    <div class="transfer-row">
      <span class={`transfer-dir ${t().direction === "upload" ? "upload" : "download"}`}>
        {t().direction === "upload" ? <IconArrowUpLine size={16} /> : <IconArrowDownLine size={16} />}
      </span>

      <div class="transfer-info">
        <div class="transfer-line-1">
          <span class="transfer-filename" title={`${t().bucket}/${t().key}`}>{basename(t().key)}</span>
          <span class={`transfer-action-badge ${t().status.toLowerCase()}`}>
            {actionVerb(t())}
          </span>
          <div class="transfer-btns">
            <Show when={isActive()}>
              <button class="icon-btn danger" title="Cancel" onClick={props.onCancel}><IconX size={13} /></button>
            </Show>
            <Show when={t().status === "failed"}>
              <button class="icon-btn" title="Retry" onClick={props.onRetry}><IconRefresh size={13} /></button>
            </Show>
            <Show when={isTerminal()}>
              <button class="icon-btn" title="Remove" onClick={props.onClear}><IconX size={13} /></button>
            </Show>
          </div>
        </div>

        <div class="transfer-line-2" title={`${t().bucket}/${t().key}`}>
          {shortPath(t())}
        </div>

        {/* progress bar only while in-flight */}
        <Show when={isActive() && (t().bytes_total || t().bytes_done > 0)}>
          <div class="progress-track">
            <div class="progress-fill" style={`width:${pct(t())}%`} />
          </div>
          <div class="transfer-stats-row">
            <Show when={pctLabel()}>
              <span class="transfer-pct">{pctLabel()}</span>
            </Show>
            <span style="flex:1" />
            <Show when={sizeLabel()}>
              <span class="transfer-stat-size">{sizeLabel()}</span>
            </Show>
            <Show when={t().status === "active" && speed() > 0}>
              <span class="transfer-stat-speed">
                · {formatBytes(speed())}/s
                <Show when={eta()}> · {fmtSecs(eta()!)}</Show>
              </span>
            </Show>
          </div>
        </Show>

        {/* compact summary line for terminal states */}
        <Show when={isTerminal()}>
          <div class="transfer-stats-row">
            <Show when={sizeLabel()}>
              <span class="transfer-stat-size">{sizeLabel()}</span>
            </Show>
            <span style="flex:1" />
            <span class="transfer-stat-time">{formatRelative(t().updated_at)}</span>
          </div>
        </Show>

        <Show when={t().error}>
          <Show when={isKeyMissing()} fallback={<div class="transfer-error">{t().error!}</div>}>
            <div class="transfer-error" style="display:flex;align-items:center;gap:10px;flex-wrap:wrap">
              <span style="flex:1;min-width:0">Encryption key for this bucket is not on this device. Load it to decrypt this download.</span>
              <Show when={props.onLoadKey}>
                <button class="btn-primary" style="padding:4px 8px;font-size:11px" onClick={() => props.onLoadKey!(t().account_id, t().bucket)}>
                  Load key
                </button>
              </Show>
            </div>
          </Show>
        </Show>
      </div>
    </div>
  );
}
