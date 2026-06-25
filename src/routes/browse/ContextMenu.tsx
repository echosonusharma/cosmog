import { Show } from "solid-js";
import {
  IconDownload, IconLink, IconTrash, IconEdit, IconEye,
  IconPlus, IconUpload, IconChevronR,
} from "../../utils/icons";
import { navigateToPrefix } from "../../state/app";
import type { CachedObjectMeta } from "../../types";

export type CtxMenu =
  | { kind: "file"; x: number; y: number; obj: CachedObjectMeta }
  | { kind: "folder"; x: number; y: number; sub: string }
  | { kind: "pane"; x: number; y: number; prefix: string };

export function ContextMenu(props: {
  menu: CtxMenu;
  onClose: () => void;
  onNewFolder: (prefix: string) => void;
  onUploadHere: (prefix: string) => void;
  onDeleteFolder: (sub: string) => void;
  onPreview: (obj: CachedObjectMeta) => void;
  onDownload: (obj: CachedObjectMeta) => void;
  onCopyLink: (obj: CachedObjectMeta) => void;
  onRename: (obj: CachedObjectMeta) => void;
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
          <button class="context-item" onClick={() => { props.onNewFolder(p().prefix); props.onClose(); }}>
            <span class="context-item-icon"><IconPlus size={14} /></span> New folder here
          </button>
        )}
      </Show>
      <Show when={folder()}>
        {(f) => (<>
          <button class="context-item" onClick={() => { navigateToPrefix(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconChevronR size={14} /></span> Open
          </button>
          <button class="context-item" onClick={() => { props.onUploadHere(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconUpload size={14} /></span> Upload here
          </button>
          <div class="context-sep" />
          <button class="context-item danger" onClick={() => { props.onDeleteFolder(f().sub); props.onClose(); }}>
            <span class="context-item-icon"><IconTrash size={14} /></span> Delete folder
          </button>
        </>)}
      </Show>
      <Show when={file()}>
        {(f) => (<>
          <button class="context-item" onClick={() => { props.onPreview(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconEye size={14} /></span> Preview
          </button>
          <button class="context-item" onClick={() => { props.onDownload(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconDownload size={14} /></span> Download
          </button>
          <button class="context-item" onClick={() => { props.onCopyLink(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconLink size={14} /></span> Copy link
          </button>
          <button class="context-item" onClick={() => { props.onRename(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconEdit size={14} /></span> Rename / Move
          </button>
          <div class="context-sep" />
          <button class="context-item danger" onClick={() => { props.onDelete(f().obj); props.onClose(); }}>
            <span class="context-item-icon"><IconTrash size={14} /></span> Delete
          </button>
        </>)}
      </Show>
    </div>
  );
}
