import { createResource, Show, Suspense, ErrorBoundary } from "solid-js";
import { listAccounts } from "./api/accounts";
import Onboarding from "./routes/Onboarding";
import MainApp from "./routes/MainApp";
import Titlebar from "./routes/Titlebar";
import { ToastStack } from "./state/toast";
import { ConfirmHost } from "./state/confirm";
import { setBrowseState, setCurrentView } from "./state/app";
import { parseWireError, isCredentialError } from "./utils/errors";

export default function App() {
  const [accounts, { refetch }] = createResource(listAccounts);

  function recoverFromError(reset: () => void) {
    const accs = accounts() ?? [];
    reset();
    if (accs.length === 0) {
      // No accounts — force back to onboarding by clearing the list
      refetch();
    } else {
      // Has accounts — clear bucket/prefix selection so user picks again
      setBrowseState({ bucket: null, prefix: "" });
      setCurrentView("browse");
    }
  }

  return (
    <div class="cosmog-root">
      <Titlebar />
      <div class="cosmog-body">
        <ErrorBoundary fallback={(err, reset) => {
          const { code, message } = parseWireError(err);
          const credErr = isCredentialError(code);
          return (
            <div style="display:flex;align-items:center;justify-content:center;height:100%">
              <div class="err-popup" style="position:static;box-shadow:none">
                <div class="err-popup-header">
                  <span class="err-popup-title">
                    {credErr ? "Credentials not found" : "Something went wrong"}
                  </span>
                </div>
                <p class="err-popup-msg">{message}</p>
                <div class="err-popup-actions">
                  <Show when={(accounts() ?? []).length > 0}
                        fallback={
                          <button class="btn-primary" style="font-size:12px"
                                  onClick={() => recoverFromError(reset)}>
                            Add account
                          </button>
                        }>
                    <button class="btn-secondary" style="font-size:12px"
                            onClick={() => { recoverFromError(reset); setCurrentView("settings"); }}>
                      Settings
                    </button>
                    <button class="btn-primary" style="font-size:12px"
                            onClick={() => recoverFromError(reset)}>
                      Back to accounts
                    </button>
                  </Show>
                </div>
              </div>
            </div>
          );
        }}>
          <Suspense>
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
