import { createSignal, Show } from "solid-js";
import { save as saveDialog, open as openDialog } from "@tauri-apps/plugin-dialog";
import { downloadDir, join } from "@tauri-apps/api/path";
import {
  enableBucketEncryption,
  disableBucketEncryption,
  saveEncryptionKeyExport,
  importEncryptionIdentity,
  importEncryptionIdentityFromFile,
} from "../../api/encryption";
import { toast, errMsg } from "../../state/toast";
import { IconKey, IconChevronR, IconChevronD } from "../../utils/icons";
import { pathFromDialog } from "./helpers";

function CodeSnippet(props: { code: string }) {
  const [copied, setCopied] = createSignal(false);
  async function copy() {
    try {
      await navigator.clipboard.writeText(props.code);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard may be blocked in webview; silently ignore */ }
  }
  return (
    <div class="enc-code-snippet-wrap">
      <button
        type="button"
        class="enc-code-snippet-copy"
        onClick={copy}
      >
        {copied() ? "Copied" : "Copy"}
      </button>
      <pre class="enc-code-snippet-pre">{props.code}</pre>
    </div>
  );
}

export function EncryptionModal(props: {
  accountId: string;
  bucket: string;
  enabled: boolean;
  identityPresent: boolean;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [enabling, setEnabling] = createSignal(false);
  const [disabling, setDisabling] = createSignal(false);
  const [exporting, setExporting] = createSignal<false | "picking" | "writing">(false);
  const [importing, setImporting] = createSignal<false | "picking" | "loading">(false);
  const [importText, setImportText] = createSignal("");
  const anyBusy = () => enabling() || disabling() || exporting() !== false || importing() !== false;
  const [err, setErr] = createSignal("");
  const [savedPath, setSavedPath] = createSignal<string | null>(null);
  const [showGuide, setShowGuide] = createSignal(false);
  // After a fresh Enable, the backend returns the generated secret identity
  // once. We surface it here so the user can immediately export a backup file
  // before the modal is closed. Never persisted.
  const [freshSecret, setFreshSecret] = createSignal<string | null>(null);
  const [freshRecipient, setFreshRecipient] = createSignal<string | null>(null);

  // Best-effort scrub of the secret string on any close path. JS strings are
  // immutable and GC'd non-deterministically, but clearing the signal at least
  // drops our own reference so the value can be collected.
  function closeAndScrub() {
    setFreshSecret(null);
    setFreshRecipient(null);
    props.onClose();
  }

  async function pickExportPath(): Promise<string | null> {
    let defaultPath = `cosmog-key-${props.bucket}.txt`;
    try { defaultPath = await join(await downloadDir(), defaultPath); } catch {}
    const sel = await saveDialog({
      defaultPath,
      filters: [{ name: "age identity", extensions: ["txt", "key", "age"] }],
    });
    if (!sel) return null;
    return pathFromDialog(sel);
  }

  async function handleEnable() {
    setEnabling(true); setErr("");
    try {
      const res = await enableBucketEncryption(props.accountId, props.bucket);
      setFreshSecret(res.secret_identity);
      setFreshRecipient(res.public_recipient);
      toast.ok("Encryption enabled", `New uploads to "${props.bucket}" will be encrypted. Save the key before closing this dialog.`);
      props.onChanged();
    } catch (e) { setErr(errMsg(e)); } finally { setEnabling(false); }
  }

  async function handleDisable() {
    setDisabling(true); setErr("");
    try {
      await disableBucketEncryption(props.accountId, props.bucket);
      toast.ok("Encryption disabled", `New uploads to "${props.bucket}" will no longer be encrypted`);
      props.onChanged();
      closeAndScrub();
    } catch (e) { setErr(errMsg(e)); } finally { setDisabling(false); }
  }

  async function handleExport() {
    setErr("");
    setExporting("picking");
    let dest: string | null;
    try {
      dest = await pickExportPath();
      if (!dest) { setExporting(false); return; }
    } catch (e) { setErr(errMsg(e)); setExporting(false); return; }

    setExporting("writing");
    try {
      await saveEncryptionKeyExport(props.accountId, props.bucket, dest);
      setSavedPath(dest);
      toast.ok("Key saved", `Encryption key for "${props.bucket}" written to ${dest}`);
    } catch (e) { setErr(errMsg(e)); } finally { setExporting(false); }
  }

  async function handleImportFromFile() {
    setErr("");
    setImporting("picking");
    let src: string | null;
    try {
      const sel = await openDialog({
        multiple: false,
        filters: [{ name: "age identity", extensions: ["txt", "key", "age"] }],
      });
      if (!sel) { setImporting(false); return; }
      src = pathFromDialog(sel as string);
    } catch (e) { setErr(errMsg(e)); setImporting(false); return; }

    setImporting("loading");
    try {
      await importEncryptionIdentityFromFile(props.accountId, props.bucket, src);
      toast.ok("Key loaded", `Encryption key for "${props.bucket}" imported from file`);
      props.onChanged();
      closeAndScrub();
    } catch (e) { setErr(errMsg(e)); } finally { setImporting(false); }
  }

  async function handleImportFromText() {
    setErr("");
    const text = importText().trim();
    if (!text) { setErr("Paste the key text (starts with AGE-SECRET-KEY-) before importing."); return; }
    setImporting("loading");
    try {
      await importEncryptionIdentity(props.accountId, props.bucket, text);
      toast.ok("Key loaded", `Encryption key for "${props.bucket}" imported`);
      setImportText("");
      props.onChanged();
      closeAndScrub();
    } catch (e) { setErr(errMsg(e)); } finally { setImporting(false); }
  }

  function exportLabel() {
    const s = exporting();
    if (s === "picking") return "Waiting for file dialog…";
    if (s === "writing") return "Saving…";
    return savedPath() ? "Save key again" : "Save key to file";
  }

  return (
    <div class="modal-backdrop" onClick={closeAndScrub}>
      <div class="modal enc-modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">Bucket encryption: {props.bucket}</div>

        <Show when={!props.enabled && !freshSecret()}>
          <div class="modal-sub">
            Turn on encryption to lock every file with a secret key before it leaves your computer. Nobody without the key can read them, not even the storage provider. Downloads and previews in cosmog unlock the files automatically.
          </div>
          <div class="modal-sub enc-notice">
            After you click Enable, cosmog shows the key file <strong>once</strong>. Save it immediately somewhere safe (password manager, encrypted drive). If you lose the key, encrypted files can never be opened again.
          </div>
          <Show when={err()}><div class="status-msg err enc-status-msg-top">{err()}</div></Show>
          <div class="btn-row mt-3">
            <button class="btn-secondary btn-half" onClick={closeAndScrub}>Cancel</button>
            <button class="btn-primary btn-half" disabled={enabling()} onClick={handleEnable}>
              {enabling() ? "Generating…" : "Enable encryption"}
            </button>
          </div>
        </Show>

        <Show when={freshSecret()}>
          <div class="modal-sub enc-notice danger">
            <strong class="block">Save your key now</strong>
            This key is the only way to open your encrypted files. Anyone who has it can read them. Cosmog will not show it again after this dialog closes.
          </div>
          <div class="modal-sub enc-field-label">Bucket ID (safe to share)</div>
          <code class="enc-code-block">{freshRecipient()}</code>
          <div class="modal-sub enc-field-label">Secret key (keep private)</div>
          <code class="enc-code-block">{freshSecret()}</code>
          <Show when={err()}><div class="status-msg err mb-2">{err()}</div></Show>
          <button class="btn-primary enc-primary-action"
                  disabled={anyBusy()} onClick={handleExport}>
            <IconKey size={14} /> {exportLabel()}
          </button>
          <Show when={savedPath()}>
            <div class="modal-sub enc-saved-note">
              Saved to <code>{savedPath()}</code>
            </div>
          </Show>
          <button class="btn-secondary enc-close-full" onClick={closeAndScrub}>
            I saved the key, close
          </button>
        </Show>

        <Show when={props.enabled && !freshSecret() && !props.identityPresent}>
          <div class="modal-sub enc-notice danger">
            <strong class="block">Key missing on this device</strong>
            This bucket is encrypted, but the key is not on this device. You need it to open your files. Load the key file you saved earlier.
          </div>

          <button class="btn-primary enc-primary-action"
                  disabled={anyBusy()} onClick={handleImportFromFile}>
            <IconKey size={14} />
            {importing() === "picking" ? "Waiting for file dialog…"
              : importing() === "loading" ? "Loading…"
              : "Load key from file"}
          </button>

          <div class="modal-sub enc-import-textarea-label">Or paste the key text</div>
          <textarea
            class="enc-import-textarea"
            placeholder="AGE-SECRET-KEY-1..."
            value={importText()}
            onInput={(e) => setImportText(e.currentTarget.value)}
          />
          <Show when={err()}><div class="status-msg err enc-status-msg-y">{err()}</div></Show>
          <div class="btn-row mt-3">
            <button class="btn-secondary btn-half" onClick={closeAndScrub}>Cancel</button>
            <button class="btn-primary btn-half" disabled={anyBusy() || !importText().trim()} onClick={handleImportFromText}>
              Load pasted key
            </button>
          </div>
        </Show>

        <Show when={props.enabled && !freshSecret() && props.identityPresent}>
          <div class="modal-sub enc-notice info-accent">
            Encryption is on. Files in this bucket are locked with a key on this device before they leave your computer. Nobody without the key file can read them, not even the storage provider.
          </div>

          <div class="modal-sub mb-2">
            Save a copy of the key file. If this device is lost or reset, the key file is the only way to open the files again. Keep it somewhere safe (password manager, encrypted drive). Anyone with this file can read every file in the bucket, so do not share it.
          </div>

          <Show when={err()}><div class="status-msg err mb-2">{err()}</div></Show>

          <button class="btn-secondary enc-secondary-full"
                  disabled={anyBusy()} onClick={handleExport}>
            <IconKey size={14} /> {exportLabel()}
          </button>

          <Show when={savedPath()}>
            <div class="modal-sub enc-saved-note">
              Saved to <code>{savedPath()}</code>
            </div>
          </Show>

          <button
            type="button"
            class="enc-guide-toggle"
            onClick={() => setShowGuide((v) => !v)}
            aria-expanded={showGuide()}
          >
            <Show when={showGuide()} fallback={<IconChevronR size={14} />}>
              <IconChevronD size={14} />
            </Show>
            <span class="enc-guide-toggle-label">External decryption guide</span>
            <span class="enc-guide-toggle-hint">age · rage · pyrage</span>
          </button>

          <Show when={showGuide()}>
            <div class="enc-guide-body">
              <div class="enc-guide-heading"><strong>Format</strong></div>
              <div class="enc-guide-note">
                Standard age v1 (streaming ChaCha20-Poly1305, 64 KiB chunks). Any age-compatible tool decrypts it. S3 user-metadata:
                <code>cosmog-encrypted=1</code>,
                <code>cosmog-format=age-v1</code>,
                <code>cosmog-recipient=age1…</code>.
              </div>

              <div class="enc-guide-heading"><strong>Decrypt with the age CLI</strong></div>
              <div class="enc-guide-note tight">
                Install: <code>brew install age</code>, <code>apt install age</code>, or download from
                <span class="enc-guide-note-inline-span">age-encryption.org</span>.
              </div>
              <CodeSnippet code={`# Fetch the object (any S3 client works).
aws s3 cp s3://${props.bucket}/<key> ciphertext.age

# Decrypt with the exported identity file.
age -d -i cosmog-key-${props.bucket}.txt ciphertext.age > plaintext`} />

              <div class="enc-guide-heading mt"><strong>Decrypt in Python (pyrage)</strong></div>
              <CodeSnippet code={`pip install pyrage

import pyrage

with open("cosmog-key-${props.bucket}.txt") as f:
    secret = next(l.strip() for l in f if l.startswith("AGE-SECRET-KEY"))
ident = pyrage.x25519.Identity.from_str(secret)

with open("ciphertext.age", "rb") as fin, open("plaintext", "wb") as fout:
    fout.write(pyrage.decrypt(fin.read(), [ident]))`} />
            </div>
          </Show>

          <div class="enc-danger-zone">
            <div class="enc-notice danger">
              <strong class="block">Danger zone</strong>
              Disabling removes the key from this device. Files already encrypted stay locked forever unless you have the key file saved. Save it first if you still need access.
            </div>
            <div class="btn-row">
              <button class="btn-secondary btn-half" onClick={closeAndScrub}>Close</button>
              <button
                class="enc-disable-btn"
                disabled={anyBusy()}
                onClick={handleDisable}
              >
                {disabling() ? "Disabling…" : "Disable encryption"}
              </button>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
