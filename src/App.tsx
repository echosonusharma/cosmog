import { batch, createSignal, Show, ErrorBoundary } from "solid-js";
import { listAccounts } from "./api/accounts";
import Onboarding from "./routes/Onboarding";
import MainApp from "./routes/MainApp";
import Titlebar from "./routes/Titlebar";
import { ConfirmHost } from "./state/confirm";
import { accounts, setAccounts, setBrowseState, setCurrentView, type View } from "./state/app";
import { parseWireError, isCredentialError, isNetworkError } from "./utils/errors";

// Boot seeds data; reload failures rethrow to the boundary below.
export default function App(props: { bootError: unknown }) {
  if (props.bootError) throw props.bootError;
  const [loadError, setLoadError] = createSignal<unknown>(null);
  const pendingErr = loadError();
  if (pendingErr) throw pendingErr;

  async function reloadAccounts() {
    try {
      setAccounts(await listAccounts());
    } catch (e) {
      setLoadError(e);
    }
  }

  function recoverFromError(reset: () => void, targetView: View = "browse") {
    const accs = accounts();
    if (accs.length === 0) {
      reset();
      void reloadAccounts();
    } else {
      // batch() flushes signal writes atomically with reset(), so when the ErrorBoundary re-renders,
      // browseState.bucket is already null — Browse never remounts with a stale bucket and re-throw.
      batch(() => {
        setBrowseState({ bucket: null, prefix: "" });
        setCurrentView(targetView);
        reset();
      });
    }
  }

  return (
    <div class="cosmog-root">
      <Titlebar />
      <div class="cosmog-body">
        <ErrorBoundary fallback={(err, reset) => {
          const { code, message } = parseWireError(err);
          const credErr = isCredentialError(code);
          const netErr  = isNetworkError(code);
          const title   = credErr ? "Credentials not found" : netErr ? "Service unreachable" : "Something went wrong";
          return (
            <div class="center-fill">
              <div class="err-popup err-popup-boot">
                <div class="err-popup-header">
                  <span class="err-popup-title">{title}</span>
                </div>
                <p class="err-popup-msg">{message}</p>
                {netErr && <p class="err-popup-msg err-popup-hint">Check that the endpoint is running and reachable, then try again.</p>}
                <div class="err-popup-actions">
                  <Show when={accounts().length > 0}
                        fallback={
                          <button class="btn-primary text-xs"
                                  onClick={() => recoverFromError(reset)}>
                            Add account
                          </button>
                        }>
                    <button class="btn-secondary text-xs"
                            onClick={() => recoverFromError(reset, "settings")}>
                      Settings
                    </button>
                    <button class="btn-primary text-xs"
                            onClick={() => recoverFromError(reset, credErr ? "settings" : "browse")}>
                      Back to accounts
                    </button>
                  </Show>
                </div>
              </div>
            </div>
          );
        }}>
          <Show when={accounts().length > 0}
                fallback={<Onboarding onDone={reloadAccounts} />}>
            <MainApp />
          </Show>
        </ErrorBoundary>
      </div>
      <ConfirmHost />
    </div>
  );
}
