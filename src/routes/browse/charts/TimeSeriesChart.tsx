import { onMount, onCleanup, createEffect } from "solid-js";
import uPlot from "uplot";
import "uplot/dist/uPlot.min.css";
import { formatBytes } from "../../../utils/fmt";

// uPlot area chart for cumulative storage growth over time. x is unix seconds,
// y is bytes. Redraws when data changes; resizes to its container width.
export function TimeSeriesChart(props: { data: [number[], number[]]; height?: number }) {
  let host!: HTMLDivElement;
  let plot: uPlot | undefined;

  function build(width: number) {
    plot?.destroy();
    // Canvas can't read CSS vars — resolve theme colors to strings.
    const css = getComputedStyle(document.documentElement);
    const muted = css.getPropertyValue("--muted").trim() || "#959aab";
    const border = css.getPropertyValue("--border").trim() || "rgba(255,255,255,0.07)";
    const opts: uPlot.Options = {
      width: Math.max(width, 240),
      height: props.height ?? 160,
      cursor: { y: false },
      legend: { show: false },
      scales: { x: { time: true } },
      axes: [
        { stroke: muted, grid: { stroke: border }, ticks: { stroke: border } },
        {
          stroke: muted,
          grid: { stroke: border },
          ticks: { stroke: border },
          values: (_u, splits) => splits.map((v) => formatBytes(v)),
          size: 64,
        },
      ],
      series: [
        {},
        {
          stroke: "#a45cf0",
          width: 2,
          fill: "rgba(164,92,240,0.18)",
          points: { show: false },
          value: (_u, v) => (v == null ? "" : formatBytes(v)),
        },
      ],
    };
    plot = new uPlot(opts, props.data, host);
  }

  onMount(() => {
    build(host.clientWidth);
    const ro = new ResizeObserver(() => plot?.setSize({ width: Math.max(host.clientWidth, 240), height: props.height ?? 160 }));
    ro.observe(host);
    onCleanup(() => { ro.disconnect(); plot?.destroy(); });
  });

  createEffect(() => {
    if (plot) plot.setData(props.data);
  });

  return <div class="ts-chart" ref={host} />;
}
