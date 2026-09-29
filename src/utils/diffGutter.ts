import { Chunk, type DiffConfig } from "@codemirror/merge";
import {
  RangeSetBuilder,
  StateEffect,
  StateField,
  Text,
  type ChangeDesc,
  type Extension,
  type RangeSet,
} from "@codemirror/state";
import {
  Decoration,
  EditorView,
  gutter,
  GutterMarker,
  type DecorationSet,
} from "@codemirror/view";

/** Retarget the diff baseline without rebuilding the gutter. */
export const setDiffBase = StateEffect.define<string>();

/** Skip diffing above this combined doc size to keep typing smooth. */
const MAX_DIFF_CHARS = 1_000_000;

/** Bail out to the imprecise diff instead of blocking a keystroke. No
 * scanLimit: at 500 any change span over ~4k chars goes imprecise and lights
 * up the whole doc. */
const DIFF_CONFIG: DiffConfig = { timeout: 100 };

type DiffKind = "add" | "change" | "delete";

const rank = (k: DiffKind) => (k === "change" ? 2 : k === "add" ? 1 : 0);

/** `reusable`: chunks are exact and safe to update incrementally. */
type DiffResult = {
  chunks: readonly Chunk[];
  precise: boolean;
  reusable: boolean;
};

type DiffState = DiffResult & {
  gutter: RangeSet<GutterMarker>;
  lines: DecorationSet;
};

/** Width reference for the gutter spacer: no kind, never counted as a mark. */
class DiffSpacer extends GutterMarker {
  toDOM() {
    const el = document.createElement("div");
    el.className = "cm-diff-marker";
    el.setAttribute("aria-hidden", "true");
    return el;
  }
}

class DiffMarker extends GutterMarker {
  constructor(readonly kind: DiffKind) {
    super();
  }
  eq(other: DiffMarker) {
    return other.kind === this.kind;
  }
  toDOM() {
    const el = document.createElement("div");
    el.className = `cm-diff-marker cm-diff-${this.kind}`;
    el.title =
      this.kind === "add"
        ? "Added lines"
        : this.kind === "change"
          ? "Changed lines"
          : "Deleted lines";
    return el;
  }
}

const KINDS: DiffKind[] = ["add", "change", "delete"];
const MARKERS = Object.fromEntries(
  KINDS.map((k) => [k, new DiffMarker(k)]),
) as Record<DiffKind, DiffMarker>;
const LINE_DECOS = Object.fromEntries(
  KINDS.map((k) => [k, Decoration.line({ class: `cm-diff-line-${k}` })]),
) as Record<DiffKind, Decoration>;

function toBaseText(base: string): Text {
  // Same split CodeMirror uses for string documents, so CRLF files compare
  // without phantom \r diffs (documents never contain \r).
  return Text.of(base.split(/\r\n?|\n/));
}

function computeChunks(
  base: Text,
  doc: Text,
  prev?: DiffResult,
  changes?: ChangeDesc,
): DiffResult {
  // Oversize skip is a deliberate perf tradeoff (reads clean); imprecise
  // results below are handled as broadly changed instead.
  if (base.length + doc.length > MAX_DIFF_CHARS) {
    return { chunks: [], precise: true, reusable: false };
  }
  try {
    if (base.eq(doc)) return { chunks: [], precise: true, reusable: true };
    const chunks =
      prev?.reusable && changes
        ? Chunk.updateB(prev.chunks, base, doc, changes, DIFF_CONFIG)
        : Chunk.build(base, doc, DIFF_CONFIG);
    const precise = chunks.every((c) => c.precise);
    return { chunks, precise, reusable: precise };
  } catch {
    // Same policy as imprecise: never silently show clean.
    return { chunks: [], precise: false, reusable: false };
  }
}

function toDiffState(doc: Text, base: Text, result: DiffResult): DiffState {
  const marks = computeMarks(doc, base, result);
  const gutterB = new RangeSetBuilder<GutterMarker>();
  const linesB = new RangeSetBuilder<Decoration>();
  for (const line of [...marks.keys()].sort((a, b) => a - b)) {
    const kind = marks.get(line)!;
    const pos = doc.line(line).from;
    gutterB.add(pos, pos, MARKERS[kind]);
    linesB.add(pos, pos, LINE_DECOS[kind]);
  }
  return { ...result, gutter: gutterB.finish(), lines: linesB.finish() };
}

/**
 * Changed-line gutter + line wash vs `initialBase` (the saved text).
 * Classification is per character-level change: classifying whole line-chunks
 * would smear insertions onto neighboring lines (e.g. appending a line would
 * also flag the previous one as changed).
 *
 * Contract:
 * ADD: a current line holding newly inserted complete line(s).
 * DELETE: removed text held complete line(s); marked on the adjacent
 *   current line (the line after the cut, or the last line at EOF), since a
 *   gutter exists only in the current document.
 * CHANGE: an existing current line was modified.
 */
export function diffGutter(initialBase: string): Extension {
  const baseField = StateField.define<Text>({
    create: () => toBaseText(initialBase),
    update: (value, tr) => {
      for (const e of tr.effects)
        if (e.is(setDiffBase)) return toBaseText(e.value);
      return value;
    },
  });

  const diffField = StateField.define<DiffState>({
    create: (state) => {
      const base = state.field(baseField);
      return toDiffState(state.doc, base, computeChunks(base, state.doc));
    },
    update: (prev, tr) => {
      const base = tr.state.field(baseField);
      if (base !== tr.startState.field(baseField)) {
        return toDiffState(tr.newDoc, base, computeChunks(base, tr.newDoc));
      }
      if (!tr.docChanged) return prev;
      return toDiffState(
        tr.newDoc,
        base,
        computeChunks(base, tr.newDoc, prev, tr.changes),
      );
    },
    provide: (f) => EditorView.decorations.from(f, (s) => s.lines),
  });

  return [
    baseField,
    diffField,
    gutter({
      class: "cm-diff-gutter",
      markers: (view) => view.state.field(diffField).gutter,
      // Hidden width reference: keeps the gutter from popping in (and
      // shifting lines) on the first keystroke.
      initialSpacer: () => new DiffSpacer(),
    }),
  ];
}

/** Maps chunks to 1-based current-doc line numbers. */
function computeMarks(
  doc: Text,
  base: Text,
  result: DiffResult,
): Map<number, DiffKind> {
  const marks = new Map<number, DiffKind>();
  // Imprecise means the differ gave up: flag everything rather than
  // silently showing clean.
  if (!result.precise) {
    for (let ln = 1; ln <= doc.lines; ln++) marks.set(ln, "change");
    return marks;
  }
  const add = (line: number, kind: DiffKind) => {
    const prev = marks.get(line);
    if (prev === undefined || rank(kind) > rank(prev)) marks.set(line, kind);
  };
  for (const c of result.chunks) {
    for (const r of c.changes) {
      // Changes are relative to the chunk start.
      const fromA = c.fromA + r.fromA;
      const toA = c.fromA + r.toA;
      const fromB = c.fromB + r.fromB;
      const toB = c.fromB + r.toB;
      const isIns = fromA === toA;
      const isDel = fromB === toB;
      if (isIns && !isDel) classifyInsertion(doc, fromB, toB, add);
      else if (isDel && !isIns)
        classifyDeletion(doc, base, fromA, toA, fromB, add);
      else if (!isIns && !isDel) {
        const first = doc.lineAt(fromB).number;
        const last = doc.lineAt(Math.max(fromB, toB - 1)).number;
        for (let ln = first; ln <= last; ln++) add(ln, "change");
      }
    }
  }
  return marks;
}

type MarkFn = (line: number, kind: DiffKind) => void;

/** Whole new lines read as added, same-line typing as changed. */
function classifyInsertion(doc: Text, fromB: number, toB: number, add: MarkFn) {
  const inserted = doc.sliceString(fromB, toB);
  const line0 = doc.lineAt(fromB);
  const pieces = inserted.split("\n");
  const addedLines = pieces.length - 1;
  if (addedLines === 0) {
    add(line0.number, "change");
    return;
  }
  const lastLn = line0.number + addedLines;
  const shifted = doc.line(lastLn).text.slice(pieces[addedLines].length);
  if (fromB === line0.from) {
    for (let j = 0; j < addedLines; j++) add(line0.number + j, "add");
  } else {
    if (pieces[0] !== shifted) add(line0.number, "change");
    for (let j = 1; j < addedLines; j++) add(line0.number + j, "add");
  }
  if (pieces[addedLines] === "" && shifted !== "") return;
  if (shifted === "") add(lastLn, "add");
  else add(lastLn, "change");
}

/** Clean whole-line removals read as deleted, joins as changed. */
function classifyDeletion(
  doc: Text,
  base: Text,
  fromA: number,
  toA: number,
  fromB: number,
  add: MarkFn,
) {
  const removed = base.sliceString(fromA, toA);
  const atEnd = fromB >= doc.length;
  const ln = atEnd ? doc.lines : doc.lineAt(fromB).number;
  if (!removed.includes("\n")) {
    add(ln, "change");
    return;
  }
  if (removed.endsWith("\n") && (atEnd || fromB === doc.line(ln).from)) {
    add(ln, "delete");
    return;
  }
  // Cut must end at a line end, else it joins lines (a change).
  if (
    removed.startsWith("\n") &&
    removed.length > 1 &&
    fromB === doc.lineAt(fromB).to
  ) {
    add(Math.min(ln + 1, doc.lines), "delete");
    return;
  }
  add(ln, "change");
}
