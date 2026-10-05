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
import { confirmDialog } from "../../state/confirm";
import { IconKey, IconChevronR, IconChevronD } from "../../utils/icons";
import { pathFromDialog } from "./helpers";
import { t } from "../../i18n";

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
        {copied() ? t("common.copied") : t("common.copy")}
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
  // After a fresh Enable the backend returns the generated secret identity once;
  // surface it so the user can export a backup before closing. Never persisted.
  const [freshSecret, setFreshSecret] = createSignal<string | null>(null);
  const [freshRecipient, setFreshRecipient] = createSignal<string | null>(null);

  // Best-effort scrub of the secret on any close path: JS strings are immutable
  // and GC'd non-deterministically, but clearing the signal drops our reference.
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
      toast.ok(t("bucketconfig.encryption.enabledTitle"), t("bucketconfig.encryption.enabledDetail", { bucket: props.bucket }));
      props.onChanged();
    } catch (e) { setErr(errMsg(e)); } finally { setEnabling(false); }
  }

  async function handleDisable() {
    // Always prompt (not confirmDestructive): losing the key is unrecoverable.
    const ok = await confirmDialog({
      title: t("bucketconfig.encryption.disableConfirmTitle"),
      body: t(savedPath() ? "bucketconfig.encryption.disableConfirmBody" : "bucketconfig.encryption.disableConfirmBodyUnsaved", { bucket: props.bucket }),
      confirmLabel: t("bucketconfig.encryption.disableConfirmLabel"),
      cancelLabel: t("common.cancel"),
      danger: true,
    });
    if (ok !== true) return;
    setDisabling(true); setErr("");
    try {
      await disableBucketEncryption(props.accountId, props.bucket);
      toast.ok(t("bucketconfig.encryption.disabledTitle"), t("bucketconfig.encryption.disabledDetail", { bucket: props.bucket }));
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
      toast.ok(t("bucketconfig.encryption.keySavedTitle"), t("bucketconfig.encryption.keySavedDetail", { bucket: props.bucket, path: dest }));
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
      toast.ok(t("bucketconfig.encryption.keyLoadedTitle"), t("bucketconfig.encryption.keyLoadedFileDetail", { bucket: props.bucket }));
      props.onChanged();
      closeAndScrub();
    } catch (e) { setErr(errMsg(e)); } finally { setImporting(false); }
  }

  async function handleImportFromText() {
    setErr("");
    const text = importText().trim();
    if (!text) { setErr(t("bucketconfig.encryption.pasteFirst")); return; }
    setImporting("loading");
    try {
      await importEncryptionIdentity(props.accountId, props.bucket, text);
      toast.ok(t("bucketconfig.encryption.keyLoadedTitle"), t("bucketconfig.encryption.keyLoadedDetail", { bucket: props.bucket }));
      setImportText("");
      props.onChanged();
      closeAndScrub();
    } catch (e) { setErr(errMsg(e)); } finally { setImporting(false); }
  }

  function exportLabel() {
    const s = exporting();
    if (s === "picking") return t("bucketconfig.encryption.waitingDialog");
    if (s === "writing") return t("common.saving");
    return savedPath() ? t("bucketconfig.encryption.saveKeyAgain") : t("bucketconfig.encryption.saveKey");
  }

  return (
    <div class="modal-backdrop" onClick={closeAndScrub}>
      <div class="modal enc-modal" onClick={(e) => e.stopPropagation()}>
        <div class="modal-title">{t("bucketconfig.encryption.title", { bucket: props.bucket })}</div>

        <Show when={!props.enabled && !freshSecret()}>
          <div class="modal-sub">
            {t("bucketconfig.encryption.intro")}
          </div>
          <div class="modal-sub enc-notice">
            {t("bucketconfig.encryption.notice")}
          </div>
          <Show when={err()}><div class="status-msg err enc-status-msg-top">{err()}</div></Show>
          <div class="btn-row mt-3">
            <button class="btn-secondary btn-half" onClick={closeAndScrub}>{t("common.cancel")}</button>
            <button class="btn-primary btn-half" disabled={enabling()} onClick={handleEnable}>
              {enabling() ? t("bucketconfig.encryption.generating") : t("bucketconfig.encryption.enable")}
            </button>
          </div>
        </Show>

        <Show when={freshSecret()}>
          <div class="modal-sub enc-notice danger">
            <strong class="block">{t("bucketconfig.encryption.freshTitle")}</strong>
            {t("bucketconfig.encryption.freshBody")}
          </div>
          <div class="modal-sub enc-field-label">{t("bucketconfig.encryption.bucketId")}</div>
          <code class="enc-code-block">{freshRecipient()}</code>
          <div class="modal-sub enc-field-label">{t("bucketconfig.encryption.secretKey")}</div>
          <code class="enc-code-block">{freshSecret()}</code>
          <Show when={err()}><div class="status-msg err mb-2">{err()}</div></Show>
          <button class="btn-primary enc-primary-action"
                  disabled={anyBusy()} onClick={handleExport}>
            <IconKey size={14} /> {exportLabel()}
          </button>
          <Show when={savedPath()}>
            <div class="modal-sub enc-saved-note">
              {t("bucketconfig.encryption.savedTo")} <code>{savedPath()}</code>
            </div>
          </Show>
          <button class="btn-secondary enc-close-full" onClick={closeAndScrub}>
            {t("bucketconfig.encryption.savedClose")}
          </button>
        </Show>

        <Show when={props.enabled && !freshSecret() && !props.identityPresent}>
          <div class="modal-sub enc-notice danger">
            <strong class="block">{t("bucketconfig.encryption.missingTitle")}</strong>
            {t("bucketconfig.encryption.missingBody")}
          </div>

          <button class="btn-primary enc-primary-action"
                  disabled={anyBusy()} onClick={handleImportFromFile}>
            <IconKey size={14} />
            {importing() === "picking" ? t("bucketconfig.encryption.waitingDialog")
              : importing() === "loading" ? t("common.loading")
              : t("bucketconfig.encryption.loadFromFile")}
          </button>

          <div class="modal-sub enc-import-textarea-label">{t("bucketconfig.encryption.orPaste")}</div>
          <textarea
            class="enc-import-textarea"
            placeholder="AGE-SECRET-KEY-1..."
            value={importText()}
            onInput={(e) => setImportText(e.currentTarget.value)}
          />
          <Show when={err()}><div class="status-msg err enc-status-msg-y">{err()}</div></Show>
          <div class="btn-row mt-3">
            <button class="btn-secondary btn-half" onClick={closeAndScrub}>{t("common.cancel")}</button>
            <button class="btn-primary btn-half" disabled={anyBusy() || !importText().trim()} onClick={handleImportFromText}>
              {t("bucketconfig.encryption.loadPasted")}
            </button>
          </div>
        </Show>

        <Show when={props.enabled && !freshSecret() && props.identityPresent}>
          <div class="modal-sub enc-notice info-accent">
            {t("bucketconfig.encryption.onNotice")}
          </div>

          <div class="modal-sub mb-2">
            {t("bucketconfig.encryption.backupHint")}
          </div>

          <Show when={err()}><div class="status-msg err mb-2">{err()}</div></Show>

          <button class="btn-secondary enc-secondary-full"
                  disabled={anyBusy()} onClick={handleExport}>
            <IconKey size={14} /> {exportLabel()}
          </button>

          <Show when={savedPath()}>
            <div class="modal-sub enc-saved-note">
              {t("bucketconfig.encryption.savedTo")} <code>{savedPath()}</code>
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
            <span class="enc-guide-toggle-label">{t("bucketconfig.encryption.guideToggle")}</span>
            <span class="enc-guide-toggle-hint">age · rage · pyrage</span>
          </button>

          <Show when={showGuide()}>
            <div class="enc-guide-body">
              <div class="enc-guide-heading"><strong>{t("bucketconfig.encryption.guideFormat")}</strong></div>
              <div class="enc-guide-note">
                {t("bucketconfig.encryption.guideFormatNote")}
                <code>cosmog-encrypted=1</code>,
                <code>cosmog-format=age-v1</code>,
                <code>cosmog-recipient=age1…</code>.
              </div>

              <div class="enc-guide-heading"><strong>{t("bucketconfig.encryption.guideCli")}</strong></div>
              <div class="enc-guide-note tight">
                {t("bucketconfig.encryption.guideInstall")} <code>brew install age</code>, <code>apt install age</code>, {t("bucketconfig.encryption.guideDownload")}
                <span class="enc-guide-note-inline-span">age-encryption.org</span>.
              </div>
              <CodeSnippet code={`# Fetch the object (any S3 client works).
aws s3 cp s3://${props.bucket}/<key> ciphertext.age

# Decrypt with the exported identity file.
age -d -i cosmog-key-${props.bucket}.txt ciphertext.age > plaintext`} />

              <div class="enc-guide-heading mt"><strong>{t("bucketconfig.encryption.guidePython")}</strong></div>
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
              <strong class="block">{t("bucketconfig.encryption.dangerTitle")}</strong>
              {t("bucketconfig.encryption.dangerBody")}
            </div>
            <div class="btn-row">
              <button class="btn-secondary btn-half" onClick={closeAndScrub}>{t("common.close")}</button>
              <button
                class="enc-disable-btn"
                disabled={anyBusy()}
                onClick={handleDisable}
              >
                {disabling() ? t("bucketconfig.encryption.disabling") : t("bucketconfig.encryption.disable")}
              </button>
            </div>
          </div>
        </Show>
      </div>
    </div>
  );
}
