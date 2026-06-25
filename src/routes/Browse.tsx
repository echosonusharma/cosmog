import { createEffect, createMemo, Show } from "solid-js";
import { accounts, browseState, setBrowseState } from "../state/app";
import { AccountSelector } from "./browse/AccountSelector";
import { BucketGrid } from "./browse/BucketGrid";
import { ObjectBrowser } from "./browse/ObjectBrowser";

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
  let _everHadBucket  = !!browseState.bucket;
  const everHadAccount = createMemo(() => { if (browseState.accountId) _everHadAccount = true; return _everHadAccount; });
  const everHadBucket  = createMemo(() => { if (browseState.bucket)    _everHadBucket  = true; return _everHadBucket; });

  const hasAccount = () => !!browseState.accountId;
  const hasBucket  = () => !!browseState.bucket;

  return (
    <div class="view-container">
      <Show when={!hasAccount()}>
        <AccountSelector />
      </Show>

      <Show when={everHadAccount()}>
        <div class="view-slot" style={{ display: !hasBucket() ? "flex" : "none" }}>
          <BucketGrid accountId={stableAccountId()} accountName={accountName()} />
        </div>
        <Show when={everHadBucket()}>
          <div class="view-slot" style={{ display: hasBucket() ? "flex" : "none" }}>
            <ObjectBrowser
              accountId={stableAccountId()}
              accountName={accountName()}
              bucket={stableBucket()}
              prefix={browseState.prefix}
              defaultDownloadDir={props.defaultDownloadDir}
            />
          </div>
        </Show>
      </Show>
    </div>
  );
}
