import { createSignal, createResource, createEffect, onCleanup, onMount, Show } from "solid-js";
import Cropper from "cropperjs";
import "cropperjs/dist/cropper.css";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { writeFile } from "@tauri-apps/plugin-fs";
import { previewObject, putObjectBytes } from "../../../api/objects";
import { objectExists } from "../newItem";
import { notify } from "../../../utils/notify";
import { errMsg } from "../../../state/toast";
import { formatBytes } from "../../../utils/fmt";
import { t } from "../../../i18n";
import { isMobile } from "../../../utils/breakpoint";
import { extOf } from "../../../utils/fmt";
import { pathFromDialog } from "../helpers";
import type { CachedObjectMeta } from "../../../types";
import {
  IconX, IconZoomIn, IconZoomOut, IconRotateCw, IconRotateCcw,
  IconFlipH, IconFlipV, IconCrop, IconSave, IconMaximize, IconMinimize, IconRefresh, IconDownload,
} from "../../../utils/icons";
import Spinner from "../../../utils/Spinner";
import { useBackHandler } from "../../../utils/androidBack";

// Editing decrypts + rasterizes the whole image in memory and ships the bytes
// over IPC; cap it so a huge object can't OOM the webview.
const EDITOR_MAX_BYTES = 30 * 1024 * 1024;

type OutFmt = { mime: string; ext: string };

// Canvas can only re-encode to a few formats. Map the source extension to the
// nearest target; anything exotic flattens to png.
function outFormat(ext: string): OutFmt {
  if (ext === "jpg" || ext === "jpeg") return { mime: "image/jpeg", ext: "jpg" };
  if (ext === "webp") return { mime: "image/webp", ext: "webp" };
  return { mime: "image/png", ext: "png" };
}

const normExt = (e: string) => (e === "jpeg" ? "jpg" : e);

// dir/name.ext -> dir/name-edited.<outExt>
function copyKey(key: string, outExt: string): string {
  const slash = key.lastIndexOf("/");
  const dir = slash >= 0 ? key.slice(0, slash + 1) : "";
  const name = slash >= 0 ? key.slice(slash + 1) : key;
  const dot = name.lastIndexOf(".");
  const stem = dot >= 0 ? name.slice(0, dot) : name;
  return `${dir}${stem}-edited.${outExt}`;
}

export function ImageEditor(props: {
  obj: CachedObjectMeta;
  encrypted?: boolean;
  onClose: () => void;
  onSaved?: (kind: "overwrite" | "copy") => void;
}) {
  const ext = () => extOf(props.obj.basename);
  const fmt = () => outFormat(ext());
  // Re-encoding to a format that differs from the original extension would leave
  // e.g. photo.gif holding png bytes: block overwrite, only allow save-as.
  const canOverwrite = () => normExt(fmt().ext) === normExt(ext());
  const tooBig = () => props.obj.size > EDITOR_MAX_BYTES;

  let imgEl: HTMLImageElement | undefined;
  let cropper: Cropper | undefined;
  let ownBlob: string | null = null;

  const [cropMode, setCropMode] = createSignal(false);
  const [full, setFull] = createSignal(false);
  const [ready, setReady] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [saveAs, setSaveAs] = createSignal(false);
  const [newKey, setNewKey] = createSignal("");
  const [err, setErr] = createSignal<string | null>(null);
  const [dirty, setDirty] = createSignal(false);
  const [confirmClose, setConfirmClose] = createSignal(false);
  const [quality, setQuality] = createSignal(0.92);
  const [flipX, setFlipX] = createSignal(false);
  const [flipY, setFlipY] = createSignal(false);
  const [natDim, setNatDim] = createSignal<{ w: number; h: number } | null>(null);
  const [cropDim, setCropDim] = createSignal<{ w: number; h: number } | null>(null);

  // Same-origin pixels only, or canvas.toBlob taints on export. Always fetch
  // our own decrypted bytes (never the pane's blob, which it revokes on save).
  const [src] = createResource(
    () => (tooBig() ? null : props.obj.key),
    async () => {
      const maxBytes = props.obj.size > 0 ? props.obj.size + 64 : EDITOR_MAX_BYTES;
      const r = await previewObject(props.obj.account_id, props.obj.bucket, props.obj.key, maxBytes);
      const mime = r.content_type || fmt().mime;
      const url = URL.createObjectURL(new Blob([new Uint8Array(r.bytes)], { type: mime }));
      ownBlob = url;
      return url;
    },
  );

  const onCropEvent = (e: Event) => {
    const d = (e as CustomEvent).detail as { width: number; height: number };
    const w = Math.round(d.width), h = Math.round(d.height);
    if (w > 0 && h > 0) setCropDim({ w, h });
  };

  createEffect(() => {
    const url = src();
    if (!url || !imgEl) return;
    const el = imgEl;
    cropper?.destroy();
    cropper = undefined;
    setReady(false);
    // Build the cropper only after the bitmap loads (measuring an unloaded <img>
    // falls back to tiny minContainer size); one-shot guards double-init.
    let done = false;
    const init = () => {
      if (done) return;
      done = true;
      el.onload = null;
      el.addEventListener("crop", onCropEvent);
      cropper = new Cropper(el, {
        viewMode: 1,
        dragMode: "move",
        autoCrop: false,
        autoCropArea: 1,
        center: true,
        background: true,
        responsive: true,
        checkOrientation: true,
        toggleDragModeOnDblclick: false,
        ready: () => {
          setReady(true);
          const d = cropper?.getImageData();
          if (d) setNatDim({ w: Math.round(d.naturalWidth), h: Math.round(d.naturalHeight) });
        },
      });
    };
    el.onload = init;
    el.src = url;
    if (el.complete && el.naturalWidth > 0) init();
  });

  onCleanup(() => {
    imgEl?.removeEventListener("crop", onCropEvent);
    cropper?.destroy();
    if (ownBlob) URL.revokeObjectURL(ownBlob);
  });

  onMount(() => {
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") requestClose(); };
    window.addEventListener("keydown", onKey);
    onCleanup(() => window.removeEventListener("keydown", onKey));
  });

  useBackHandler(() => true, () => {
    if (confirmClose()) setConfirmClose(false);
    else requestClose();
    return true;
  });

  function requestClose() {
    if (saving()) return;
    if (dirty()) { setConfirmClose(true); return; }
    props.onClose();
  }

  function toggleCrop() {
    if (!cropper) return;
    const next = !cropMode();
    setCropMode(next);
    if (next) { cropper.crop(); cropper.setDragMode("crop"); setDirty(true); }
    else { cropper.clear(); cropper.setDragMode("move"); setCropDim(null); }
  }
  const zoom = (d: number) => cropper?.zoom(d);
  const rotate = (d: number) => { cropper?.rotate(d); setDirty(true); };
  const flipH = () => { const v = !flipX(); setFlipX(v); cropper?.scaleX(v ? -1 : 1); setDirty(true); };
  const flipV = () => { const v = !flipY(); setFlipY(v); cropper?.scaleY(v ? -1 : 1); setDirty(true); };
  // Cropper only refits on window resize, so nudge it after the layout change.
  function toggleFull() {
    setFull(!full());
    requestAnimationFrame(() => window.dispatchEvent(new Event("resize")));
  }

  function reset() {
    setFlipX(false); setFlipY(false);
    cropper?.reset();
    cropper?.scaleX(1); cropper?.scaleY(1);
    if (cropMode()) { setCropMode(false); cropper?.clear(); cropper?.setDragMode("move"); }
    setCropDim(null);
    setDirty(false);
  }
  const setAspect = (r: number) => { cropper?.setAspectRatio(r); setDirty(true); };

  async function encodeCanvas(): Promise<{ blob: Blob; f: OutFmt }> {
    const canvas = cropper!.getCroppedCanvas({ imageSmoothingQuality: "high" });
    const f = fmt();
    const q = f.mime === "image/png" ? undefined : quality();
    const blob: Blob | null = await new Promise((res) => canvas.toBlob((b) => res(b), f.mime, q));
    if (!blob) throw new Error(t("preview.image.encodeFailed"));
    return { blob, f };
  }

  async function doSave(key: string, checkExists: boolean) {
    if (!cropper || saving()) return;
    setErr(null);
    setSaving(true);
    try {
      if (checkExists) {
        const exists = await objectExists(props.obj.account_id, props.obj.bucket, key);
        if (exists) { setErr(t("preview.image.exists")); return; }
      }
      const { blob, f } = await encodeCanvas();
      const bytes = Array.from(new Uint8Array(await blob.arrayBuffer()));
      await putObjectBytes(props.obj.account_id, props.obj.bucket, key, bytes, f.mime);
      notify(t("preview.saved", { name: key.split("/").pop() ?? key }), props.obj.bucket, {
        largeBody: t("preview.image.savedBody", { key, bucket: props.obj.bucket }),
      });
      props.onSaved?.(key === props.obj.key ? "overwrite" : "copy");
      props.onClose();
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setSaving(false);
    }
  }

  async function download() {
    if (!cropper || saving()) return;
    setErr(null);
    try {
      const { blob, f } = await encodeCanvas();
      const stem = props.obj.basename.replace(/\.[^.]+$/, "");
      const sel = await saveDialog({ defaultPath: `${stem}-edited.${f.ext}` });
      if (!sel) return;
      await writeFile(pathFromDialog(sel), new Uint8Array(await blob.arrayBuffer()));
      notify(t("preview.image.downloaded", { name: `${stem}-edited.${f.ext}` }), props.obj.bucket);
    } catch (e) {
      setErr(errMsg(e));
    }
  }

  function startSaveAs() {
    setNewKey(copyKey(props.obj.key, fmt().ext));
    setSaveAs(true);
  }

  const busy = () => saving();

  return (
    <div class="img-editor" classList={{ "is-full": full() }} onClick={requestClose}>
      <div class="img-editor-panel" onClick={(e) => e.stopPropagation()}>
        <div class="img-editor-toolbar">
          <span class="img-editor-name">{props.obj.basename}</span>
          <Show when={natDim()}>
            <span class="img-editor-size muted">
              {natDim()!.w}×{natDim()!.h}
              <Show when={cropMode() && cropDim()}> · {t("preview.image.cropSize", { w: cropDim()!.w, h: cropDim()!.h })}</Show>
            </span>
          </Show>
          <div class="img-editor-tools">
            <button class="icon-btn" title={t("preview.zoomIn")} disabled={!ready()} onClick={() => zoom(0.1)}><IconZoomIn size={16} /></button>
            <button class="icon-btn" title={t("preview.zoomOut")} disabled={!ready()} onClick={() => zoom(-0.1)}><IconZoomOut size={16} /></button>
            <button class="icon-btn" title={t("preview.image.fitReset")} disabled={!ready()} onClick={reset}><IconRefresh size={16} /></button>
            <span class="img-editor-sep" />
            <button class="icon-btn" title={t("preview.image.rotateLeft")} disabled={!ready()} onClick={() => rotate(-90)}><IconRotateCcw size={16} /></button>
            <button class="icon-btn" title={t("preview.image.rotateRight")} disabled={!ready()} onClick={() => rotate(90)}><IconRotateCw size={16} /></button>
            <button class="icon-btn" classList={{ active: flipX() }} title={t("preview.image.flipH")} disabled={!ready()} onClick={flipH}><IconFlipH size={16} /></button>
            <button class="icon-btn" classList={{ active: flipY() }} title={t("preview.image.flipV")} disabled={!ready()} onClick={flipV}><IconFlipV size={16} /></button>
            <span class="img-editor-sep" />
            <button class="icon-btn" classList={{ active: cropMode() }} title={t("preview.image.crop")} disabled={!ready()} onClick={toggleCrop}><IconCrop size={16} /></button>
          </div>
          <button class="icon-btn img-editor-full" title={full() ? t("preview.exitFullView") : t("preview.fullView")} onClick={toggleFull}>
            {full() ? <IconMinimize size={16} /> : <IconMaximize size={16} />}
          </button>
          <button class="icon-btn img-editor-close" title={t("common.close")} onClick={requestClose}><IconX size={18} /></button>
        </div>

        <Show when={cropMode()}>
          <div class="img-editor-aspects">
            <button class="btn-secondary btn-xs" onClick={() => setAspect(NaN)}>{t("preview.image.aspectFree")}</button>
            <button class="btn-secondary btn-xs" onClick={() => setAspect(1)}>1:1</button>
            <button class="btn-secondary btn-xs" onClick={() => setAspect(16 / 9)}>16:9</button>
            <button class="btn-secondary btn-xs" onClick={() => setAspect(4 / 3)}>4:3</button>
            <button class="btn-secondary btn-xs" onClick={() => setAspect(3 / 4)}>3:4</button>
          </div>
        </Show>

        <div class="img-editor-canvas">
          <Show when={tooBig()}>
            <div class="preview-err-inline">
              <div class="preview-err-inline-title">{t("preview.image.tooLargeTitle")}</div>
              <div class="preview-err-inline-hint">{t("preview.image.tooLargeHint", { size: formatBytes(props.obj.size), max: formatBytes(EDITOR_MAX_BYTES) })}</div>
            </div>
          </Show>
          <Show when={!tooBig() && src.loading}>
            <div class="preview-loader"><Spinner size={50} /><span>{props.encrypted ? t("preview.decrypting") : t("common.loading")}</span></div>
          </Show>
          <Show when={src.error}>
            <div class="preview-err-inline"><div class="preview-err-inline-title">{t("preview.image.loadFailed")}</div><div class="preview-err-inline-hint">{errMsg(src.error)}</div></div>
          </Show>
          <img ref={imgEl} class="img-editor-img" alt={props.obj.basename} />

          <Show when={confirmClose()}>
            <div class="img-editor-confirm">
              <div class="img-editor-confirm-box">
                <div class="img-editor-confirm-title">{t("preview.image.discardTitle")}</div>
                <div class="btn-row">
                  <button class="btn-secondary btn-half" onClick={() => setConfirmClose(false)}>{t("preview.keepEditing")}</button>
                  <button class="btn-primary btn-half" onClick={props.onClose}>{t("preview.discard")}</button>
                </div>
              </div>
            </div>
          </Show>
        </div>

        <div class="img-editor-footer">
          <Show when={err()}><span class="img-editor-err">{err()}</span></Show>
          <Show when={ready() && fmt().mime !== "image/png"}>
            <label class="img-editor-quality">
              {t("preview.image.quality")}
              <input type="range" min="0.3" max="1" step="0.05" value={quality()} disabled={busy()}
                onInput={(e) => setQuality(+e.currentTarget.value)} />
              <span>{Math.round(quality() * 100)}%</span>
            </label>
          </Show>
          <Show
            when={saveAs()}
            fallback={
              <div class="img-editor-actions">
                <Show when={!isMobile()}>
                  <button class="btn-secondary" title={t("preview.image.downloadToDisk")} disabled={!ready() || busy()} onClick={download}><IconDownload size={15} /> {t("common.download")}</button>
                </Show>
                <button class="btn-secondary" disabled={!ready() || busy()} onClick={startSaveAs}><IconSave size={15} /> {t("preview.image.saveAsCopy")}</button>
                <button class="btn-primary" title={canOverwrite() ? "" : t("preview.image.cantOverwrite", { ext: ext(), fmt: fmt().ext })}
                  disabled={!ready() || busy() || !canOverwrite()} onClick={() => doSave(props.obj.key, false)}>
                  {saving() ? t("common.saving") : t("preview.image.overwrite")}
                </button>
              </div>
            }
          >
            <div class="img-editor-saveas">
              <input
                class="field img-editor-keyinput"
                value={newKey()}
                onInput={(e) => setNewKey(e.currentTarget.value)}
                placeholder={t("preview.image.destKey")}
                spellcheck={false}
              />
              <button class="btn-secondary" disabled={busy()} onClick={() => setSaveAs(false)}>{t("common.cancel")}</button>
              <button class="btn-primary" disabled={busy() || !newKey().trim()} onClick={() => doSave(newKey().trim(), true)}>
                {saving() ? t("common.saving") : t("preview.image.saveCopy")}
              </button>
            </div>
          </Show>
        </div>
      </div>
    </div>
  );
}
