import { createEffect, createMemo, Show, ErrorBoundary } from "solid-js";
import { accounts, browseState, setBrowseState, setCurrentView } from "../state/app";
import { AccountSelector } from "./browse/AccountSelector";
import { BucketGrid } from "./browse/BucketGrid";
import { ObjectBrowser } from "./browse/ObjectBrowser";
import { isCredentialError, parseWireError } from "../utils/errors";

// ── root ──────────────────────────────────────────────────────────────────────

export default function Browse(props: { defaultDownloadDir: string }) {
  createEffect(() => {
    if (accounts().length === 1 && !browseState.accountId) {
      setBrowseState("accountId", accounts()[0].id);
    }
  });

  const accountName = () =>
    accounts().find((a) => a.id === browseState.accountId)?.name ?? "Account";

  // createMemo updates synchronously in the same reactive batch as the store —
  // no post-render effect lag, so ObjectBrowser never sees a stale bucket.
  let _lastAccountId = browseState.accountId ?? "";
  let _lastBucket    = browseState.bucket    ?? "";
  const stableAccountId = createMemo(() => {
    if (browseState.accountId) _lastAccountId = browseState.accountId;
    return _lastAccountId;
  });
  const stableBucket = createMemo(() => {
    if (browseState.bucket) _lastBucket = browseState.bucket;
    return _lastBucket;
  });

  // Mount guards: don't create ObjectBrowser/BucketGrid until at least one
  // valid value has been seen — avoids invoke() calls with empty strings.
  let _everHadAccount = !!browseState.accountId;
  const everHadAccount = createMemo(() => { if (browseState.accountId) _everHadAccount = true; return _everHadAccount; });

  const hasAccount = () => !!browseState.accountId;
  const hasBucket  = () => !!browseState.bucket;

  return (
    <div class="view-container">
      <Show when={!hasAccount()}>
        <AccountSelector />
      </Show>

      <Show when={everHadAccount()}>
        <div class="view-slot" style={{ display: !hasBucket() ? "flex" : "none" }}>
          <Show when={stableAccountId()} keyed>
            {(accountId) => (
              <ErrorBoundary fallback={(err, reset) => {
                const { code, message } = parseWireError(err);
                return (
                  <div style="display:flex;align-items:center;justify-content:center;height:100%;width:100%">
                    <div class="err-popup" style="position:static;box-shadow:none">
                      <div class="err-popup-header"><span class="err-popup-title">{isCredentialError(code) ? "Credentials not found" : "Something went wrong"}</span></div>
                      <p class="err-popup-msg">{message}</p>
                      <div class="err-popup-actions">
                        <button class="btn-secondary" style="font-size:12px" onClick={() => reset()}>Dismiss</button>
                        <button class="btn-primary" style="font-size:12px" onClick={() => { setCurrentView("settings"); reset(); }}>Settings</button>
                      </div>
                    </div>
                  </div>
                );
              }}>
                <BucketGrid accountId={accountId} accountName={accountName()} />
              </ErrorBoundary>
            )}
          </Show>
        </div>
        <Show when={hasBucket()}>
          <div class="view-slot" style="display:flex;flex:1;min-height:0">
            <ErrorBoundary fallback={(err, reset) => {
              const { code, message } = parseWireError(err);
              return (
                <div style="display:flex;align-items:center;justify-content:center;height:100%;width:100%">
                  <div class="err-popup" style="position:static;box-shadow:none">
                    <div class="err-popup-header"><span class="err-popup-title">{isCredentialError(code) ? "Credentials not found" : "Something went wrong"}</span></div>
                    <p class="err-popup-msg">{message}</p>
                    <div class="err-popup-actions">
                      <button class="btn-secondary" style="font-size:12px" onClick={() => { setBrowseState({ bucket: null, prefix: "" }); reset(); }}>Back to buckets</button>
                      <button class="btn-primary" style="font-size:12px" onClick={() => { setCurrentView("settings"); reset(); }}>Settings</button>
                    </div>
                  </div>
                </div>
              );
            }}>
              <ObjectBrowser
                accountId={stableAccountId()}
                accountName={accountName()}
                bucket={stableBucket()}
                prefix={browseState.prefix}
                defaultDownloadDir={props.defaultDownloadDir}
              />
            </ErrorBoundary>
          </div>
        </Show>
      </Show>
    </div>
  );
}
