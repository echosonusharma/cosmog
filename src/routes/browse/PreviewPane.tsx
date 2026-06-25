import { createSignal, createResource, Show, createEffect } from "solid-js";
import { presignGet, previewObject, putObjectText } from "../../api/objects";
import { notify } from "../../utils/notify";
import { errMsg } from "../../state/toast";
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

// ── preview pane ──────────────────────────────────────────────────────────────

export function PreviewPane(props: { obj: CachedObjectMeta; onClose: () => void; onDownload: () => void; onCopyLink: () => void; }) {
  const ct = () => props.obj.content_type ?? "";
  const ext = () => extOf(props.obj.basename);
  const isImage = () => ct().startsWith("image/") || IMAGE_EXTS.has(ext());
  const isSheet = () => SHEET_EXTS.has(ext());
  const isText = () => !isSheet() && (ct().startsWith("text/") || ct().includes("json") || ct().includes("xml") || ct().includes("javascript") || TEXT_EXTS.has(ext()));

  const [loadRequested, setLoadRequested] = createSignal(false);
  const [expanded, setExpanded] = createSignal(false);
  const [editOpen, setEditOpen] = createSignal(false);
  const tooBig = () => props.obj.size > 10 * 1024 * 1024;
  const imageAutoLoad = () => isImage();
  const textAutoLoad = () => isText() && props.obj.size <= 512 * 1024;

  // Reset on object change
  createEffect(() => { void props.obj.key; setLoadRequested(false); setExpanded(false); setEditOpen(false); });

  // Images: presigned URL for raster images; data URL for SVG (avoids CSP/content-type issues)
  const [imgUrl] = createResource(
    () => (imageAutoLoad() || (isImage() && loadRequested()) ? { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket, x: ext() } : null),
    async ({ a, b, k, x }) => {
      if (x === "svg") {
        const r = await previewObject(a, b, k, 2 * 1024 * 1024);
        const base64 = btoa(new Uint8Array(r.bytes).reduce((s, byte) => s + String.fromCharCode(byte), ""));
        return `data:image/svg+xml;base64,${base64}`;
      }
      return presignGet(a, b, k);
    },
  );

  // Text: fetch bytes via backend
  const [loadedKey, setLoadedKey] = createSignal<string | null>(null);
  const textShouldFetch = () => isText() && !isImage() && (textAutoLoad() || loadRequested());
  const [preview, { refetch: refetchPreview }] = createResource(
    () => (textShouldFetch() ? { k: props.obj.key, a: props.obj.account_id, b: props.obj.bucket } : null),
    async ({ a, b, k }) => { try { const r = await previewObject(a, b, k, 256 * 1024); return r; } finally { setLoadedKey(k); } },
  );
  const cur = () => {
    if (preview.loading || loadedKey() !== props.obj.key) return null;
    return preview() ?? null;
  };
  function textContent() {
    const p = cur(); if (!p) return "";
    try { return new TextDecoder().decode(new Uint8Array(p.bytes)); }
    catch { return ""; }
  }

  const imgSrc = () => imgUrl.latest ?? "";

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
          <Show when={isImage() && imgUrl.latest}>
            <button class="icon-btn" title="Expand" onClick={() => setExpanded(true)}><IconArrowUpLine size={15} /></button>
          </Show>
          <Show when={isText() && cur()}>
            <button class="icon-btn" title="Edit" onClick={() => setEditOpen(true)}><IconEdit size={15} /></button>
          </Show>
          <button class="icon-btn" onClick={props.onClose} title="Close"><IconX size={16} /></button>
        </div>
        <div class="preview-body">
          <Show when={preview.error}>
            <div class="status-msg err">Preview failed: {errMsg(preview.error)}</div>
          </Show>

          {/* Image preview via presigned URL */}
          <Show when={isImage()}>
            <div class="preview-img-area">
              <Show when={imgUrl.loading}>
                <span class="spinner" />
              </Show>
              <Show when={imgUrl.error}>
                <span class="muted" style="font-size:12px">Failed to load preview</span>
              </Show>
              <Show when={imgUrl.latest}>
                <img class="preview-thumb" src={imgSrc()}
                     onClick={() => setExpanded(true)} style="cursor:zoom-in" />
              </Show>
            </div>
          </Show>

          <Show when={isText() && !isImage()}>
            <Show when={textAutoLoad()}>
              <Show when={preview.loading}>
                <div class="loading-row"><span class="spinner" /> Loading…</div>
              </Show>
              <Show when={cur()}>
                <div class="preview-editor">
                  <CodeEditor value={textContent()} ext={ext()} readOnly dark={resolvedTheme() === "dark"} />
                </div>
              </Show>
            </Show>
            <Show when={!textAutoLoad()}>
              <div class="preview-img-area">
                <Show when={!loadRequested()}>
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
                <Show when={loadRequested() && !cur()}><span class="spinner" /></Show>
                <Show when={!!cur()}>
                  <div class="preview-editor" style="width:100%">
                    <CodeEditor value={textContent()} ext={ext()} readOnly dark={resolvedTheme() === "dark"} />
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
        open={expanded() && !!imgUrl.latest}
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
