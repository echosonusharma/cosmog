import { createSignal, Show } from "solid-js";
import { putObjectText } from "../../api/objects";
import { focusEnd, objectExists, prefixToPath } from "./newItem";
import { errMsg } from "../../state/toast";
import { extOf } from "../../utils/fmt";
import { Select } from "../../utils/Select";
import { TEXT_TYPES, textTypeForName, withExt } from "../../utils/textTypes";
import { mimeTypeSchema, objectKeySchema, parseSchema } from "../../validation";

export function NewFileModal(props: {
  accountId: string;
  bucket: string;
  prefix: string;
  onClose: () => void;
  onCreated: (key: string, mime: string) => void;
}) {
  const [name, setName] = createSignal(prefixToPath(props.prefix));
  const [ext, setExt] = createSignal("txt");
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  // empty = follow the selected type
  const [customMime, setCustomMime] = createSignal("");
  const typeMime = () => TEXT_TYPES.find((t) => t.ext === ext())?.mime ?? "text/plain";
  const mime = () => customMime().trim() || typeMime();

  function onNameInput(v: string) {
    setName(v);
    setErr("");
    const t = textTypeForName(v);
    if (t) setExt(t.ext);
    else if (extOf(v.split("/").pop() ?? "")) setExt("txt");
  }

  function onTypeChange(v: string) {
    setExt(v);
    setCustomMime("");
    setName(withExt(name(), v));
    setErr("");
  }

  async function submit() {
    if (busy()) return;
    const result = parseSchema(objectKeySchema, name());
    if (!result.success) { setErr(result.message); return; }
    const key = result.data;
    if (key.endsWith("/")) { setErr("File name is required"); return; }
    const mimeResult = parseSchema(mimeTypeSchema, mime());
    if (!mimeResult.success) { setErr(mimeResult.message); return; }
    const contentType = mimeResult.data;
    setBusy(true);
    try {
      const exists = await objectExists(props.accountId, props.bucket, key);
      if (exists) { setErr("A file with this name already exists"); return; }
      await putObjectText(props.accountId, props.bucket, key, "", contentType);
      props.onCreated(key, contentType);
      props.onClose();
    } catch (e) {
      setErr(errMsg(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div class="modal-backdrop" onClick={props.onClose}>
      <div class="modal new-file-modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">New file</div>
        <div class="modal-sub modal-sub-path-label">Type</div>
        <Select
          value={ext()}
          options={TEXT_TYPES.map((t) => ({ value: t.ext, label: `${t.label} (.${t.ext})` }))}
          onChange={onTypeChange}
        />
        <div class="modal-sub modal-sub-path-label">Path</div>
        <input class="field" placeholder="path/to/file.txt" autocapitalize="off" autocorrect="off" spellcheck={false}
               value={name()}
               onInput={(e) => onNameInput(e.currentTarget.value)}
               onKeyDown={(e) => e.key === "Enter" && submit()}
               ref={focusEnd} />
        <div class="modal-sub modal-sub-path-label">Content type</div>
        <input class="field" placeholder={typeMime()} autocapitalize="off" autocorrect="off" spellcheck={false} value={customMime()}
               onInput={(e) => { setCustomMime(e.currentTarget.value); setErr(""); }}
               onKeyDown={(e) => e.key === "Enter" && submit()} />
        <Show when={err()}><div class="status-msg err">{err()}</div></Show>
        <div class="btn-row mt-3">
          <button class="btn-secondary btn-half" onClick={props.onClose}>Cancel</button>
          <button class="btn-primary btn-half" disabled={busy() || !name().trim().replace(/\//g, "")} onClick={submit}>
            {busy() ? "Creating…" : "Create"}
          </button>
        </div>
      </div>
    </div>
  );
}
