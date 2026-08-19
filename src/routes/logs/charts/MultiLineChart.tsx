import { For, Show, onMount, onCleanup, createEffect, createSignal } from "solid-js";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { formatChartCount } from "../../../utils/requestLogMeta";

export interface ChartSeries {
  label: string;
  /** Full name for hover tooltip when label is truncated. */
  fullLabel?: string;
  values: number[];
  color: string;
  fill?: boolean;
  width?: number;
  dash?: number[];
}

type HoverTip = {
  left: number;
  top: number;
  date: string;
  rows: { label: string; value: string; color: string; n: number }[];
};

// Multi-series uPlot chart with hover tooltip. x is shared unix-second timestamps;
// each series is a y-array aligned to x.
export function MultiLineChart(props: {
  x: number[];
  series: ChartSeries[];
  height?: number;
  formatY?: (v: number) => string;
}) {
  let host!: HTMLDivElement;
  let wrap!: HTMLDivElement;
  let plot: uPlot | undefined;
  let lastBuildKey = "";
  const [hoverTip, setHoverTip] = createSignal<HoverTip | null>(null);

  function fmtY(v: number) {
    return props.formatY?.(v) ?? formatChartCount(v);
  }

  function themeColors() {
    const css = getComputedStyle(document.documentElement);
    return {
      muted: css.getPropertyValue("--muted").trim() || "#959aab",
      border: css.getPropertyValue("--border").trim() || "rgba(255,255,255, 0.07)",
      text: css.getPropertyValue("--text-soft").trim() || "#c8cdd8",
    };
  }

  function layoutHoverTip(u: uPlot) {
    const idx = u.cursor.idx;
    if (idx == null) {
      setHoverTip(null);
      return;
    }

    const ts = u.data[0][idx];
    const date = uPlot.fmtDate("{MM}/{DD}/{YYYY}")(new Date(ts * 1000));
    const rows = props.series
      .map((s) => {
        const n = s.values[idx] ?? 0;
        return {
          label: s.fullLabel ?? s.label,
          value: fmtY(n),
          color: s.color,
          n,
        };
      })
      .sort((a, b) => b.n - a.n);

    const plotW = u.bbox.width;
    const plotH = u.bbox.height;
    const tipW = Math.min(260, Math.max(160, plotW - 24));
    const tipH = 28 + rows.length * 20;
    let left = (u.cursor.left ?? 0) + 14;
    let top = (u.cursor.top ?? 0) + 14;
    if (left + tipW > plotW - 8) left = Math.max(8, (u.cursor.left ?? 0) - tipW - 8);
    if (top + tipH > plotH - 8) top = Math.max(8, (u.cursor.top ?? 0) - tipH - 8);

    setHoverTip({ left, top, date, rows });
  }

  function buildKey(width: number) {
    const height = props.height ?? 200;
    const seriesKey = props.series
      .map((s) => `${s.label}|${s.color}|${s.fill ? 1 : 0}|${s.width ?? ""}|${s.dash?.join(",") ?? ""}`)
      .join(";");
    return `${width}|${height}|${seriesKey}`;
  }

  function build(width: number) {
    plot?.destroy();
    plot = undefined;
    setHoverTip(null);
    const { muted, border, text } = themeColors();
    const height = props.height ?? 200;
    const seriesOpts: uPlot.Series[] = [{}, ...props.series.map((s) => ({
      label: s.label,
      stroke: s.color,
      width: s.width ?? 2,
      dash: s.dash,
      fill: s.fill ? hexAlpha(s.color, 0.2) : undefined,
      points: { show: false },
      value: (_u: uPlot, v: number | null) => (v == null ? "" : fmtY(v)),
    }))];

    const data: uPlot.AlignedData = [
      props.x,
      ...props.series.map((s) => s.values),
    ];

    const opts: uPlot.Options = {
      width: Math.max(width, 240),
      height,
      cursor: {
        show: true,
        drag: { x: false, y: false },
        points: {
          show: true,
          size: 6,
          width: 1,
          stroke: (_u, si) => props.series[si - 1]?.color ?? text,
          fill: (_u, si) => props.series[si - 1]?.color ?? text,
        },
      },
      legend: { show: false },
      scales: { x: { time: true } },
      axes: [
        { stroke: muted, grid: { stroke: border }, ticks: { stroke: border } },
        {
          stroke: muted,
          grid: { stroke: border },
          ticks: { stroke: border },
          values: (_u, splits) => splits.map((v) => fmtY(v)),
          size: width < 420 ? 36 : 48,
        },
      ],
      series: seriesOpts,
      hooks: {
        setCursor: [(u) => layoutHoverTip(u)],
        setLegend: [() => setHoverTip(null)],
      },
    };
    plot = new uPlot(opts, data, host);
    lastBuildKey = buildKey(width);
  }

  onMount(() => {
    const ro = new ResizeObserver(() => {
      if (!host) return;
      const w = Math.max(host.clientWidth, 240);
      if (plot) plot.setSize({ width: w, height: props.height ?? 200 });
      else build(w);
    });
    ro.observe(host);
    const hideTip = () => setHoverTip(null);
    wrap?.addEventListener("mouseleave", hideTip);
    wrap?.addEventListener("touchend", hideTip, { passive: true });
    onCleanup(() => {
      ro.disconnect();
      wrap?.removeEventListener("mouseleave", hideTip);
      wrap?.removeEventListener("touchend", hideTip);
      plot?.destroy();
    });
  });

  createEffect(() => {
    props.x;
    props.series;
    props.height;
    props.formatY;
    if (!host) return;
    const width = Math.max(host.clientWidth, 240);
    const key = buildKey(width);
    if (plot && key === lastBuildKey) {
      plot.setData([
        props.x,
        ...props.series.map((s) => s.values),
      ]);
      plot.setSize({ width, height: props.height ?? 200 });
      return;
    }
    build(width);
  });

  return (
    <div class="api-dash-chart-wrap api-dash-chart-wrap-hover" ref={wrap}>
      <div class="ts-chart api-dash-chart" ref={host} />
      <Show when={hoverTip()}>
        {(tip) => (
          <div
            class="api-dash-chart-tooltip"
            style={{
              left: `${tip().left}px`,
              top: `${tip().top}px`,
            }}
          >
            <div class="api-dash-chart-tooltip-date">{tip().date}</div>
            <For each={tip().rows}>
              {(row) => (
                <div class="api-dash-chart-tooltip-row">
                  <span class="api-dash-chart-tooltip-dot" style={{ background: row.color }} />
                  <span class="api-dash-chart-tooltip-label">{row.label}</span>
                  <span class="api-dash-chart-tooltip-val">{row.value}</span>
                </div>
              )}
            </For>
          </div>
        )}
      </Show>
    </div>
  );
}

function hexAlpha(hex: string, alpha: number): string {
  const h = hex.replace("#", "");
  if (h.length !== 6) return hex;
  const r = parseInt(h.slice(0, 2), 16);
  const g = parseInt(h.slice(2, 4), 16);
  const b = parseInt(h.slice(4, 6), 16);
  return `rgba(${r},${g},${b},${alpha})`;
}
