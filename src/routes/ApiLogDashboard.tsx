import { For, Show, createEffect, createMemo, createResource, createSignal, onCleanup, untrack, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { getRequestLogStats } from "../api/requestLogs";
import { getSettings } from "../api/settings";
import type { RequestLogAccountStat, RequestLogStats } from "../types";
import { currentView } from "../state/app";
import { isMobile } from "../utils/breakpoint";
import { Donut } from "./browse/charts/Donut";
import { MultiLineChart } from "./logs/charts/MultiLineChart";
import { IconInfo } from "../utils/icons";
import {
  CHART_PALETTE,
  accountKey,
  accountLabelFromMap,
  accountLabelMap,
  chartLegendLabel,
  opColor,
  opLabel,
} from "../utils/requestLogMeta";


const TOP_ACCOUNTS = 7;

type InfoKey =
  | "total"
  | "accounts"
  | "operation-types"
  | "latency"
  | "errors"
  | "daily-volume"
  | "daily-account"
  | "by-operation"
  | "by-account"
  | "top-buckets";

const DASH_INFO: Record<InfoKey, { title: string; body: JSX.Element }> = {
  total: {
    title: "Total requests",
    body: (
      <>
        Count of every S3-compatible API call Cosmog made in this period: listings, downloads,
        uploads, deletes, head requests, etc. Cloud providers often charge per request class — this
        total shows overall activity volume.
      </>
    ),
  },
  accounts: {
    title: "Accounts",
    body: "Storage accounts that had at least one logged API call in this period.",
  },
  "operation-types": {
    title: "Operation types",
    body: (
      <>
        Distinct API operation kinds (e.g. List Objects, Get Object, Put Object). Providers map
        these to billing classes — such as Backblaze B2 Class A vs Class B — with different
        per-request prices.
      </>
    ),
  },
  latency: {
    title: "Average latency",
    body: "Mean round-trip time for all logged requests. Not a billing metric, but high latency can mean distance, large payloads, or throttling.",
  },
  errors: {
    title: "Errors",
    body: "Requests that failed (auth, not found, timeout, etc.). Some providers still bill failed calls depending on the operation.",
  },
  "daily-volume": {
    title: "Daily request volume",
    body: (
      <>
        Number of API calls per calendar day. Hover the chart for exact counts. Purple is all
        requests; when failures exist, a red errors series appears too. Spikes often come from
        bucket syncs, bulk transfers, or background indexing.
      </>
    ),
  },
  "daily-account": {
    title: "Requests by account",
    body: (
      <>
        Daily request count per storage account. Use the account chips to select which accounts
        to plot. Hover the chart to see exact request counts per day; unselected accounts are
        grouped into a dashed Other line.
      </>
    ),
  },
  "by-operation": {
    title: "By operation",
    body: (
      <>
        How many calls of each API type. Bar labels match S3 operations Cosmog logs (List Objects,
        Preview Object, Upload, …). This is the closest view to provider request-class billing. The
        right column shows average latency per operation.
      </>
    ),
  },
  "by-account": {
    title: "By account",
    body: "Total API calls per selected account in this period. Follows the account filter above the daily chart. Error counts appear in the meta column when present; otherwise average latency is shown.",
  },
  "top-buckets": {
    title: "Top buckets",
    body: "Buckets with the highest number of logged API calls. Heavy listing or sync against one bucket often dominates request counts even when total storage is spread across many buckets.",
  },
};

type PopoverLayout = {
  top: number;
  left: number;
  placement: "above" | "below";
  arrowX: number;
};

const POPOVER_PAD = 12;
const POPOVER_GAP = 10;

function layoutPopover(btn: HTMLButtonElement, pop: HTMLElement): PopoverLayout {
  const br = btn.getBoundingClientRect();
  const popW = pop.offsetWidth;
  const popH = pop.offsetHeight;
  const anchorX = br.left + br.width / 2;

  let placement: "above" | "below" = "above";
  let top = br.top - POPOVER_GAP - popH;
  if (top < POPOVER_PAD) {
    placement = "below";
    top = br.bottom + POPOVER_GAP;
  }
  if (top + popH > window.innerHeight - POPOVER_PAD) {
    top = Math.max(POPOVER_PAD, window.innerHeight - POPOVER_PAD - popH);
  }

  let left = anchorX - popW / 2;
  left = Math.max(POPOVER_PAD, Math.min(left, window.innerWidth - POPOVER_PAD - popW));

  const arrowX = Math.max(16, Math.min(popW - 16, anchorX - left));

  return { top, left, placement, arrowX };
}

function DashInfoBtn(props: { infoKey: InfoKey; size?: number; class?: string; label?: string }) {
  const [open, setOpen] = createSignal(false);
  const [layout, setLayout] = createSignal<PopoverLayout | null>(null);
  let btn!: HTMLButtonElement;
  let pop!: HTMLDivElement;
  const info = DASH_INFO[props.infoKey];

  function updateLayout() {
    if (!btn || !pop) return;
    setLayout(layoutPopover(btn, pop));
  }

  function toggle(e: MouseEvent) {
    e.stopPropagation();
    if (open()) {
      setOpen(false);
      setLayout(null);
      return;
    }
    setLayout(null);
    setOpen(true);
  }

  createEffect(() => {
    if (!open()) return;
    requestAnimationFrame(updateLayout);
    const onResize = () => updateLayout();
    window.addEventListener("resize", onResize);
    onCleanup(() => window.removeEventListener("resize", onResize));
  });

  createEffect(() => {
    if (!open()) return;
    const close = (e: MouseEvent) => {
      const t = e.target as Node;
      if (btn.contains(t)) return;
      if (pop?.contains(t)) return;
      setOpen(false);
      setLayout(null);
    };
    const onScroll = () => {
      setOpen(false);
      setLayout(null);
    };
    const t = setTimeout(() => document.addEventListener("click", close), 0);
    const scrollEl = btn?.closest(".api-dash-scroll");
    scrollEl?.addEventListener("scroll", onScroll, { passive: true });
    onCleanup(() => {
      clearTimeout(t);
      document.removeEventListener("click", close);
      scrollEl?.removeEventListener("scroll", onScroll);
    });
  });

  return (
    <div class="api-dash-info-wrap">
      <button
        ref={btn}
        type="button"
        class={`icon-btn api-dash-info-btn${props.class ? ` ${props.class}` : ""}`}
        classList={{ active: open() }}
        title="What does this mean?"
        aria-expanded={open()}
        aria-label={props.label ?? info.title}
        onClick={toggle}
      >
        <IconInfo size={props.size ?? 14} />
      </button>
      <Show when={open()}>
        <Portal mount={document.body}>
          <div
            ref={pop}
            id={`api-dash-popover-${props.infoKey}`}
            class="api-dash-popover"
            classList={{ "is-ready": !!layout() }}
            role="tooltip"
            data-placement={layout()?.placement ?? "above"}
            style={{
              top: layout() ? `${layout()!.top}px` : "-9999px",
              left: layout() ? `${layout()!.left}px` : "0",
              "--arrow-x": layout() ? `${layout()!.arrowX}px` : "50%",
            }}
            onClick={(e) => e.stopPropagation()}
          >
            <div class="api-dash-popover-body">{info.body}</div>
          </div>
        </Portal>
      </Show>
    </div>
  );
}

function DashSectionHead(section: { title: string; sub?: string; infoKey: InfoKey }) {
  return (
    <div class="api-dash-card-head">
      <div class="api-dash-card-head-text">
        <h3 class="api-dash-card-title">{section.title}</h3>
        <Show when={section.sub}>
          <span class="api-dash-card-sub">{section.sub}</span>
        </Show>
      </div>
      <DashInfoBtn infoKey={section.infoKey} label={`Explain ${section.title}`} />
    </div>
  );
}

function DashStat(stat: {
  val: string;
  lbl: string;
  infoKey: InfoKey;
  class?: string;
}) {
  return (
    <div class={`api-dash-stat${stat.class ? ` ${stat.class}` : ""}`}>
      <div class="api-dash-stat-top">
        <span class="api-dash-stat-val">{stat.val}</span>
        <DashInfoBtn infoKey={stat.infoKey} size={13} class="api-dash-stat-info-btn" />
      </div>
      <span class="api-dash-stat-lbl">{stat.lbl}</span>
    </div>
  );
}

function formatMs(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  return `${(ms / 1000).toFixed(1)}s`;
}

function dayAxis(stats: RequestLogStats): number[] {
  const startDay = Math.floor(stats.since_ts / 86400) * 86400;
  return Array.from({ length: stats.period_days }, (_, i) => startDay + i * 86400);
}

function countsForDays(x: number[], dayMap: Map<number, number>): number[] {
  return x.map((day) => dayMap.get(day) ?? 0);
}

function accountColorAt(acc: RequestLogAccountStat, all: RequestLogAccountStat[]): string {
  const key = accountKey(acc.account_id, acc.account_name);
  const idx = all.findIndex(
    (a) => accountKey(a.account_id, a.account_name) === key,
  );
  return CHART_PALETTE[(idx >= 0 ? idx : 0) % CHART_PALETTE.length];
}

function defaultAccountKeys(accounts: RequestLogAccountStat[]): Set<string> {
  return new Set(
    accounts.slice(0, TOP_ACCOUNTS).map((a) => accountKey(a.account_id, a.account_name)),
  );
}

export function ApiLogDashboard(props: { active: boolean }) {
  const [settings] = createResource(getSettings);
  const [stats, setStats] = createSignal<RequestLogStats | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [refreshing, setRefreshing] = createSignal(false);
  const [fetchError, setFetchError] = createSignal(false);
  const [selectedAccountKeys, setSelectedAccountKeys] = createSignal<Set<string> | null>(null);

  let loadGen = 0;
  const periodDays = () => stats()?.period_days ?? settings()?.request_log_ttl_days ?? 30;

  async function load() {
    const isRefresh = untrack(() => stats() !== null);
    const gen = ++loadGen;
    if (isRefresh) setRefreshing(true);
    else setLoading(true);
    setFetchError(false);
    try {
      const data = await getRequestLogStats();
      if (gen !== loadGen) return;
      setStats(data);
    } catch {
      if (gen !== loadGen) return;
      setFetchError(true);
      if (!isRefresh) setStats(null);
    } finally {
      if (gen === loadGen) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  }

  createEffect(() => {
    if (!props.active || currentView() !== "dashboard") return;
    // Refetch when retention changes so the header + charts match settings.
    void settings()?.request_log_ttl_days;
    untrack(() => load());
  });

  const accountLabels = createMemo(() => accountLabelMap(stats()?.by_account ?? [], 24));

  createEffect(() => {
    const s = stats();
    if (!s || s.by_account.length === 0) {
      setSelectedAccountKeys(null);
      return;
    }
    const allKeys = s.by_account.map((a) => accountKey(a.account_id, a.account_name));
    const keySet = new Set(allKeys);
    const current = selectedAccountKeys();
    if (!current) {
      setSelectedAccountKeys(defaultAccountKeys(s.by_account));
      return;
    }
    const pruned = new Set([...current].filter((k) => keySet.has(k)));
    if (pruned.size === 0) {
      setSelectedAccountKeys(defaultAccountKeys(s.by_account));
    } else if (pruned.size !== current.size) {
      setSelectedAccountKeys(pruned);
    }
  });

  const activeAccountKeys = createMemo(() => {
    const s = stats();
    if (!s || s.by_account.length === 0) return new Set<string>();
    const sel = selectedAccountKeys();
    if (sel && sel.size > 0) return sel;
    return defaultAccountKeys(s.by_account);
  });

  const selectedAccounts = createMemo(() => {
    const s = stats();
    if (!s) return [];
    const keys = activeAccountKeys();
    return s.by_account.filter((a) => keys.has(accountKey(a.account_id, a.account_name)));
  });

  const accountSelectionLabel = createMemo(() => {
    const total = stats()?.by_account.length ?? 0;
    const n = selectedAccounts().length;
    if (total === 0) return "";
    if (n === total) return `All ${total} accounts`;
    return `${n} of ${total} selected`;
  });

  function toggleAccountSelection(key: string) {
    setSelectedAccountKeys((prev) => {
      const next = new Set(prev ?? activeAccountKeys());
      if (next.has(key)) {
        if (next.size <= 1) return next;
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

  function selectTopAccounts() {
    const s = stats();
    if (!s) return;
    setSelectedAccountKeys(defaultAccountKeys(s.by_account));
  }

  function selectAllAccounts() {
    const s = stats();
    if (!s) return;
    setSelectedAccountKeys(
      new Set(s.by_account.map((a) => accountKey(a.account_id, a.account_name))),
    );
  }

  const dailyTotal = createMemo(() => {
    const s = stats();
    if (!s) return { x: [], series: [] };
    const x = dayAxis(s);
    const totalMap = new Map(s.by_day.map((d) => [d.day, d.count]));
    const errMap = new Map(s.by_day.map((d) => [d.day, d.error_count]));
    const series: {
      label: string;
      values: number[];
      color: string;
      fill?: boolean;
      width?: number;
    }[] = [
      {
        label: "Requests",
        values: countsForDays(x, totalMap),
        color: "#a45cf0",
        fill: true,
      },
    ];
    if (s.by_day.some((d) => d.error_count > 0)) {
      series.push({
        label: "Errors",
        values: countsForDays(x, errMap),
        color: "#ef4444",
        fill: true,
        width: 1.5,
      });
    }
    return { x, series };
  });

  const dailyByAccount = createMemo(() => {
    const s = stats();
    if (!s || s.by_account.length === 0) return { x: [], series: [], totalAccounts: 0 };
    const x = dayAxis(s);
    const labels = accountLabels();
    const keys = activeAccountKeys();
    const selected = s.by_account.filter((a) =>
      keys.has(accountKey(a.account_id, a.account_name)),
    );

    const series: {
      label: string;
      fullLabel?: string;
      values: number[];
      color: string;
      fill?: boolean;
      width?: number;
      dash?: number[];
    }[] = selected.map((acc) => {
      const key = accountKey(acc.account_id, acc.account_name);
      const dayMap = new Map<number, number>();
      for (const row of s.by_day_by_account) {
        if (accountKey(row.account_id, row.account_name) === key) {
          dayMap.set(row.day, row.count);
        }
      }
      const full = accountLabelFromMap(labels, acc.account_id, acc.account_name);
      return {
        label: chartLegendLabel(full),
        fullLabel: full,
        values: countsForDays(x, dayMap),
        color: accountColorAt(acc, s.by_account),
        fill: false,
      };
    });

    const restAccounts = s.by_account.length - selected.length;
    if (restAccounts > 0) {
      const otherByDay = new Map<number, number>();
      for (const row of s.by_day_by_account) {
        const key = accountKey(row.account_id, row.account_name);
        if (!keys.has(key)) {
          otherByDay.set(row.day, (otherByDay.get(row.day) ?? 0) + row.count);
        }
      }
      const full = `Other (${restAccounts} accounts)`;
      series.push({
        label: chartLegendLabel(full),
        fullLabel: full,
        values: countsForDays(x, otherByDay),
        color: CHART_PALETTE[6],
        fill: false,
        width: 1.5,
        dash: [4, 4],
      });
    }

    return { x, series, totalAccounts: s.by_account.length };
  });

  const accountChartSub = createMemo(() => {
    const label = accountSelectionLabel();
    return label ? `${label} — daily trend` : "Daily trend by account";
  });

  const byAccountSub = createMemo(() => {
    const label = accountSelectionLabel();
    return label ? `${label} — usage breakdown` : "Usage per storage account";
  });

  const opDonut = createMemo(() => {
    const s = stats();
    if (!s) return [];
    const top = s.by_operation.slice(0, 7);
    const rest = s.by_operation.slice(7).reduce((n, o) => n + o.count, 0);
    const segs = top.map((o, i) => ({
      label: opLabel(o.operation),
      value: o.count,
      color: opColor(o.operation, i),
    }));
    if (rest > 0) segs.push({ label: "Other", value: rest, color: CHART_PALETTE[6] });
    return segs;
  });

  const accountDonut = createMemo(() => {
    const s = stats();
    if (!s) return [];
    const labels = accountLabels();
    const keys = activeAccountKeys();
    const selected = selectedAccounts();
    const segs = selected.map((a) => ({
      label: accountLabelFromMap(labels, a.account_id, a.account_name),
      value: a.count,
      color: accountColorAt(a, s.by_account),
    }));
    const rest = s.by_account
      .filter((a) => !keys.has(accountKey(a.account_id, a.account_name)))
      .reduce((n, a) => n + a.count, 0);
    if (rest > 0) segs.push({ label: "Other", value: rest, color: CHART_PALETTE[6] });
    return segs;
  });

  const maxOpCount = createMemo(() =>
    Math.max(1, ...(stats()?.by_operation ?? []).slice(0, 12).map((o) => o.count)),
  );

  const maxAccountCount = createMemo(() =>
    Math.max(1, ...selectedAccounts().map((a) => a.count)),
  );

  const maxBucketCount = createMemo(() =>
    Math.max(1, ...(stats()?.top_buckets ?? []).map((b) => b.count)),
  );

  const errorRate = createMemo(() => {
    const s = stats();
    if (!s || s.total === 0) return "0%";
    return `${((s.error_count / s.total) * 100).toFixed(1)}%`;
  });

  const chartHeight = () => (isMobile() ? 168 : 220);
  const chartHeightSm = () => (isMobile() ? 152 : 200);
  const donutSize = () => (isMobile() ? 96 : 120);
  const donutThickness = () => (isMobile() ? 14 : 18);

  return (
    <div class="api-dash-layout">
      <div class="api-dash-toolbar panel-header">
        <div class="api-dash-toolbar-text">
          <span class="api-dash-period">
            Last {periodDays()} days
          </span>
          <p class="api-dash-period-desc">
            From locally stored API request logs. Updates when you open Dashboard or press Refresh.
          </p>
        </div>
        <button
          type="button"
          class="btn-secondary api-dash-refresh"
          classList={{ "is-busy": refreshing() }}
          disabled={loading() && !stats()}
          aria-busy={refreshing()}
          onClick={() => load()}
        >
          <Show when={refreshing()}>
            <span class="spinner api-dash-refresh-spinner" aria-hidden="true" />
          </Show>
          Refresh
        </button>
      </div>

      <div class="api-dash-main">
        <Show when={loading() && !stats()}>
          <div class="bcfg-loading api-dash-loading" aria-busy="true" aria-live="polite">
            <span class="spinner spinner-lg" />
            <span>Loading analytics…</span>
          </div>
        </Show>

        <Show when={fetchError() && !stats() && !loading()}>
          <div class="api-dash-empty">
            <p class="logs-fetch-err">Failed to load API analytics.</p>
            <button type="button" class="btn-secondary" onClick={() => load()}>Retry</button>
          </div>
        </Show>

        <Show when={stats()}>
          {(s) => (
            <div class="api-dash-main-body">
              <div class="api-dash-scroll">
                <Show
                  when={s().total > 0}
                  fallback={
                    <div class="api-dash-empty">
                      <p class="logs-empty-text">No API requests in the last {s().period_days} days.</p>
                      <p class="api-dash-empty-hint">S3 calls appear here as you browse buckets and transfer files.</p>
                    </div>
                  }
                >
                  <div class="api-dash-hero">
                    <DashStat
                      class="api-dash-stat-primary"
                      val={s().total.toLocaleString()}
                      lbl="total requests"
                      infoKey="total"
                    />
                    <DashStat
                      val={s().by_account.length.toLocaleString()}
                      lbl="accounts"
                      infoKey="accounts"
                    />
                    <DashStat
                      val={s().by_operation.length.toLocaleString()}
                      lbl="operation types"
                      infoKey="operation-types"
                    />
                    <DashStat
                      val={formatMs(s().avg_duration_ms)}
                      lbl="avg latency"
                      infoKey="latency"
                    />
                    <DashStat
                      class="api-dash-stat-err"
                      val={errorRate()}
                      lbl={`${s().error_count.toLocaleString()} errors`}
                      infoKey="errors"
                    />
                  </div>

                  <Show when={dailyTotal().x.length > 0}>
                    <div class="api-dash-card api-dash-card-wide">
                      <DashSectionHead
                        title="Daily request volume"
                        sub="Total calls + errors per day"
                        infoKey="daily-volume"
                      />
                      <MultiLineChart
                        x={dailyTotal().x}
                        series={dailyTotal().series}
                        height={chartHeight()}
                      />
                    </div>
                  </Show>

                  <Show when={dailyByAccount().series.length > 0}>
                    <div class="api-dash-card api-dash-card-wide">
                      <DashSectionHead
                        title="Requests by account"
                        sub={accountChartSub()}
                        infoKey="daily-account"
                      />
                      <Show when={s().by_account.length > 1}>
                        <div class="api-dash-account-filters">
                          <div class="api-dash-account-filter-bar">
                            <span class="api-dash-account-filter-label">Show</span>
                            <button
                              type="button"
                              class="btn-ghost api-dash-account-filter-btn"
                              onClick={selectTopAccounts}
                            >
                              Top {TOP_ACCOUNTS}
                            </button>
                            <button
                              type="button"
                              class="btn-ghost api-dash-account-filter-btn"
                              onClick={selectAllAccounts}
                            >
                              All
                            </button>
                          </div>
                          <div class="api-dash-account-chips">
                            <For each={s().by_account}>
                              {(acc) => {
                                const key = accountKey(acc.account_id, acc.account_name);
                                const full = accountLabelFromMap(
                                  accountLabels(),
                                  acc.account_id,
                                  acc.account_name,
                                );
                                return (
                                  <button
                                    type="button"
                                    class="api-dash-account-chip"
                                    classList={{ active: activeAccountKeys().has(key) }}
                                    style={{ "--chip-color": accountColorAt(acc, s().by_account) }}
                                    onClick={() => toggleAccountSelection(key)}
                                    title={full}
                                  >
                                    <span class="api-dash-account-chip-dot" />
                                    <span class="api-dash-account-chip-label">
                                      {full}
                                    </span>
                                  </button>
                                );
                              }}
                            </For>
                          </div>
                        </div>
                      </Show>
                      <MultiLineChart
                        x={dailyByAccount().x}
                        series={dailyByAccount().series}
                        height={chartHeightSm()}
                      />
                    </div>
                  </Show>

                  <div class="api-dash-stats">
                    <div class="api-dash-grid">
                      <div class="api-dash-card">
                      <DashSectionHead
                        title="By operation"
                        sub="Request class breakdown"
                        infoKey="by-operation"
                      />
                      <div class="api-dash-donut-row">
                        <Donut segments={opDonut()} size={donutSize()} thickness={donutThickness()} />
                      </div>
                      <div class="api-dash-bars">
                        <For each={s().by_operation.slice(0, 12)}>
                          {(op, i) => (
                            <div class="api-dash-bar-row">
                              <span class="api-dash-bar-label truncate">{opLabel(op.operation)}</span>
                              <div class="api-dash-bar-track">
                                <div
                                  class="api-dash-bar-fill"
                                  style={{
                                    width: `${(op.count / maxOpCount()) * 100}%`,
                                    background: opColor(op.operation, i()),
                                  }}
                                />
                              </div>
                              <span class="api-dash-bar-val">{op.count.toLocaleString()}</span>
                              <span class="api-dash-bar-meta">{formatMs(op.avg_duration_ms)}</span>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>

                    <div class="api-dash-card">
                      <DashSectionHead
                        title="By account"
                        sub={byAccountSub()}
                        infoKey="by-account"
                      />
                      <Show when={accountDonut().length > 0}>
                        <div class="api-dash-donut-row">
                          <Donut segments={accountDonut()} size={donutSize()} thickness={donutThickness()} />
                        </div>
                      </Show>
                      <div class="api-dash-bars">
                        <For each={selectedAccounts()}>
                          {(acc) => (
                            <div class="api-dash-bar-row">
                              <span
                                class="api-dash-bar-label truncate"
                                title={accountLabelFromMap(accountLabels(), acc.account_id, acc.account_name)}
                              >
                                {accountLabelFromMap(accountLabels(), acc.account_id, acc.account_name)}
                              </span>
                              <div class="api-dash-bar-track">
                                <div
                                  class="api-dash-bar-fill"
                                  style={{
                                    width: `${(acc.count / maxAccountCount()) * 100}%`,
                                    background: accountColorAt(acc, s().by_account),
                                  }}
                                />
                              </div>
                              <span class="api-dash-bar-val">{acc.count.toLocaleString()}</span>
                              <span class="api-dash-bar-meta">
                                {acc.error_count > 0 ? `${acc.error_count} err` : formatMs(acc.avg_duration_ms)}
                              </span>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>
                    </div>

                    <Show when={s().top_buckets.length > 0}>
                      <div class="api-dash-card api-dash-card-wide">
                        <DashSectionHead
                          title="Top buckets"
                          sub="Most API activity by bucket"
                          infoKey="top-buckets"
                        />
                        <div class="api-dash-bars api-dash-bars-buckets">
                          <For each={s().top_buckets}>
                            {(b, i) => (
                              <div class="api-dash-bar-row api-dash-bar-row-bucket">
                                <span class="api-dash-bar-label truncate" title={b.bucket}>{b.bucket}</span>
                                <div class="api-dash-bar-track">
                                  <div
                                    class="api-dash-bar-fill"
                                    style={{
                                      width: `${(b.count / maxBucketCount()) * 100}%`,
                                      background: CHART_PALETTE[i() % CHART_PALETTE.length],
                                    }}
                                  />
                                </div>
                                <span class="api-dash-bar-val">{b.count.toLocaleString()}</span>
                              </div>
                            )}
                          </For>
                        </div>
                      </div>
                    </Show>
                  </div>
                </Show>
              </div>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}
