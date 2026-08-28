import type { JSX } from "solid-js";

const MIN_LEN_TO_HIGHLIGHT = 2;

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function highlightText(text: string, query: string): JSX.Element {
  const q = query.trim();
  if (q.length < MIN_LEN_TO_HIGHLIGHT) return <>{text}</> as unknown as JSX.Element;
  // Dedupe/cap terms to avoid huge regex (DoS) and redundant alternations.
  const seen = new Set<string>();
  const terms: string[] = [];
  for (const t of q.split(/\s+/)) {
    if (t.length < MIN_LEN_TO_HIGHLIGHT) continue;
    const k = t.toLowerCase();
    if (seen.has(k)) continue;
    seen.add(k);
    terms.push(t);
    if (terms.length >= 8) break;
  }
  if (!terms.length) return <>{text}</> as unknown as JSX.Element;
  // Cap total pattern length to avoid RegExp explosion on long inputs.
  let pattern = terms.map(escapeRegex).join("|");
  if (pattern.length > 200) pattern = pattern.slice(0, 200);
  // capture group keeps delimiters in split result
  let re: RegExp;
  let matchRe: RegExp;
  try {
    re = new RegExp(`(${pattern})`, "gi");
    matchRe = new RegExp(`^(?:${pattern})$`, "i");
  } catch {
    return <>{text}</> as unknown as JSX.Element;
  }
  const parts = text.split(re);
  return (
    <>
      {parts.map((part) => (matchRe.test(part) ? <mark class="search-highlight">{part}</mark> : part))}
    </>
  ) as unknown as JSX.Element;
}

export const HIGHLIGHT_MIN_LEN = MIN_LEN_TO_HIGHLIGHT;
