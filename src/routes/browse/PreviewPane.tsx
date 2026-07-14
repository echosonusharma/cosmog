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
    <div style="width:100%;padding:12px 14px;border:1px solid color-mix(in srgb, var(--red) 30%, transparent);background:color-mix(in srgb, var(--red) 8%, transparent);border-radius:6px;color:var(--text);font-size:12px;line-height:1.5">
      <div style="font-weight:600;margin-bottom:4px;color:var(--red)">{parts().title}</div>
      <div style="color:var(--muted)">{parts().hint}</div>
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
        return URL.createObjectURL(blob);
      }
      return presignGet(a, b, k);
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
    const u = imgUrl();
    if (!u) return;
    if (priorBlob && priorBlob !== u && priorBlob.startsWith("blob:")) {
      URL.revokeObjectURL(priorBlob);
    }
    priorBlob = u.startsWith("blob:") ? u : null;
    setDisplayUrl(u);
    setDisplayKey(props.obj.key);
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
  const textSwitching = () => preview.loading && !!displayText() && displayText()!.key !== props.obj.key;
  function textContent() {
    const d = displayText(); if (!d) return "";
    try { return new TextDecoder().decode(new Uint8Array(d.bytes)); }
    catch { return ""; }
  }

  const imgSrc = () => displayUrl() ?? "";
  // True while a NEW image is being fetched for the current target (previous
  // URL is still on-screen). Drives an unobtrusive spinner overlay instead of
  // an unmount/blank flash.
  const imgSwitching = () => imgUrl.loading && displayKey() !== null && displayKey() !== props.obj.key;

  async function saveEdit(content: string) {
    const ct = props.obj.content_type || `text/${ext() || "plain"}`;
    await putObjectText(props.obj.account_id, props.obj.bucket, props.obj.key, content, ct);
    refetchPreview();
    notify("Saved", props.obj.basename);
  }

  return (
    <>
      <div class="preview-pane">
        <div class="preview-header">
          <FileIcon name={props.obj.basename} size={20} />
          <span class="preview-title">{props.obj.basename}</span>
          <Show when={isImage() && displayUrl()}>
            <button class="icon-btn" title="Expand" onClick={() => setExpanded(true)}><IconArrowUpLine size={15} /></button>
          </Show>
          <Show when={isText() && cur()}>
            <button class="icon-btn" title="Edit" onClick={() => setEditOpen(true)}><IconEdit size={15} /></button>
          </Show>
          <button class="icon-btn" onClick={props.onClose} title="Close"><IconX size={16} /></button>
        </div>
        <div class="preview-body">
          <Show when={preview.error}>
            <PreviewErrorCard err={preview.error} />
          </Show>

          {/* Image preview via presigned URL */}
          <Show when={isImage()}>
            <div class="preview-img-area" style="position:relative">
              <Show when={!imageAutoLoad() && !loadRequested() && !displayUrl()}>
                <div style="display:flex;flex-direction:column;align-items:center;gap:8px;padding:12px">
                  <span class="muted" style="font-size:12px">
                    Encrypted image ({formatBytes(props.obj.size)}) — decrypts whole into memory.
                  </span>
                  <button class="btn-secondary" onClick={() => setLoadRequested(true)}
                          style="display:flex;align-items:center;gap:8px">
                    <IconEye size={15} /> Load preview
                  </button>
                </div>
              </Show>
              <Show when={imgUrl.loading && !displayUrl()}>
                <div style="display:flex;flex-direction:column;align-items:center;gap:8px">
                  <span class="spinner" />
                  <Show when={props.encrypted}>
                    <span class="muted" style="font-size:11px">Decrypting…</span>
                  </Show>
                </div>
              </Show>
              <Show when={imgUrl.error && !displayUrl()}>
                <PreviewErrorCard err={imgUrl.error} />
              </Show>
              <Show when={displayUrl()}>
                <img class="preview-thumb" src={imgSrc()}
                     onClick={() => setExpanded(true)} style="cursor:zoom-in" />
                <Show when={imgSwitching()}>
                  <span class="spinner" style="position:absolute;top:8px;right:8px" />
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
                <div class="preview-editor" style="position:relative">
                  <CodeEditor value={textContent()} ext={extOf(displayText()!.key)} readOnly dark={resolvedTheme() === "dark"} />
                  <Show when={textSwitching()}>
                    <span class="spinner" style="position:absolute;top:8px;right:8px" />
                  </Show>
                </div>
              </Show>
            </Show>
            <Show when={!textAutoLoad()}>
              <div class="preview-img-area" style="position:relative">
                <Show when={!loadRequested() && !displayText()}>
                  <Show when={tooBig()}
                        fallback={
                          <button class="btn-secondary" onClick={() => setLoadRequested(true)}
                                  style="display:flex;align-items:center;gap:8px">
                            <IconEye size={15} /> Load preview
                          </button>
                        }>
                    <span class="muted" style="font-size:12px">File too large to preview</span>
                  </Show>
                </Show>
                <Show when={loadRequested() && !displayText()}><span class="spinner" /></Show>
                <Show when={displayText()}>
                  <div class="preview-editor" style="width:100%;position:relative">
                    <CodeEditor value={textContent()} ext={extOf(displayText()!.key)} readOnly dark={resolvedTheme() === "dark"} />
                    <Show when={textSwitching()}>
                      <span class="spinner" style="position:absolute;top:8px;right:8px" />
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
            <div class="muted" style="font-size:12px;text-align:center;padding:20px">
              Binary content · {formatBytes(props.obj.size)}
            </div>
          </Show>

          <MetaList obj={props.obj} />

          <div class="btn-row">
            <button class="btn-secondary" style="flex:1" onClick={props.onCopyLink}>Copy link</button>
            <button class="btn-primary" style="flex:1" onClick={props.onDownload}>Download</button>
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
