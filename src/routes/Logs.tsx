import { createSignal } from "solid-js";
import { RequestLogs } from "./RequestLogs";
import { SystemLog } from "./SystemLog";

type Tab = "requests" | "system";

export default function Logs() {
  const [tab, setTab] = createSignal<Tab>("requests");
  return (
    <div class="view-container">
      <div class="logs-header logs-tabbar">
        <h2 class="logs-tabbar-title">Logs</h2>
        <button
          class={`logs-tab${tab() === "requests" ? " active" : ""}`}
          onClick={() => setTab("requests")}
        >
          API Requests
        </button>
        <button
          class={`logs-tab${tab() === "system" ? " active" : ""}`}
          onClick={() => setTab("system")}
        >
          System
        </button>
      </div>

      <div class="logs-panels">
        <div class="logs-panel" classList={{ hidden: tab() !== "requests" }}>
          <RequestLogs active={tab() === "requests"} />
        </div>
        <div class="logs-panel" classList={{ hidden: tab() !== "system" }}>
          <SystemLog active={tab() === "system"} />
        </div>
      </div>
    </div>
  );
}
