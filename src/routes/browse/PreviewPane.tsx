import { createSignal, createResource, Show, createEffect, onCleanup } from "solid-js";
import { presignGet, previewObject, putObjectText } from "../../api/objects";
import { notify } from "../../utils/notify";
import { errMsg } from "../../state/toast";
import { errCode } from "../../utils/errors";
import { formatBytes } from "../../utils/fmt";
import {
  FileIcon,
  IconX, IconEdit, IconEye, IconArrowUpLine,
} from "../../utils/icons";
import type { CachedObjectMeta } from "../../types";
import { CodeEditor, EditorModal } from "../../utils/CodeEditor";
import { resolvedTheme } from "../../state/theme";
import { IMAGE_EXTS, TEXT_EXTS, SHEET_EXTS, extOf } from "./helpers";
import { Lightbox } from "./preview/Lightbox";
import { SheetPreview } from "./preview/SheetModal";
import { MetaList } from "./preview/MetaList";
import { useBackHandler } from "../../utils/androidBack";

// Map a Tauri IPC rejection to a short, human-facing (title, hint) pair for
// the preview error card. Falls back to the raw wire message when the code
// isn't specifically recognised.
function previewErrorParts(err: unknown): { title: string; hint: string } {
  const code = errCode(err);
  const msg = errMsg(err);
  if (code === "encryption_identity_missing") {
    return {
      title: "Encryption key missing on this device",
      hint: "Load the key file you saved earlier to open this file.",
    };
  }
  if (code === "invalid_input" && /age/i.test(msg)) {
    return {
      title: "Cannot open this file",
      hint: "It was encrypted with a different key, or was not encrypted by cosmog. If you have the original key, load it from the bucket encryption menu.",
    };
  }
  return { title: "Preview failed", hint: msg };
}

function PreviewErrorCard(props: { err: unknown }) {
  const parts = () => previewErrorParts(props.err);
  return (
    <div class="preview-err-inline">
      <div class="preview-err-inline-title">{parts().title}</div>
      <div class="preview-err-inline-hint">{parts().hint}</div>
    </div>
  );
}

// ── preview pane ──────────────────────────────────────────────────────────────

export function PreviewPane(props: { obj: CachedObjectMeta; onClose: () => void; onDownload: () => void; onCopyLink: () => void; encrypted?: boolean; }) {
  const ct = () => props.obj.content_type ?? "";
  const ext = () => extOf(props.obj.basename);
  const isImage = () => ct().startsWith("image/") || IMAGE_EXTS.has(ext());
  const isSheet = () => SHEET_EXTS.has(ext());
  const isText = () => !isSheet() && (ct().startsWith("text/") || ct().includes("json") || ct().includes("xml") || ct().includes("javascript") || TEXT_EXTS.has(ext()));

  const [loadRequested, setLoadRequested] = createSignal(false);
  const [expanded, setExpanded] = createSignal(false);
  const [editOpen, setEditOpen] = createSignal(false);

  // Android back: close the lightbox / editor before the preview pane itself
  // (which ObjectBrowser closes once this returns false).
  useBackHandler(() => true, () => {
    if (expanded()) { setExpanded(false); return true; }
    if (editOpen()) { setEditOpen(false); return true; }
    return false;
  });
  const tooBig = () => props.obj.size > 10 * 1024 * 1024;
  // Encrypted images are decrypted whole into a Blob URL. Cap auto-load so a
  // 100 MB ciphertext doesn't balloon the webview. User can still click
  // "Load preview" to force it (via loadRequested()).
  const ENCRYPTED_IMAGE_AUTOLOAD_MAX = 8 * 1024 * 1024;
  const imageAutoLoad = () =>
    isImage() &&
    !(props.encrypted && props.obj.size > ENCRYPTED_IMAGE_AUTOLOAD_MAX);
  const textAutoLoad = () => isText() && props.obj.size <= 512 * 1024;

  // Reset on object change
  createEffect(() => { void props.obj.key; setLoadRequested(false); setExpanded(false); setEditOpen(false); });

  // Images: presigned URL for raster images; blob/data URL for SVG and encrypted buckets.
  // Wait until encStatus resolves (encrypted !== undefined) — otherwise the resource
  // fires once with encrypted=undefined (wrong path, ciphertext blob) then refetches
  // once status arrives, causing a visible reload/jitter.
  const [imgUrl] = createResource(
    () => {
      if (props.encrypted === undefined) return null;
      if (!isImage()) return null;
      if (!(imageAutoLoad() || loadRequested())) return null;
      return { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket, x: ext(), enc: props.encrypted };
    },
    async ({ a, b, k, x, enc }) => {
      if (x === "svg" || enc) {
        const maxBytes = props.obj.size > 0 ? props.obj.size + 64 : 20 * 1024 * 1024;
        const r = await previewObject(a, b, k, maxBytes);
        const mimeType = r.content_type || (x === "svg" ? "image/svg+xml" : `image/${x}`);
        const blob = new Blob([new Uint8Array(r.bytes)], { type: mimeType });
        return { url: URL.createObjectURL(blob), key: k };
      }
      return { url: await presignGet(a, b, k), key: k };
    },
  );
  // Latch: keep displaying the previous URL while the next one is fetching in
  // the background. createResource returns undefined during a source-keyed
  // refetch, which would unmount <img> and flash a blank frame. displayUrl
  // only ever advances to a defined value, so the img element stays mounted.
  const [displayUrl, setDisplayUrl] = createSignal<string | null>(null);
  const [displayKey, setDisplayKey] = createSignal<string | null>(null);
  let priorBlob: string | null = null;
  createEffect(() => {
    const r = imgUrl();
    if (!r) return;
    if (priorBlob && priorBlob !== r.url && priorBlob.startsWith("blob:")) {
      URL.revokeObjectURL(priorBlob);
    }
    priorBlob = r.url.startsWith("blob:") ? r.url : null;
    setDisplayUrl(r.url);
    setDisplayKey(r.key);
  });
  // Reset latch when the preview target changes to a non-image (so we don't
  // keep showing an old image over a text/binary preview).
  createEffect(() => {
    if (!isImage()) { setDisplayUrl(null); setDisplayKey(null); }
  });
  onCleanup(() => { if (priorBlob) URL.revokeObjectURL(priorBlob); });

  // Text: fetch bytes via backend
  const [loadedKey, setLoadedKey] = createSignal<string | null>(null);
  const textShouldFetch = () => isText() && !isImage() && (textAutoLoad() || loadRequested());
  const [preview, { refetch: refetchPreview }] = createResource(
    () => (textShouldFetch() ? { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket } : null),
    async ({ a, b, k }) => { try { const r = await previewObject(a, b, k, 256 * 1024); return r; } finally { setLoadedKey(k); } },
  );

  // Latch text preview the same way as images: hold the previously-loaded
  // bytes on-screen while the next target is fetched in the background, so
  // switching between text files doesn't unmount CodeEditor and flash blank.
  type TextSnap = { key: string; bytes: number[]; content_type?: string | null };
  const [displayText, setDisplayText] = createSignal<TextSnap | null>(null);
  createEffect(() => {
    const p = preview();
    const k = loadedKey();
    if (!p || !k) return;
    setDisplayText({ key: k, bytes: p.bytes, content_type: p.content_type });
  });
  // Clear latch when target is no longer a text preview (e.g. switched to image).
  createEffect(() => { if (!isText() || isImage()) setDisplayText(null); });
  // Fresh preview available for the current target?
  const cur = () => {
    const d = displayText();
    return d && d.key === props.obj.key ? d : null;
  };
  function textContent() {
    const d = displayText(); if (!d) return "";
    try { return new TextDecoder().decode(new Uint8Array(d.bytes)); }
    catch { return ""; }
  }

  const imgSrc = () => displayUrl() ?? "";
  // Min-duration switching flag: turns on the moment a switch begins (target
  // key differs from displayed key) and stays on until the new content lands
  // AND a floor delay has elapsed, so fast fetches still show the overlay.
  const SWITCH_MIN_MS = 350;
  const [imgSwitching, setImgSwitching] = createSignal(false);
  let imgSwitchTimer: number | null = null;
  createEffect(() => {
    const targetKey = props.obj.key;
    const shown = displayKey();
    if (isImage() && shown !== null && shown !== targetKey) {
      setImgSwitching(true);
      if (imgSwitchTimer !== null) { clearTimeout(imgSwitchTimer); imgSwitchTimer = null; }
      imgSwitchTimer = window.setTimeout(() => {
        imgSwitchTimer = null;
        if (displayKey() === props.obj.key) setImgSwitching(false);
      }, SWITCH_MIN_MS);
    } else if (shown === targetKey && imgSwitchTimer === null) {
      setImgSwitching(false);
    }
  });
  onCleanup(() => { if (imgSwitchTimer !== null) clearTimeout(imgSwitchTimer); });

  const [textSwitching, setTextSwitchingLatched] = createSignal(false);
  let textSwitchTimer: number | null = null;
  createEffect(() => {
    const targetKey = props.obj.key;
    const d = displayText();
    const shown = d?.key ?? null;
    if (isText() && !isImage() && shown !== null && shown !== targetKey) {
      setTextSwitchingLatched(true);
      if (textSwitchTimer !== null) { clearTimeout(textSwitchTimer); textSwitchTimer = null; }
      textSwitchTimer = window.setTimeout(() => {
        textSwitchTimer = null;
        const cur = displayText();
        if ((cur?.key ?? null) === props.obj.key) setTextSwitchingLatched(false);
      }, SWITCH_MIN_MS);
    } else if (shown === targetKey && textSwitchTimer === null) {
      setTextSwitchingLatched(false);
    }
  });
  onCleanup(() => { if (textSwitchTimer !== null) clearTimeout(textSwitchTimer); });

  async function saveEdit(content: string) {
    const ct = props.obj.content_type || `text/${ext() || "plain"}`;
    await putObjectText(props.obj.account_id, props.obj.bucket, props.obj.key, content, ct);
    refetchPreview();
    notify(`Saved ${props.obj.basename}`, props.obj.bucket, {
      largeBody: `Saved changes to "${props.obj.key}" in "${props.obj.bucket}"`,
    });
  }

  return (
    <>
      <div class="preview-pane">
        <div class="preview-header">
          <FileIcon name={props.obj.basename} size={20} />
          <span class="preview-title">{props.obj.basename}</span>
          <Show when={isImage() && displayUrl()}>
            <button class="icon-btn" onClick={() => setExpanded(true)}><IconArrowUpLine size={15} /></button>
          </Show>
          <Show when={isText() && cur()}>
            <button class="icon-btn" onClick={() => setEditOpen(true)}><IconEdit size={15} /></button>
          </Show>
          <button class="icon-btn" onClick={props.onClose}><IconX size={16} /></button>
        </div>
        <div class="preview-body">
          <Show when={preview.error}>
            <PreviewErrorCard err={preview.error} />
          </Show>

          {/* Image preview via presigned URL */}
          <Show when={isImage()}>
            <div class="preview-img-area rel">
              <Show when={!imageAutoLoad() && !loadRequested() && !displayUrl()}>
                <div class="preview-load-hint">
                  <span class="muted text-xs">
                    Encrypted image ({formatBytes(props.obj.size)}) — decrypts whole into memory.
                  </span>
                  <button class="btn-secondary preview-btn-inline" onClick={() => setLoadRequested(true)}>
                    <IconEye size={15} /> Load preview
                  </button>
                </div>
              </Show>
              <Show when={imgUrl.loading && !displayUrl()}>
                <div class="preview-decrypting">
                  <span class="spinner" />
                  <Show when={props.encrypted}>
                    <span class="muted text-xxs">Decrypting…</span>
                  </Show>
                </div>
              </Show>
              <Show when={imgUrl.error && !displayUrl()}>
                <PreviewErrorCard err={imgUrl.error} />
              </Show>
              <Show when={displayUrl()}>
                <img
                  class="preview-thumb preview-img-thumb-zoom"
                  classList={{ "preview-thumb-switching": imgSwitching() }}
                  src={imgSrc()}
                  onClick={() => setExpanded(true)}
                />
                <Show when={imgSwitching()}>
                  <div class="preview-switching-overlay">
                    <span class="spinner spinner-lg" />
                  </div>
                </Show>
              </Show>
            </div>
          </Show>

          <Show when={isText() && !isImage()}>
            <Show when={textAutoLoad()}>
              <Show when={preview.loading && !displayText()}>
                <div class="loading-row"><span class="spinner" /> {props.encrypted ? "Decrypting…" : "Loading…"}</div>
              </Show>
              <Show when={displayText()}>
                <div class="preview-editor rel">
                  <CodeEditor value={textContent()} ext={extOf(displayText()!.key)} readOnly dark={resolvedTheme() === "dark"} />
                  <Show when={textSwitching()}>
                    <div class="preview-switching-overlay">
                      <span class="spinner spinner-lg" />
                    </div>
                  </Show>
                </div>
              </Show>
            </Show>
            <Show when={!textAutoLoad()}>
              <div class="preview-img-area rel">
                <Show when={!loadRequested() && !displayText()}>
                  <Show when={tooBig()}
                        fallback={
                          <button class="btn-secondary preview-btn-inline" onClick={() => setLoadRequested(true)}>
                            <IconEye size={15} /> Load preview
                          </button>
                        }>
                    <span class="muted text-xs">File too large to preview</span>
                  </Show>
                </Show>
                <Show when={loadRequested() && !displayText()}><span class="spinner" /></Show>
                <Show when={displayText()}>
                  <div class="preview-editor full">
                    <CodeEditor value={textContent()} ext={extOf(displayText()!.key)} readOnly dark={resolvedTheme() === "dark"} />
                    <Show when={textSwitching()}>
                      <div class="preview-switching-overlay">
                        <span class="spinner spinner-lg" />
                      </div>
                    </Show>
                  </div>
                </Show>
              </div>
            </Show>
          </Show>

          {/* Spreadsheet preview */}
          <Show when={isSheet()}>
            <SheetPreview obj={props.obj} />
          </Show>

          <Show when={!isImage() && !isText() && !isSheet()}>
            <div class="muted preview-binary-note">
              Binary content · {formatBytes(props.obj.size)}
            </div>
          </Show>

          <MetaList obj={props.obj} />

          <div class="btn-row">
            <button class="btn-secondary btn-half" onClick={props.onCopyLink}>Copy link</button>
            <button class="btn-primary btn-half" onClick={props.onDownload}>Download</button>
          </div>
        </div>
      </div>

      {/* Lightbox */}
      <Lightbox
        open={expanded() && !!displayUrl()}
        src={imgSrc()}
        alt={props.obj.basename}
        onClose={() => setExpanded(false)}
      />

      {/* Full-screen text editor modal */}
      <Show when={editOpen() && cur()}>
        <EditorModal
          value={textContent()}
          ext={ext()}
          filename={props.obj.basename}
          dark={resolvedTheme() === "dark"}
          onSave={saveEdit}
          onClose={() => setEditOpen(false)}
        />
      </Show>
    </>
  );
}
