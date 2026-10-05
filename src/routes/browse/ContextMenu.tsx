import { Show } from "solid-js";
import {
  IconDownload, IconLink, IconTrash, IconEdit, IconEye,
  IconPlus, IconDoc, IconUpload, IconChevronR, IconActivity,
} from "../../utils/icons";
import { navigateToPrefix } from "../../state/app";
import type { CachedObjectMeta } from "../../types";
import { t } from "../../i18n";

export type CtxMenu =
  | { kind: "file"; x: number; y: number; obj: CachedObjectMeta }
  | { kind: "folder"; x: number; y: number; sub: string }
  | { kind: "pane"; x: number; y: number; prefix: string };

export function ContextMenu(props: {
  menu: CtxMenu;
  onClose: () => void;
  onNewFolder: (prefix: string) => void;
  onNewFile: (prefix: string) => void;
  onUploadHere: (prefix: string) => void;
  onDeleteFolder: (sub: string) => void;
  onPreview: (obj: CachedObjectMeta) => void;
  onDownload: (obj: CachedObjectMeta) => void;
  onCopyLink: (obj: CachedObjectMeta) => void;
  onRename: (obj: CachedObjectMeta) => void;
  onVersionHistory: (obj: CachedObjectMeta) => void;
  onDelete: (obj: CachedObjectMeta) => void;
}) {
  const m = () => props.menu;
  const pane = () => m().kind === "pane" ? m() as { kind: "pane"; x: number; y: number; prefix: string } : null;
  const folder = () => m().kind === "folder" ? m() as { kind: "folder"; x: number; y: number; sub: string } : null;
  const file = () => m().kind === "file" ? m() as { kind: "file"; x: number; y: number; obj: CachedObjectMeta } : null;

  return (
    <div class="context-menu" style={{ left: `${m().x}px`, top: `${m().y}px` }}
         onClick={(e) => e.stopPropagation()}>
      <Show when={pane()}>
        {(p) => (
          <>
            <button class="context-item" onClick={() => { props.onNewFolder(p().prefix); props.onClose(); }}>
              <span class="context-item-icon"><IconPlus size={14} /></span> {t("browse.ctx.newFolderHere")}
            </button>
            <button class="context-item" onClick={() => { props.onNewFile(p().prefix); props.onClose(); }}>
              <span class="context-item-icon"><IconDoc size={14} /></span> {t("browse.ctx.newFileHere")}
            </button>
          </>
        )}
      </Show>
      <Show when={folder()}>
        {(f) => (<>
          <button class="context-item" onClick={() => { navigateToPrefix(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconChevronR size={14} /></span> {t("common.open")}
          </button>
          <button class="context-item" onClick={() => { props.onUploadHere(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconUpload size={14} /></span> {t("browse.ctx.uploadHere")}
          </button>
          <div class="context-sep" />
          <button class="context-item danger" onClick={() => { props.onDeleteFolder(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconTrash size={14} /></span> {t("browse.ctx.deleteFolder")}
          </button>
        </>)}
      </Show>
      <Show when={file()}>
        {(f) => (<>
          <button class="context-item" onClick={() => { props.onPreview(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconEye size={14} /></span> {t("browse.ctx.preview")}
          </button>
          <button class="context-item" onClick={() => { props.onDownload(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconDownload size={14} /></span> {t("common.download")}
          </button>
          <button class="context-item" onClick={() => { props.onCopyLink(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconLink size={14} /></span> {t("browse.ctx.copyLink")}
          </button>
          <button class="context-item" onClick={() => { props.onRename(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconEdit size={14} /></span> {t("browse.ctx.renameMove")}
          </button>
          <button class="context-item" onClick={() => { props.onVersionHistory(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconActivity size={14} /></span> {t("browse.ctx.versionHistory")}
          </button>
          <div class="context-sep" />
          <button class="context-item danger" onClick={() => { props.onDelete(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconTrash size={14} /></span> {t("common.delete")}
          </button>
        </>)}
      </Show>
    </div>
  );
}
