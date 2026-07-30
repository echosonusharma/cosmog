import { createSignal, Show } from "solid-js";
import { RequestLogs } from "./RequestLogs";
import { SystemLog } from "./SystemLog";

type Tab = "requests" | "system";

export default function Logs() {
  const [tab, setTab] = createSignal<Tab>("requests");
  return (
    <div class="view-container">
      {/* tab bar */}
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

      <Show when={tab() === "requests"}><RequestLogs /></Show>
      <Show when={tab() === "system"}><SystemLog /></Show>
    </div>
  );
}
