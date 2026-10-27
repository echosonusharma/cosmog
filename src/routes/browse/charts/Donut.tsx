import { For, Show, createMemo } from "solid-js";

export interface DonutSegment {
  label: string;
  value: number;
  color: string;
}

// SVG donut. Segments drawn as dash-offset arcs on stacked circles so there
// is no dependency and it themes with plain CSS colors.
export function Donut(props: {
  segments: DonutSegment[];
  size?: number;
  thickness?: number;
  /** Fills the hole: big value on top, small caption under it. */
  centerValue?: string;
  centerLabel?: string;
}) {
  const size = () => props.size ?? 132;
  const thickness = () => props.thickness ?? 18;
  const r = () => (size() - thickness()) / 2;
  const circ = () => 2 * Math.PI * r();
  const total = createMemo(() => props.segments.reduce((s, x) => s + x.value, 0) || 1);

  const arcs = createMemo(() => {
    let acc = 0;
    return props.segments.map((seg) => {
      const frac = seg.value / total();
      const arc = { seg, dash: frac * circ(), offset: acc * circ() };
      acc += frac;
      return arc;
    });
  });

  return (
    <svg class="donut" width={size()} height={size()} viewBox={`0 0 ${size()} ${size()}`}>
      <g transform={`rotate(-90 ${size() / 2} ${size() / 2})`}>
        <circle
          cx={size() / 2} cy={size() / 2} r={r()}
          fill="none" stroke="var(--border)" stroke-width={thickness()}
        />
        <For each={arcs()}>
          {(a) => (
            <circle
              cx={size() / 2} cy={size() / 2} r={r()}
              fill="none" stroke={a.seg.color} stroke-width={thickness()}
              stroke-dasharray={`${a.dash} ${circ() - a.dash}`}
              stroke-dashoffset={-a.offset}
            />
          )}
        </For>
      </g>
      <Show when={props.centerValue}>
        <text
          class="donut-center-val"
          x="50%"
          y="50%"
          dy={props.centerLabel ? "-0.15em" : "0.35em"}
          text-anchor="middle"
        >
          {props.centerValue}
        </text>
      </Show>
      <Show when={props.centerLabel}>
        <text class="donut-center-lbl" x="50%" y="50%" dy="1.15em" text-anchor="middle">
          {props.centerLabel}
        </text>
      </Show>
    </svg>
  );
}
