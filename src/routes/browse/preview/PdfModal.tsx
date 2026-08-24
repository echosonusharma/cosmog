import { createSignal, createEffect, onCleanup, Show } from "solid-js";
import { previewObject } from "../../../api/objects";
import { errMsg } from "../../../state/toast";
import { formatBytes } from "../../../utils/fmt";
import { IconEye, IconX } from "../../../utils/icons";
import { useBackHandler } from "../../../utils/androidBack";
import type { CachedObjectMeta } from "../../../types";
import type { PDFDocumentProxy, PDFDocumentLoadingTask, RenderTask } from "pdfjs-dist";

// Lazy-loaded (heavy). Legacy build runs on the older WebKit Tauri ships
// (WebKitGTK on Linux, macOS WKWebView) — no native PDF renderer there.
let pdfjsPromise: Promise<typeof import("pdfjs-dist/legacy/build/pdf.mjs")> | null = null;
async function loadPdfjs() {
  if (!pdfjsPromise) {
    pdfjsPromise = (async () => {
      const pdfjs = await import("pdfjs-dist/legacy/build/pdf.mjs");
      const worker = await import("pdfjs-dist/legacy/build/pdf.worker.min.mjs?url");
      pdfjs.GlobalWorkerOptions.workerSrc = worker.default;
      return pdfjs;
    })();
  }
  return pdfjsPromise;
}

// Cap IPC bytes: preview_object returns the whole object as number[], so a huge
// PDF would balloon memory — past this the user downloads instead.
const PDF_CAP = 25 * 1024 * 1024;
// Display size scales with zoom but the backing buffer is capped at PIX_CAP so
// a deep zoom can't allocate a gigapixel canvas (gets softer past the cap).
const MIN_ZOOM = 1;
const MAX_ZOOM = 4;
const PIX_CAP = 6144;

// Inline "View PDF" trigger plus a full-screen modal rendering one page at a
// time. Bytes come via the Rust backend: no S3 CORS, encrypted decrypts transparently.
export function PdfPreview(props: { obj: CachedObjectMeta }) {
  const tooBig = () => props.obj.size > PDF_CAP;
  const [expanded, setExpanded] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [doc, setDoc] = createSignal<PDFDocumentProxy | null>(null);
  const [pageNum, setPageNum] = createSignal(1);
  const [numPages, setNumPages] = createSignal(0);
  const [zoomPct, setZoomPct] = createSignal(100);
  // Drag is either a pan or a text selection (same gesture), so a header toggle
  // switches between Move and Select modes. Same model on touch and desktop.
  const [selecting, setSelecting] = createSignal(false);
  // Tauri WebView suppresses the native long-press Copy callout, so we show our
  // own Copy button whenever there's a non-empty selection in the overlay.
  const [hasSel, setHasSel] = createSignal(false);
  const [copied, setCopied] = createSignal(false);

  let canvas: HTMLCanvasElement | undefined;
  let wrap: HTMLDivElement | undefined;
  let pageEl: HTMLDivElement | undefined;
  let textEl: HTMLDivElement | undefined;
  let renderTask: RenderTask | null = null;
  let loadingTask: PDFDocumentLoadingTask | null = null;
  let textLayer: { cancel: () => void } | null = null;

  // renderZoom is baked into canvas pixels (crisp); liveScale is the transient
  // CSS scale applied live during a pinch, folded back into renderZoom on settle.
  let renderZoom = 1;
  let liveScale = 1;
  let tx = 0;
  let ty = 0;
  // Cached CSS display size at liveScale=1 so clampPan never reads offsetWidth
  // (which forces a layout reflow every move).
  let dispW = 0;
  let dispH = 0;
  let rafId = 0;

  const pointers = new Map<number, { x: number; y: number }>();
  let prevCentroid: { x: number; y: number } | null = null;
  let prevDist = 0;
  let lastTap = 0;

  function destroyDoc() {
    renderTask?.cancel();
    renderTask = null;
    textLayer?.cancel();
    textLayer = null;
    // destroy() on the loading task tears down the doc + its worker port.
    loadingTask?.destroy();
    loadingTask = null;
    setDoc(null);
  }

  function resetView() {
    renderZoom = 1;
    liveScale = 1;
    tx = 0;
    ty = 0;
    setZoomPct(100);
  }

  createEffect(() => {
    void props.obj.key;
    destroyDoc();
    setExpanded(false);
    setErr(null);
    setPageNum(1);
    setNumPages(0);
    resetView();
  });

  onCleanup(() => { if (rafId) cancelAnimationFrame(rafId); destroyDoc(); });

  // Android back closes the modal before the preview pane itself.
  useBackHandler(() => expanded(), () => {
    if (expanded()) { close(); return true; }
    return false;
  });

  async function loadPdf() {
    if (doc() || loading()) return;
    setLoading(true);
    setErr(null);
    try {
      const r = await previewObject(props.obj.account_id, props.obj.bucket, props.obj.key, PDF_CAP);
      const pdfjs = await loadPdfjs();
      const data = new Uint8Array(r.bytes);
      loadingTask = pdfjs.getDocument({ data });
      const loaded = await loadingTask.promise;
      setDoc(loaded);
      setNumPages(loaded.numPages);
      setPageNum(1);
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setLoading(false);
    }
  }

  createEffect(() => {
    if (!expanded() || !wrap) return;
    let raf = 0;
    const ro = new ResizeObserver(() => {
      if (raf) return;
      raf = requestAnimationFrame(() => {
        raf = 0;
        const d = doc();
        if (d) { resetView(); renderPage(d, pageNum()); }
      });
    });
    ro.observe(wrap);
    onCleanup(() => { if (raf) cancelAnimationFrame(raf); ro.disconnect(); });
  });

  function open() { setExpanded(true); loadPdf(); }
  function close() {
    renderTask?.cancel();
    renderTask = null;
    setExpanded(false);
  }

  createEffect(() => {
    const d = doc();
    const n = pageNum();
    if (!d || !canvas || !wrap) return;
    resetView();
    renderPage(d, n);
  });

  async function renderPage(d: PDFDocumentProxy, n: number, retries = 0) {
    if (!expanded()) return; // modal closed mid-retry — stop (avoids a rAF loop)
    // The modal may not be laid out on the first open — clientWidth 0 would fit
    // the page to nothing. Wait a frame and retry until the wrapper has width.
    if (!wrap || wrap.clientWidth === 0) {
      if (retries >= 60) return;
      requestAnimationFrame(() => renderPage(d, n, retries + 1));
      return;
    }
    renderTask?.cancel();
    try {
      const page = await d.getPage(n);
      const el = canvas!;
      const dpr = window.devicePixelRatio || 1;
      const base = page.getViewport({ scale: 1 });
      const avail = (wrap!.clientWidth || base.width) - 24;
      const fit = avail > 0 ? avail / base.width : 1;
      const cssW = fit * renderZoom * base.width;
      const cssH = fit * renderZoom * base.height;
      let pixScale = fit * renderZoom * dpr;
      pixScale = Math.min(pixScale, PIX_CAP / base.width, PIX_CAP / base.height);
      const viewport = page.getViewport({ scale: pixScale });
      const ctx = el.getContext("2d");
      if (!ctx) return;
      el.width = Math.round(viewport.width);
      el.height = Math.round(viewport.height);
      el.style.width = `${cssW}px`;
      el.style.height = `${cssH}px`;
      if (pageEl) { pageEl.style.width = `${cssW}px`; pageEl.style.height = `${cssH}px`; }
      dispW = cssW;
      dispH = cssH;
      if (renderZoom === 1 && liveScale === 1) {
        const ww = wrap!.clientWidth;
        tx = cssW < ww ? (ww - cssW) / 2 : 0;
        ty = 0;
      }
      renderTask = page.render({ canvas: el, canvasContext: ctx, viewport });
      await renderTask.promise;
      renderTask = null;
      applyTransform();
      // Selectable text layer; failure (e.g. older pdf.js without TextLayer)
      // leaves the canvas usable, just without text selection.
      renderTextLayer(page, fit * renderZoom).catch(() => {});
    } catch (e: any) {
      // A cancelled render throws RenderingCancelledException; ignore it.
      if (e?.name !== "RenderingCancelledException") setErr(errMsg(e));
    }
  }

  // Text overlay: `--total-scale-factor` drives the span sizing pdf.js emits;
  // viewport is in CSS units (no dpr) so spans align with the canvas CSS box.
  async function renderTextLayer(page: any, cssScale: number) {
    if (!textEl) return;
    const pdfjs: any = await loadPdfjs();
    if (typeof pdfjs.TextLayer !== "function") return;
    textLayer?.cancel();
    textEl.replaceChildren();
    textEl.style.setProperty("--total-scale-factor", String(cssScale));
    const viewport = page.getViewport({ scale: cssScale });
    const layer = new pdfjs.TextLayer({
      textContentSource: page.streamTextContent(),
      container: textEl,
      viewport,
    });
    textLayer = layer;
    await layer.render();
  }

  // Clamp the pan without re-anchoring: in-range values are left untouched so it
  // never fights the pinch anchor (the zoom-jitter source). Cached dims, no reflow.
  function clampPan() {
    if (!wrap) return;
    const cw = dispW * liveScale;
    const ch = dispH * liveScale;
    const ww = wrap.clientWidth;
    const wh = wrap.clientHeight;
    const [loX, hiX] = cw <= ww ? [0, ww - cw] : [ww - cw, 0];
    const [loY, hiY] = ch <= wh ? [0, wh - ch] : [wh - ch, 0];
    tx = Math.min(hiX, Math.max(loX, tx));
    ty = Math.min(hiY, Math.max(loY, ty));
  }

  function applyTransform() {
    if (!pageEl) return;
    clampPan();
    pageEl.style.transform = `translate(${tx}px, ${ty}px) scale(${liveScale})`;
  }

  function scheduleApply() {
    if (rafId) return;
    rafId = requestAnimationFrame(() => { rafId = 0; applyTransform(); });
  }

  // Fold the live pinch scale into the baked render zoom (translate is in CSS
  // pixels so the swap is visually identical) and re-render crisp.
  function commitZoom() {
    if (Math.abs(liveScale - 1) < 0.01) { liveScale = 1; return; }
    const total = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, renderZoom * liveScale));
    renderZoom = total;
    liveScale = 1;
    setZoomPct(Math.round(renderZoom * 100));
    const d = doc();
    if (d) renderPage(d, pageNum());
  }

  function centroidOf(rect: DOMRect) {
    const pts = [...pointers.values()];
    const x = pts.reduce((s, p) => s + p.x, 0) / pts.length - rect.left;
    const y = pts.reduce((s, p) => s + p.y, 0) / pts.length - rect.top;
    return { x, y };
  }
  function spreadOf() {
    const pts = [...pointers.values()];
    if (pts.length < 2) return 0;
    return Math.hypot(pts[0].x - pts[1].x, pts[0].y - pts[1].y);
  }

  function onPointerDown(e: PointerEvent) {
    if (selecting()) return; // Select mode: let the browser drive selection
    (e.currentTarget as HTMLElement).setPointerCapture?.(e.pointerId);
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const rect = wrap!.getBoundingClientRect();
    prevCentroid = centroidOf(rect);
    prevDist = spreadOf();
    const now = e.timeStamp;
    if (pointers.size === 1 && now - lastTap < 300) {
      toggleZoom({ x: e.clientX - rect.left, y: e.clientY - rect.top });
      lastTap = 0;
    } else {
      lastTap = now;
    }
  }

  function onPointerMove(e: PointerEvent) {
    if (selecting()) return;
    if (!pointers.has(e.pointerId)) return;
    pointers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const rect = wrap!.getBoundingClientRect();
    const c = centroidOf(rect);
    if (pointers.size >= 2) {
      const dist = spreadOf();
      if (prevDist > 0 && dist > 0) zoomAbout(c, dist / prevDist);
      prevDist = dist;
    }
    if (prevCentroid) {
      tx += c.x - prevCentroid.x;
      ty += c.y - prevCentroid.y;
    }
    prevCentroid = c;
    scheduleApply();
  }

  function endPointer(e: PointerEvent) {
    if (!pointers.delete(e.pointerId)) return;
    if (pointers.size >= 1) {
      const rect = wrap!.getBoundingClientRect();
      prevCentroid = centroidOf(rect);
      prevDist = spreadOf();
    } else {
      prevCentroid = null;
      prevDist = 0;
      commitZoom();
    }
  }

  function zoomAbout(c: { x: number; y: number }, factor: number) {
    const total = renderZoom * liveScale * factor;
    const clamped = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, total));
    const applied = clamped / (renderZoom * liveScale);
    tx = c.x - applied * (c.x - tx);
    ty = c.y - applied * (c.y - ty);
    liveScale *= applied;
    setZoomPct(Math.round(renderZoom * liveScale * 100));
  }

  function toggleZoom(c: { x: number; y: number }) {
    const cur = renderZoom * liveScale;
    zoomAbout(c, cur > 1.01 ? 1 / cur : 2);
    applyTransform();
    commitZoom();
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    const rect = wrap!.getBoundingClientRect();
    const c = { x: e.clientX - rect.left, y: e.clientY - rect.top };
    if (e.ctrlKey) {
      // Trackpad pinch / ctrl-scroll
      zoomAbout(c, e.deltaY < 0 ? 1.1 : 1 / 1.1);
      applyTransform();
      commitZoom();
    } else {
      tx -= e.deltaX;
      ty -= e.deltaY;
      scheduleApply();
    }
  }

  function onSelectionChange() {
    if (!selecting() || !textEl) { setHasSel(false); return; }
    const sel = window.getSelection();
    const text = sel?.toString().trim() ?? "";
    const anchored = !!sel && sel.anchorNode != null && textEl.contains(sel.anchorNode);
    setHasSel(!!text && anchored);
  }
  createEffect(() => {
    if (!expanded()) return;
    document.addEventListener("selectionchange", onSelectionChange);
    onCleanup(() => document.removeEventListener("selectionchange", onSelectionChange));
  });
  createEffect(() => {
    if (!selecting()) { window.getSelection()?.removeAllRanges(); setHasSel(false); }
  });

  function copySelection() {
    const text = window.getSelection()?.toString() ?? "";
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
      window.getSelection()?.removeAllRanges();
      setHasSel(false);
    }).catch(() => {});
  }

  const prev = () => setPageNum((p) => Math.max(1, p - 1));
  const next = () => setPageNum((p) => Math.min(numPages(), p + 1));

  function zoomStep(factor: number) {
    if (!wrap) return;
    const c = { x: wrap.clientWidth / 2, y: wrap.clientHeight / 2 };
    zoomAbout(c, factor);
    applyTransform();
    commitZoom();
  }

  return (
    <>
      <div class="preview-img-area sheet-preview-col">
        <Show when={tooBig()}>
          <span class="muted sheet-preview-hint">File too large to preview ({formatBytes(props.obj.size)} · max {formatBytes(PDF_CAP)})</span>
        </Show>
        <Show when={!tooBig()}>
          <button class="btn-secondary preview-btn-inline" onClick={open}>
            <IconEye size={15} /> View PDF
          </button>
        </Show>
      </div>

      <Show when={expanded()}>
        <div class="pdf-modal-overlay" onClick={close}>
          <div class="pdf-modal-inner" onClick={(e) => e.stopPropagation()}>
            <div class="pdf-modal-header">
              <span class="pdf-modal-title">{props.obj.basename}</span>
              <Show when={doc()}>
                <div class="pdf-modal-nav">
                  <button class="icon-btn" disabled={pageNum() <= 1} onClick={prev} aria-label="Previous page">‹</button>
                  <span class="pdf-page-ind">{pageNum()} / {numPages()}</span>
                  <button class="icon-btn" disabled={pageNum() >= numPages()} onClick={next} aria-label="Next page">›</button>
                </div>
              </Show>
              <button class="icon-btn pdf-modal-close" onClick={close} aria-label="Close"><IconX size={18} /></button>
              <Show when={doc()}>
                <div class="pdf-header-controls">
                  <div class="pdf-modal-zoom">
                    <button class="icon-btn" disabled={zoomPct() <= MIN_ZOOM * 100} onClick={() => zoomStep(1 / 1.25)} aria-label="Zoom out">−</button>
                    <span class="pdf-zoom-ind">{zoomPct()}%</span>
                    <button class="icon-btn" disabled={zoomPct() >= MAX_ZOOM * 100} onClick={() => zoomStep(1.25)} aria-label="Zoom in">+</button>
                  </div>
                  <button
                    class="btn-ghost pdf-mode-btn"
                    classList={{ active: selecting() }}
                    onClick={() => setSelecting((s) => !s)}
                  >
                    {selecting() ? "Move" : "Select"}
                  </button>
                </div>
              </Show>
            </div>

            <div
              class="pdf-canvas-wrap"
              classList={{ selecting: selecting() }}
              ref={wrap}
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={endPointer}
              onPointerCancel={endPointer}
              onWheel={onWheel}
              onDragStart={(e) => e.preventDefault()}
              onDrop={(e) => { e.preventDefault(); e.stopPropagation(); }}
              onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); }}
            >
              <Show when={loading()}>
                <div class="preview-loader pdf-modal-loading">
                  <span class="spinner spinner-lg" />
                  <span>Loading PDF…</span>
                </div>
              </Show>
              <Show when={err()}>
                <div class="status-msg err pdf-modal-err">{err()}</div>
              </Show>
              <div ref={pageEl} class="pdf-page" classList={{ hidden: !doc() }}>
                <canvas ref={canvas} class="pdf-canvas" />
                <div ref={textEl} class="textLayer" />
              </div>
              <Show when={selecting() && (hasSel() || copied())}>
                <button class="pdf-copy-fab" onClick={copySelection}>
                  {copied() ? "Copied!" : "Copy selection"}
                </button>
              </Show>
            </div>
          </div>
        </div>
      </Show>
    </>
  );
}
