import type { DonutSegment } from "./Donut";

// Distinct hues for donut / legend slices. Last is the "Other" bucket.
export const PALETTE = ["#a45cf0", "#4f9dff", "#37c9a8", "#f0b429", "#ff7a5c", "#e05cc0", "#8a94a6"];

// Top N slices by value plus one folded "Other" slice covering the rest of `total`.
export function foldTop<T>(
  items: T[], n: number, total: number, label: (x: T) => string, value: (x: T) => number, otherLabel: string,
): DonutSegment[] {
  const top = items.slice(0, n);
  const rest = total - top.reduce((sum, x) => sum + value(x), 0);
  const segs = top.map((x, i) => ({ label: label(x), value: value(x), color: PALETTE[i] }));
  if (rest > 0) segs.push({ label: otherLabel, value: rest, color: PALETTE[PALETTE.length - 1] });
  return segs;
}
