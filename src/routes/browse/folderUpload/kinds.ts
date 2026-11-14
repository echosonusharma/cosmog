import { fileKind, type Kind } from "../../../utils/icons";
import type { ExcludeReason, ExtStat } from "../../../types";

export type FileKind = Exclude<Kind, "folder"> | "other";
export interface KindStat { kind: FileKind; files: number; bytes: number }

// by_ext folded into fileKind buckets; "*" (backend rollup) becomes "other". Sorted by bytes.
export function foldKinds(byExt: ExtStat[]): KindStat[] {
  const acc = new Map<FileKind, KindStat>();
  for (const e of byExt) {
    const kind: FileKind = e.ext === "*" ? "other" : e.ext === "" ? "generic" : (fileKind(`x.${e.ext}`) as FileKind);
    const cur = acc.get(kind) ?? { kind, files: 0, bytes: 0 };
    cur.files += e.files;
    cur.bytes += e.bytes;
    acc.set(kind, cur);
  }
  return [...acc.values()].sort((a, b) => b.bytes - a.bytes);
}

export const EXCLUDE_REASONS: ExcludeReason[] = [
  "symlink", "special", "unreadable", "unreadable_dir", "non_utf8", "control_chars", "key_too_long", "file_too_large",
];

export const excludedTotal = (c: Record<ExcludeReason, number>) =>
  EXCLUDE_REASONS.reduce((s, r) => s + (c[r] ?? 0), 0);
