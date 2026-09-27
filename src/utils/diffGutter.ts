import { Chunk } from "@codemirror/merge";
import {
  RangeSetBuilder,
  StateEffect,
  StateField,
  Text,
  type Extension,
  type RangeSet,
} from "@codemirror/state";
import { Decoration, EditorView, gutter, GutterMarker } from "@codemirror/view";

/** Retarget the diff baseline without rebuilding the gutter. */
export const setDiffBase = StateEffect.define<string>();

/** Skip diffing above this combined doc size to keep typing smooth. */
const MAX_DIFF_CHARS = 1_000_000;

type DiffKind = "add" | "change" | "delete";

const rank = (k: DiffKind) => (k === "change" ? 2 : k === "add" ? 1 : 0);

type DiffResult = { chunks: readonly Chunk[]; precise: boolean };

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

function toBaseText(base: string): Text {
  // Same split CodeMirror uses for string documents, so CRLF files compare
  // without phantom \r diffs (documents never contain \r).
  return Text.of(base.split(/\r\n?|\n/));
}

function computeChunks(base: Text, doc: Text): DiffResult {
  // Oversize skip is a deliberate perf tradeoff (reads clean); imprecise
  // results below are handled as broadly changed instead.
  if (base.length + doc.length > MAX_DIFF_CHARS) return { chunks: [], precise: true };
  try {
    if (base.eq(doc)) return { chunks: [], precise: true };
    const chunks = Chunk.build(base, doc);
    return { chunks, precise: chunks.every((c) => c.precise) };
  } catch {
    return { chunks: [], precise: true };
  }
}

/**
 * Changed-line gutter + line wash vs `initialBase` (the saved text).
 * Classification is per character-level change: classifying whole line-chunks
 * would smear insertions onto neighboring lines (e.g. appending a line would
 * also flag the previous one as changed).
 *
 * Contract:
 * ADD — a current line holding newly inserted complete line(s).
 * DELETE — removed text held complete line(s); marked on the adjacent
 *   current line (the line after the cut, or the last line at EOF), since a
 *   gutter exists only in the current document.
 * CHANGE — an existing current line was modified.
 */
export function diffGutter(initialBase: string): Extension {
  const baseField = StateField.define<Text>({
    create: () => toBaseText(initialBase),
    update: (value, tr) => {
      for (const e of tr.effects) if (e.is(setDiffBase)) return toBaseText(e.value);
      return value;
    },
  });

  const chunksField = StateField.define<DiffResult>({
    create: (state) => computeChunks(state.field(baseField), state.doc),
    update: (result, tr) => {
      for (const e of tr.effects) {
        if (e.is(setDiffBase)) return computeChunks(toBaseText(e.value), tr.newDoc);
      }
      if (tr.docChanged) {
        return computeChunks(tr.state.field(baseField), tr.newDoc);
      }
      return result;
    },
  });

  const lineHighlight = EditorView.decorations.compute([chunksField], (state) => {
    const marks = computeMarks(state.doc, state.field(baseField), state.field(chunksField));
    if (marks.size === 0) return Decoration.none;
    const builder = new RangeSetBuilder<Decoration>();
    for (const line of [...marks.keys()].sort((a, b) => a - b)) {
      const pos = state.doc.line(line).from;
      builder.add(pos, pos, Decoration.line({ class: `cm-diff-line-${marks.get(line)}` }));
    }
    return builder.finish();
  });

  function markers(view: EditorView): RangeSet<GutterMarker> {
    const builder = new RangeSetBuilder<GutterMarker>();
    const marks = computeMarks(
      view.state.doc,
      view.state.field(baseField),
      view.state.field(chunksField),
    );
    for (const line of [...marks.keys()].sort((a, b) => a - b)) {
      const pos = view.state.doc.line(line).from;
      builder.add(pos, pos, new DiffMarker(marks.get(line)!));
    }
    return builder.finish();
  }

  return [
    baseField,
    chunksField,
    lineHighlight,
    gutter({
      class: "cm-diff-gutter",
      markers,
      // Hidden width reference: keeps the gutter from popping in (and
      // shifting lines) on the first keystroke.
      initialSpacer: () => new DiffSpacer(),
    }),
  ];
}

/** Maps chunks to 1-based current-doc line numbers. */
function computeMarks(doc: Text, base: Text, result: DiffResult): Map<number, DiffKind> {
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
      else if (isDel && !isIns) classifyDeletion(doc, base, fromA, toA, fromB, add);
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
  if (removed.startsWith("\n") && removed.length > 1) {
    add(Math.min(ln + 1, doc.lines), "delete");
    return;
  }
  add(ln, "change");
}
