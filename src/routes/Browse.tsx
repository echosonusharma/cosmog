import { createEffect, createMemo, Show, ErrorBoundary } from "solid-js";
import { accounts, browseState, setBrowseState, setCurrentView } from "../state/app";
import { AccountSelector } from "./browse/AccountSelector";
import { BucketGrid } from "./browse/BucketGrid";
import { ObjectBrowser } from "./browse/ObjectBrowser";
import { isCredentialError, isNetworkError, parseWireError } from "../utils/errors";

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
    // Reset when account changes so a previous account's bucket never leaks
    // into the new account's ObjectBrowser instance.
    if (browseState.accountId !== _lastAccountId) _lastBucket = "";
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
        <div class="view-slot" classList={{ hidden: hasBucket() }}>
          <Show when={stableAccountId()} keyed>
            {(accountId) => (
              <ErrorBoundary fallback={(err, reset) => {
                const { code, message } = parseWireError(err);
                const credErr = isCredentialError(code);
                const netErr  = isNetworkError(code);
                const title   = credErr ? "Credentials not found" : netErr ? "Service unreachable" : "Something went wrong";
                return (
                  <div class="browse-err-fallback">
                    <div class="err-popup err-popup-boot">
                      <div class="err-popup-header"><span class="err-popup-title">{title}</span></div>
                      <p class="err-popup-msg">{message}</p>
                      {netErr && <p class="err-popup-msg err-popup-hint">Check that the endpoint is running and reachable, then try again.</p>}
                      <div class="err-popup-actions">
                        <button class="btn-secondary btn-xs" onClick={() => reset()}>Dismiss</button>
                        <button class="btn-primary btn-xs" onClick={() => { setCurrentView("settings"); reset(); }}>Settings</button>
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
          <div class="view-slot view-slot-fill">
            <ErrorBoundary fallback={(err, reset) => {
              const { code, message } = parseWireError(err);
              const credErr = isCredentialError(code);
              const netErr  = isNetworkError(code);
              const title   = credErr ? "Credentials not found" : netErr ? "Service unreachable" : "Something went wrong";
              return (
                <div class="browse-err-fallback">
                  <div class="err-popup err-popup-boot">
                    <div class="err-popup-header"><span class="err-popup-title">{title}</span></div>
                    <p class="err-popup-msg">{message}</p>
                    {netErr && <p class="err-popup-msg err-popup-hint">Check that the endpoint is running and reachable, then try again.</p>}
                    <div class="err-popup-actions">
                      <button class="btn-secondary btn-xs" onClick={() => { setBrowseState({ bucket: null, prefix: "" }); reset(); }}>Back to buckets</button>
                      <button class="btn-primary btn-xs" onClick={() => { setCurrentView("settings"); reset(); }}>Settings</button>
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
