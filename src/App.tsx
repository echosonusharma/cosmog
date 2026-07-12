import { batch, createResource, Show, Suspense, ErrorBoundary } from "solid-js";
import { listAccounts } from "./api/accounts";
import Onboarding from "./routes/Onboarding";
import MainApp from "./routes/MainApp";
import Titlebar from "./routes/Titlebar";
import { ToastStack } from "./state/toast";
import { ConfirmHost } from "./state/confirm";
import { setBrowseState, setCurrentView, type View } from "./state/app";
import { parseWireError, isCredentialError, isNetworkError } from "./utils/errors";

export default function App() {
  const [accounts, { refetch }] = createResource(listAccounts);

  function recoverFromError(reset: () => void, targetView: View = "browse") {
    const accs = accounts() ?? [];
    if (accs.length === 0) {
      reset();
      refetch();
    } else {
      // batch() ensures all signal writes flush atomically with reset() so that
      // when SolidJS re-evaluates ErrorBoundary children, browseState.bucket is
      // already null — preventing Browse from mounting ObjectBrowser with a
      // stale bucket, which would immediately re-throw the same credential error.
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
            <div style="display:flex;align-items:center;justify-content:center;height:100%">
              <div class="err-popup" style="position:static;box-shadow:none">
                <div class="err-popup-header">
                  <span class="err-popup-title">{title}</span>
                </div>
                <p class="err-popup-msg">{message}</p>
                {netErr && <p class="err-popup-msg" style="opacity:0.65;margin-top:-8px">Check that the endpoint is running and reachable, then try again.</p>}
                <div class="err-popup-actions">
                  <Show when={(accounts() ?? []).length > 0}
                        fallback={
                          <button class="btn-primary" style="font-size:12px"
                                  onClick={() => recoverFromError(reset)}>
                            Add account
                          </button>
                        }>
                    <button class="btn-secondary" style="font-size:12px"
                            onClick={() => recoverFromError(reset, "settings")}>
                      Settings
                    </button>
                    <button class="btn-primary" style="font-size:12px"
                            onClick={() => recoverFromError(reset, credErr ? "settings" : "browse")}>
                      Back to accounts
                    </button>
                  </Show>
                </div>
              </div>
            </div>
          );
        }}>
          <Suspense fallback={<div style="display:flex;align-items:center;justify-content:center;height:100%"><span class="spinner" style="width:32px;height:32px;border-width:3px" /></div>}>
            <Show when={!accounts.loading}>
              <Show
                when={(accounts() ?? []).length > 0}
                fallback={<Onboarding onDone={() => refetch()} />}
              >
                <MainApp />
              </Show>
            </Show>
          </Suspense>
        </ErrorBoundary>
      </div>
      <ToastStack />
      <ConfirmHost />
    </div>
  );
}
