import { createSignal, createMemo, createEffect, onCleanup, For, Show } from "solid-js";
import ExcelJS from "exceljs";
import { previewObject, putObjectBytes } from "../../../api/objects";
import { notify } from "../../../utils/notify";
import { toast, errMsg } from "../../../state/toast";
import { confirmDialog } from "../../../state/confirm";
import { formatBytes } from "../../../utils/fmt";
import { IconEye, IconX } from "../../../utils/icons";
import type { CachedObjectMeta } from "../../../types";
import { extOf, parseCsvIntoSheet, worksheetToCsv, detectCsvFormat, type CsvFormat } from "../helpers";
import Spinner from "../../../utils/Spinner";
import { useBackHandler } from "../../../utils/androidBack";

const SHEET_CAP = 10 * 1024 * 1024;
// ExcelJS drops VBA on write, so macro workbooks are view-only.
const READ_ONLY_EXTS = new Set(["xlsm"]);

// Keeps the BOM so detectCsvFormat can restore it on save.
const decodeCsv = (bytes: Uint8Array) => new TextDecoder("utf-8", { ignoreBOM: true }).decode(bytes);

async function buildWorkbook(bytes: Uint8Array, ext: string): Promise<ExcelJS.Workbook> {
  const wb = new ExcelJS.Workbook();
  if (ext === "csv") {
    parseCsvIntoSheet(decodeCsv(bytes), wb.addWorksheet("Sheet1"));
  } else {
    // Copy so ExcelJS never holds the pristine buffer used for Discard.
    await wb.xlsx.load(bytes.slice().buffer as ExcelJS.Buffer);
  }
  return wb;
}

export function SheetPreview(props: { obj: CachedObjectMeta }) {
  const ext = () => extOf(props.obj.basename);
  const sheetTooBig = () => props.obj.size > SHEET_CAP;
  const [sheetExpanded, setSheetExpanded] = createSignal(false);
  const [activeSheet, setActiveSheet] = createSignal<string>("");
  const [sheetDirty, setSheetDirty] = createSignal(false);
  const [sheetSaving, setSheetSaving] = createSignal(false);
  const [sheetEditMode, setSheetEditMode] = createSignal(false);
  const [sheetWb, setSheetWb] = createSignal<ExcelJS.Workbook | null>(null);
  const [sheetRev, setSheetRev] = createSignal(0);
  const [sheetLoading, setSheetLoading] = createSignal(false);
  const [sheetErr, setSheetErr] = createSignal<string | null>(null);
  const readOnly = () => READ_ONLY_EXTS.has(ext());

  let loadGen = 0;
  // Discard rebuilds the workbook from these bytes.
  let origBytes: Uint8Array | null = null;
  let loadedKey: string | null = null;
  let csvFmt: CsvFormat | undefined;

  createEffect(() => {
    void props.obj.key;
    loadGen++;
    origBytes = null;
    loadedKey = null;
    setSheetWb(null); setSheetExpanded(false); setActiveSheet(""); setSheetDirty(false);
    setSheetEditMode(false); setSheetErr(null); setSheetLoading(false);
  });
  onCleanup(() => { loadGen++; });

  async function loadSheet() {
    if (sheetWb() || sheetLoading()) return;
    const gen = ++loadGen;
    const key = props.obj.key;
    const x = ext();
    setSheetLoading(true);
    setSheetErr(null);
    try {
      const r = await previewObject(props.obj.account_id, props.obj.bucket, key, SHEET_CAP);
      if (gen !== loadGen) return;
      if (r.truncated) throw new Error(`File too large to open here (max ${formatBytes(SHEET_CAP)})`);
      const bytes = new Uint8Array(r.bytes);
      const wb = await buildWorkbook(bytes, x);
      if (gen !== loadGen) return;
      origBytes = bytes;
      loadedKey = key;
      csvFmt = x === "csv" ? detectCsvFormat(decodeCsv(bytes)) : undefined;
      setActiveSheet(wb.worksheets[0]?.name ?? "");
      setSheetWb(wb);
    } catch (e: any) {
      if (gen === loadGen) setSheetErr(errMsg(e));
    } finally {
      if (gen === loadGen) setSheetLoading(false);
    }
  }

  async function revertWorkbook() {
    if (!origBytes) return;
    const gen = loadGen;
    try {
      const wb = await buildWorkbook(origBytes, ext());
      if (gen !== loadGen) return;
      const names = wb.worksheets.map((w) => w.name);
      if (!names.includes(activeSheet())) setActiveSheet(names[0] ?? "");
      setSheetWb(wb);
      setSheetRev((n) => n + 1);
    } catch (e) { toast.err(e); }
  }

  function openSheet() { setSheetExpanded(true); loadSheet(); }

  async function closeSheet() {
    if (sheetDirty()) {
      const action = await confirmDialog({
        title: "Unsaved changes",
        body: "Save changes to the spreadsheet?",
        confirmLabel: "Save",
        cancelLabel: "Discard",
        dismissLabel: "Keep editing",
        cancelDanger: true,
      });
      if (action === null) return;
      if (action === true) {
        const saved = await doSaveSheet();
        if (!saved) return;
      } else {
        await revertWorkbook();
      }
    }
    setSheetExpanded(false);
    setSheetEditMode(false);
    setSheetDirty(false);
  }

  async function saveSheet() {
    const ok = await confirmDialog({ title: "Save changes", body: `Save changes to ${props.obj.basename}?`, confirmLabel: "Save", cancelLabel: "Cancel" });
    if (!ok) return;
    if (await doSaveSheet()) setSheetEditMode(false);
  }

  async function discardSheet() {
    if (sheetDirty()) {
      const ok = await confirmDialog({ title: "Discard changes", body: "Discard unsaved changes?", confirmLabel: "Discard", cancelLabel: "Keep editing", danger: true });
      if (!ok) return;
      await revertWorkbook();
    }
    setSheetDirty(false);
    setSheetEditMode(false);
  }

  useBackHandler(() => sheetExpanded(), () => { void closeSheet(); return true; });

  // rowNum is the real 1-based ExcelJS row (blank rows are skipped in display).
  const sheetRows = createMemo((): { rowNum: number; cells: string[] }[] => {
    const wb = sheetWb();
    sheetRev(); // track revision so cell edits trigger recompute
    if (!wb) return [];
    const ws = wb.getWorksheet(activeSheet());
    if (!ws) return [];
    // columnCount is the max column index; actualColumnCount only counts non-empty columns.
    const colCount = ws.columnCount || 1;
    const result: { rowNum: number; cells: string[] }[] = [];
    ws.eachRow({ includeEmpty: false }, (row, rowNum) => {
      const cells: string[] = [];
      for (let c = 1; c <= colCount; c++) {
        const cell = row.getCell(c);
        cells.push(cell.text ?? String(cell.value ?? ""));
      }
      result.push({ rowNum, cells });
    });
    return result;
  });

  function sheetCellUpdate(rowNum: number, ci: number, val: string) {
    const wb = sheetWb();
    if (!wb) return;
    const ws = wb.getWorksheet(activeSheet());
    if (!ws) return;
    // ci is 0-indexed from render; ExcelJS is 1-indexed
    const row = ws.getRow(rowNum);
    row.getCell(ci + 1).value = val || null;
    row.commit();
    setSheetDirty(true);
    setSheetRev((n) => n + 1);
  }

  async function doSaveSheet(): Promise<boolean> {
    const wb = sheetWb();
    // Snapshot the target: a selection change mid-save must not redirect the write.
    const obj = props.obj;
    const isCsv = ext() === "csv";
    if (!wb || readOnly() || loadedKey !== obj.key) return false;
    setSheetSaving(true);
    try {
      let bytes: number[];
      let ct: string;
      if (isCsv) {
        const ws = wb.worksheets[0];
        const csvStr = worksheetToCsv(ws, csvFmt);
        bytes = Array.from(new TextEncoder().encode(csvStr));
        ct = "text/csv";
      } else {
        const buf = await wb.xlsx.writeBuffer();
        bytes = Array.from(new Uint8Array(buf as ArrayBuffer));
        ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
      }
      await putObjectBytes(obj.account_id, obj.bucket, obj.key, bytes, ct);
      if (loadedKey === obj.key) { origBytes = new Uint8Array(bytes); setSheetDirty(false); }
      notify(`Saved ${obj.basename}`, obj.bucket, {
        largeBody: `Saved changes to "${obj.key}" in "${obj.bucket}"`,
      });
      return true;
    } catch (e) { toast.err(e); return false; }
    finally { setSheetSaving(false); }
  }

  return (
    <>
      <div class="preview-img-area sheet-preview-col">
        <Show when={sheetTooBig()}>
          <span class="muted sheet-preview-hint">File too large to preview ({formatBytes(props.obj.size)} · max {formatBytes(SHEET_CAP)})</span>
        </Show>
        <Show when={!sheetTooBig()}>
          <button class="btn-secondary preview-btn-inline" onClick={openSheet}>
            <IconEye size={15} /> View spreadsheet
          </button>
        </Show>
      </div>

      <Show when={sheetExpanded()}>
        <div class="sheet-modal-overlay" onClick={closeSheet}>
          <div class="sheet-modal-inner" onClick={(e) => e.stopPropagation()}>
            <div class="sheet-modal-header">
              <span class="sheet-modal-title">{props.obj.basename}</span>
              <Show when={(sheetWb()?.worksheets?.length ?? 0) > 1}>
                <div class="sheet-tabs">
                  <For each={sheetWb()!.worksheets.map((ws) => ws.name)}>
                    {(s) => (
                      <button class={`sheet-tab ${activeSheet() === s ? "active" : ""}`}
                              onClick={() => setActiveSheet(s)}>{s}</button>
                    )}
                  </For>
                </div>
              </Show>
              <div class="sheet-modal-actions">
                <Show when={readOnly()}>
                  <span class="muted sheet-readonly-note" title="Saving would strip the workbook's macros">
                    Read-only (macros)
                  </span>
                </Show>
                <Show when={!sheetEditMode() && !readOnly()}>
                  <button class="btn-secondary sheet-modal-btn"
                          disabled={!sheetWb()}
                          onClick={() => setSheetEditMode(true)}>
                    Edit
                  </button>
                </Show>
                <Show when={sheetEditMode()}>
                  <button class="btn-ghost sheet-modal-btn"
                          onClick={discardSheet}>
                    Discard
                  </button>
                  <button class="btn-primary sheet-modal-btn"
                          disabled={sheetSaving()} onClick={saveSheet}>
                    {sheetSaving() ? "Saving…" : "Save"}
                  </button>
                </Show>
                <button class="icon-btn" onClick={closeSheet}><IconX size={18} /></button>
              </div>
            </div>
            <Show when={sheetLoading()}>
              <div class="preview-loader sheet-modal-loading">
                <Spinner size={50} />
                <span>Loading spreadsheet…</span>
              </div>
            </Show>
            <Show when={sheetErr()}>
              <div class="status-msg err sheet-modal-err">{sheetErr()}</div>
            </Show>
            <Show when={sheetWb()}>
              <div class="sheet-table-wrap sheet-table-full">
                <table class="sheet-table">
                  <For each={sheetRows()}>
                    {(row, ri) => (
                      <tr>
                        <For each={row.cells}>
                          {(cell, ci) => ri() === 0
                            ? <th>{String(cell)}</th>
                            : <td contentEditable={sheetEditMode() || undefined}
                                  onBlur={(e) => {
                                    if (!sheetEditMode()) return;
                                    const v = e.currentTarget.textContent ?? "";
                                    if (v !== String(cell)) sheetCellUpdate(row.rowNum, ci(), v);
                                  }}
                              >{String(cell)}</td>
                          }
                        </For>
                      </tr>
                    )}
                  </For>
                </table>
              </div>
            </Show>
          </div>
        </div>
      </Show>
    </>
  );
}
