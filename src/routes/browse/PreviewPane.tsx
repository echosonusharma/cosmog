import { createSignal, createResource, Show, createEffect, onCleanup, lazy, Suspense } from "solid-js";
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
import { resolvedTheme } from "../../state/theme";
import { IMAGE_EXTS, TEXT_EXTS, SHEET_EXTS, PDF_EXTS, AUDIO_EXTS, extOf, isDotEnvName, editorExtOf } from "./helpers";
import { PdfPreview } from "./preview/PdfModal";

// Heavy libs (CodeMirror core, cropperjs, exceljs) are code-split: each chunk
// only loads when its preview/editor is first shown.
const CodeEditor = lazy(() => import("../../utils/CodeEditor").then((m) => ({ default: m.CodeEditor })));
const EditorModal = lazy(() => import("../../utils/CodeEditor").then((m) => ({ default: m.EditorModal })));
const ImageEditor = lazy(() => import("./preview/ImageEditor").then((m) => ({ default: m.ImageEditor })));
const SheetPreview = lazy(() => import("./preview/SheetModal").then((m) => ({ default: m.SheetPreview })));

const chunkSpinner = () => (
  <div class="preview-loader"><span class="spinner spinner-lg" /></div>
);
import { AudioPreview } from "./preview/AudioPlayer";
import { MetaList } from "./preview/MetaList";
import { useBackHandler } from "../../utils/androidBack";

// Maps a Tauri IPC rejection to a short human-facing (title, hint) pair,
// falling back to the raw wire message when the code isn't recognized.
function previewErrorParts(err: unknown, storageClass?: string | null): { title: string; hint: string } {
  const code = errCode(err);
  const msg = errMsg(err);
  if (code === "archived") {
    const cls = storageClass ? ` (${storageClass})` : "";
    return {
      title: "File can't be previewed",
      hint: `This object's storage class${cls} doesn't allow direct access. Restore it or move it to a standard tier before previewing or downloading.`,
    };
  }
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

function PreviewErrorCard(props: { err: unknown; storageClass?: string | null }) {
  const parts = () => previewErrorParts(props.err, props.storageClass);
  return (
    <div class="preview-err-inline">
      <div class="preview-err-inline-title">{parts().title}</div>
      <div class="preview-err-inline-hint">{parts().hint}</div>
    </div>
  );
}

export function PreviewPane(props: { obj: CachedObjectMeta; onClose: () => void; onDownload: () => void; onCopyLink: () => void; encrypted?: boolean; reloadToken?: number; onListChanged?: () => void; }) {
  const ct = () => props.obj.content_type ?? "";
  const ext = () => extOf(props.obj.basename);
  const editorExt = () => editorExtOf(props.obj.basename);
  const isImage = () => ct().startsWith("image/") || IMAGE_EXTS.has(ext());
  const isSheet = () => SHEET_EXTS.has(ext());
  const isPdf = () => ct() === "application/pdf" || PDF_EXTS.has(ext());
  const isAudio = () => ct().startsWith("audio/") || AUDIO_EXTS.has(ext());
  const isText = () => !isSheet() && !isPdf() && !isAudio() && (
    ct().startsWith("text/") || ct().includes("json") || ct().includes("xml") || ct().includes("javascript")
    || TEXT_EXTS.has(ext()) || isDotEnvName(props.obj.basename)
  );

  type PreviewKind = "image" | "text" | "sheet" | "pdf" | "audio" | "binary";
  const targetKind = (): PreviewKind => {
    if (isImage()) return "image";
    if (isText()) return "text";
    if (isSheet()) return "sheet";
    if (isPdf()) return "pdf";
    if (isAudio()) return "audio";
    return "binary";
  };

  const [loadRequested, setLoadRequested] = createSignal(false);
  const [expanded, setExpanded] = createSignal(false);
  const [editOpen, setEditOpen] = createSignal(false);
  // Bumped after an in-place image save so the presigned/blob URL refetches.
  const [imgReload, setImgReload] = createSignal(0);

  // Android back: close the lightbox / editor before the preview pane itself
  // (which ObjectBrowser closes once this returns false).
  useBackHandler(() => true, () => {
    if (expanded()) { setExpanded(false); return true; }
    if (editOpen()) { setEditOpen(false); return true; }
    return false;
  });
  const tooBig = () => props.obj.size > 10 * 1024 * 1024;
  // Encrypted images decrypt whole into a Blob URL; cap auto-load so a large
  // ciphertext can't balloon the webview (user can still force via Load preview).
  const ENCRYPTED_IMAGE_AUTOLOAD_MAX = 8 * 1024 * 1024;
  const imageAutoLoad = () =>
    isImage() &&
    !(props.encrypted && props.obj.size > ENCRYPTED_IMAGE_AUTOLOAD_MAX);
  const textAutoLoad = () => isText() && props.obj.size <= 512 * 1024;

  createEffect(() => { void props.obj.key; setLoadRequested(false); setExpanded(false); setEditOpen(false); });

  // Presigned URL normally; blob URL for SVG + encrypted buckets. Wait until
  // encStatus resolves or it fires with encrypted=undefined (wrong path) + refetch.
  const [imgUrl] = createResource(
    () => {
      if (props.encrypted === undefined) return null;
      if (!isImage()) return null;
      if (!(imageAutoLoad() || loadRequested())) return null;
      return { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket, x: ext(), enc: props.encrypted, r: (props.reloadToken ?? 0) + imgReload() };
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
  // Latch: hold the previous URL while the next fetches — createResource returns
  // undefined mid-refetch, which would unmount <img> and flash a blank frame.
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

  onCleanup(() => { if (priorBlob) URL.revokeObjectURL(priorBlob); });

  function clearImageLatch() {
    if (priorBlob) {
      URL.revokeObjectURL(priorBlob);
      priorBlob = null;
    }
    setDisplayUrl(null);
    setDisplayKey(null);
  }

  const [imgLoaded, setImgLoaded] = createSignal(false);
  createEffect(() => { if (displayUrl()) setImgLoaded(false); });;

  const [loadedKey, setLoadedKey] = createSignal<string | null>(null);
  const textShouldFetch = () => isText() && !isImage() && (textAutoLoad() || loadRequested());
  const [preview, { refetch: refetchPreview }] = createResource(
    () => (textShouldFetch() ? { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket, r: props.reloadToken ?? 0 } : null),
    async ({ a, b, k }) => { try { const r = await previewObject(a, b, k, 256 * 1024); return r; } finally { setLoadedKey(k); } },
  );

  // Latch text like images: hold previously-loaded bytes while the next target
  // fetches, so switching doesn't unmount CodeEditor and flash blank.
  type TextSnap = { key: string; bytes: number[]; content_type?: string | null };
  const [displayText, setDisplayText] = createSignal<TextSnap | null>(null);
  createEffect(() => {
    if (preview.error) return;
    const p = preview();
    const k = loadedKey();
    if (!p || !k) return;
    setDisplayText({ key: k, bytes: p.bytes, content_type: p.content_type });
  });

  const cur = () => {
    const d = displayText();
    return d && d.key === props.obj.key ? d : null;
  };
  function textContent() {
    const d = displayText(); if (!d) return "";
    try { return new TextDecoder().decode(new Uint8Array(d.bytes)); }
    catch { return ""; }
  }

  // Cross-format latch: keep showing the previous kind until the new target is
  // ready, so binary↔text / image↔text switches don't blank the stage mid-frame.
  const isKindReady = (kind: PreviewKind): boolean => {
    if (kind === "image") return displayKey() === props.obj.key && !!displayUrl();
    if (kind === "text") return displayText()?.key === props.obj.key;
    // sheet/pdf/audio/binary render sync from props — always "ready"
    return true;
  };

  const [pinnedKind, setPinnedKind] = createSignal<PreviewKind | null>(null);
  const displayKind = (): PreviewKind | null => {
    const t = targetKind();
    if (isKindReady(t)) return t;
    const pinned = pinnedKind();
    if (pinned === "text" || pinned === "image") return pinned;
    return t;
  };

  createEffect(() => {
    const t = targetKind();
    if (isKindReady(t)) {
      setPinnedKind(t);
      if (t !== "text") setDisplayText(null);
      if (t !== "image") clearImageLatch();
      return;
    }
    if (t === "binary" || t === "sheet" || t === "pdf" || t === "audio") {
      setPinnedKind(t);
      setDisplayText(null);
      clearImageLatch();
    }
  });

  const crossLoading = () => {
    const t = targetKind();
    if (!((t === "text" || t === "image") && !isKindReady(t))) return false;
    // Overlay only while holding prior content — cold load would stack two spinners.
    const pinned = pinnedKind();
    return pinned === "text" || pinned === "image";
  };

  // Once warmed, keep CodeEditor mounted (hidden) so format switches don't
  // re-init CodeMirror — a major source of the whole-UI hitch.
  const [cmWarm, setCmWarm] = createSignal(false);
  createEffect(() => { if (displayText()) setCmWarm(true); });

  const imgSrc = () => displayUrl() ?? "";

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
          <Show when={displayKind() === "image" && displayUrl()}>
            <button class="icon-btn" onClick={() => setExpanded(true)}><IconArrowUpLine size={15} /></button>
          </Show>
          <Show when={displayKind() === "text" && cur()}>
            <button class="icon-btn" onClick={() => setEditOpen(true)}><IconEdit size={15} /></button>
          </Show>
          <button class="icon-btn" onClick={props.onClose}><IconX size={16} /></button>
        </div>
        <div class="preview-body">
          <Show when={preview.error}>
            <PreviewErrorCard err={preview.error} storageClass={props.obj.storage_class} />
          </Show>

          <div class="preview-stage">
          <Show when={displayKind() === "image"}>
            <div class="preview-img-area rel">
              <Show when={!imageAutoLoad() && !loadRequested() && !displayUrl()}>
                <div class="preview-load-hint">
                  <span class="muted text-xs">
                    Encrypted image ({formatBytes(props.obj.size)}). Decrypts whole into memory.
                  </span>
                  <button class="btn-secondary preview-btn-inline" onClick={() => setLoadRequested(true)}>
                    <IconEye size={15} /> Load preview
                  </button>
                </div>
              </Show>
              <Show when={imgUrl.loading && !displayUrl()}>
                <div class="preview-loader">
                  <span class="spinner spinner-lg" />
                  <span>{props.encrypted ? "Decrypting…" : "Loading image…"}</span>
                </div>
              </Show>
              <Show when={imgUrl.error && !displayUrl()}>
                <PreviewErrorCard err={imgUrl.error} />
              </Show>
              <Show when={displayUrl()}>
                <img
                  class="preview-thumb preview-img-thumb-zoom"
                  src={imgSrc()}
                  onClick={() => setExpanded(true)}
                  onLoad={() => setImgLoaded(true)}
                  onError={() => setImgLoaded(true)}
                />
                <Show when={!imgLoaded()}>
                  <div class="preview-switching-overlay">
                    <span class="spinner spinner-lg" />
                  </div>
                </Show>
              </Show>
            </div>
          </Show>

          <Show when={displayKind() === "text" && !preview.error}>
            <Show when={textAutoLoad() && preview.loading && !displayText()}>
              <div class="preview-loader">
                <span class="spinner spinner-lg" />
                <span>{props.encrypted ? "Decrypting…" : "Loading…"}</span>
              </div>
            </Show>
            <Show when={!textAutoLoad() && !displayText()}>
              <div class="preview-img-area rel">
                <Show when={!loadRequested()}
                      fallback={
                        <div class="preview-loader">
                          <span class="spinner spinner-lg" />
                          <span>{props.encrypted ? "Decrypting…" : "Loading…"}</span>
                        </div>
                      }>
                  <Show when={tooBig()}
                        fallback={
                          <button class="btn-secondary preview-btn-inline" onClick={() => setLoadRequested(true)}>
                            <IconEye size={15} /> Load preview
                          </button>
                        }>
                    <span class="muted text-xs">File too large to preview</span>
                  </Show>
                </Show>
              </div>
            </Show>
          </Show>

          <Show when={(cmWarm() || !!displayText()) && !preview.error}>
            <div
              class="preview-editor rel"
              classList={{ hidden: displayKind() !== "text" || !displayText() }}
            >
              <Suspense fallback={chunkSpinner()}>
                <CodeEditor value={textContent()} ext={editorExt()} readOnly dark={resolvedTheme() === "dark"} />
              </Suspense>
            </div>
          </Show>

          <Show when={displayKind() === "sheet"}>
            <Suspense fallback={chunkSpinner()}>
              <SheetPreview obj={props.obj} />
            </Suspense>
          </Show>

          <Show when={displayKind() === "pdf"}>
            <PdfPreview obj={props.obj} />
          </Show>

          <Show when={displayKind() === "audio"}>
            <AudioPreview obj={props.obj} encrypted={props.encrypted} />
          </Show>

          <Show when={displayKind() === "binary"}>
            <div class="muted preview-binary-note">
              Binary content · {formatBytes(props.obj.size)}
            </div>
          </Show>

          <Show when={crossLoading()}>
            <div class="preview-switching-overlay">
              <span class="spinner spinner-lg" />
            </div>
          </Show>
          </div>

          <MetaList obj={props.obj} />

          <div class="btn-row">
            <button class="btn-secondary btn-half" onClick={props.onCopyLink}>Copy link</button>
            <button class="btn-primary btn-half" onClick={props.onDownload}>Download</button>
          </div>
        </div>
      </div>

      <Show when={expanded() && !!displayUrl()}>
        <Suspense fallback={chunkSpinner()}>
        <ImageEditor
          obj={props.obj}
          encrypted={props.encrypted}
          onSaved={(kind) => {
            if (kind === "overwrite") setImgReload((n) => n + 1);
            props.onListChanged?.();
          }}
          onClose={() => setExpanded(false)}
        />
        </Suspense>
      </Show>

      <Show when={editOpen() && cur()}>
        <Suspense fallback={chunkSpinner()}>
        <EditorModal
          value={textContent()}
          ext={editorExt()}
          filename={props.obj.basename}
          dark={resolvedTheme() === "dark"}
          onSave={saveEdit}
          onClose={() => setEditOpen(false)}
        />
        </Suspense>
      </Show>
    </>
  );
}
