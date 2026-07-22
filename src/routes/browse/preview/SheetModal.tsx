import { createSignal, createMemo, createEffect, For, Show } from "solid-js";
import ExcelJS from "exceljs";
import { previewObject, putObjectBytes } from "../../../api/objects";
import { notify } from "../../../utils/notify";
import { toast, errMsg } from "../../../state/toast";
import { confirmDialog } from "../../../state/confirm";
import { formatBytes } from "../../../utils/fmt";
import { IconEye, IconX } from "../../../utils/icons";
import type { CachedObjectMeta } from "../../../types";
import { extOf, parseCsvIntoSheet, worksheetToCsv } from "../helpers";

const SHEET_CAP = 10 * 1024 * 1024;

// Spreadsheet trigger button + full-screen modal. Holds its own loaded
// workbook + edit state; resets when `obj` changes.
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

  // Reset everything when the selected object changes
  createEffect(() => { void props.obj.key; setSheetWb(null); setSheetExpanded(false); setActiveSheet(""); setSheetDirty(false); setSheetEditMode(false); setSheetErr(null); });

  async function loadSheet() {
    if (sheetWb() || sheetLoading()) return;
    setSheetLoading(true);
    setSheetErr(null);
    try {
      const r = await previewObject(props.obj.account_id, props.obj.bucket, props.obj.key, SHEET_CAP);
      const wb = new ExcelJS.Workbook();
      const bytes = new Uint8Array(r.bytes);
      if (ext() === "csv") {
        const csvStr = new TextDecoder().decode(bytes);
        const ws = wb.addWorksheet("Sheet1");
        parseCsvIntoSheet(csvStr, ws);
      } else {
        await wb.xlsx.load(bytes.buffer as ExcelJS.Buffer);
      }
      const first = wb.worksheets[0]?.name ?? "";
      setActiveSheet(first);
      setSheetWb(wb);
    } catch (e: any) {
      setSheetErr(errMsg(e));
    } finally {
      setSheetLoading(false);
    }
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
      });
      if (action === null) return;
      if (action === true) {
        const saved = await doSaveSheet();
        if (!saved) return;
      }
    }
    setSheetExpanded(false);
    setSheetEditMode(false);
    setSheetDirty(false);
  }

  async function saveSheet() {
    const ok = await confirmDialog({ title: "Save changes", body: `Save changes to ${props.obj.basename}?`, confirmLabel: "Save", cancelLabel: "Cancel" });
    if (!ok) return;
    await doSaveSheet();
    setSheetEditMode(false);
  }

  async function discardSheet() {
    if (sheetDirty()) {
      const ok = await confirmDialog({ title: "Discard changes", body: "Discard unsaved changes?", confirmLabel: "Discard", cancelLabel: "Keep editing", danger: true });
      if (!ok) return;
    }
    setSheetDirty(false);
    setSheetEditMode(false);
  }

  const sheetRows = createMemo((): string[][] => {
    const wb = sheetWb();
    sheetRev(); // track revision so cell edits trigger recompute
    if (!wb) return [];
    const ws = wb.getWorksheet(activeSheet());
    if (!ws) return [];
    const colCount = ws.actualColumnCount || 1;
    const result: string[][] = [];
    ws.eachRow({ includeEmpty: false }, (row) => {
      const cells: string[] = [];
      for (let c = 1; c <= colCount; c++) {
        const cell = row.getCell(c);
        cells.push(cell.text ?? String(cell.value ?? ""));
      }
      result.push(cells);
    });
    return result;
  });

  function sheetCellUpdate(ri: number, ci: number, val: string) {
    const wb = sheetWb();
    if (!wb) return;
    const ws = wb.getWorksheet(activeSheet());
    if (!ws) return;
    // ri/ci are 0-indexed from render; ExcelJS is 1-indexed
    const row = ws.getRow(ri + 1);
    row.getCell(ci + 1).value = val || null;
    row.commit();
    setSheetDirty(true);
    setSheetRev((n) => n + 1);
  }

  async function doSaveSheet(): Promise<boolean> {
    const wb = sheetWb();
    if (!wb) return false;
    setSheetSaving(true);
    try {
      let bytes: number[];
      let ct: string;
      if (ext() === "csv") {
        const ws = wb.worksheets[0];
        const csvStr = worksheetToCsv(ws);
        bytes = Array.from(new TextEncoder().encode(csvStr));
        ct = "text/csv";
      } else {
        const buf = await wb.xlsx.writeBuffer();
        bytes = Array.from(new Uint8Array(buf as ArrayBuffer));
        ct = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
      }
      await putObjectBytes(props.obj.account_id, props.obj.bucket, props.obj.key, bytes, ct);
      setSheetDirty(false);
      notify(`Saved ${props.obj.basename}`, props.obj.bucket, {
        largeBody: `Saved changes to "${props.obj.key}" in "${props.obj.bucket}"`,
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

      {/* Full-screen spreadsheet modal */}
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
                <Show when={!sheetEditMode()}>
                  <button class="btn-secondary sheet-modal-btn"
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
                <span class="spinner spinner-lg" />
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
                        <For each={row as string[]}>
                          {(cell, ci) => ri() === 0
                            ? <th>{String(cell)}</th>
                            : <td contentEditable={sheetEditMode() || undefined}
                                  onBlur={(e) => {
                                    if (!sheetEditMode()) return;
                                    const v = e.currentTarget.textContent ?? "";
                                    if (v !== String(cell)) sheetCellUpdate(ri(), ci(), v);
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
