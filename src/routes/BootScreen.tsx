import type { Accessor } from "solid-js";
import Spinner from "../utils/Spinner";

// Sync first paint; swaps the static #boot-splash without a flash.
export default function BootScreen(props: { stage: Accessor<string> }) {
  return (
    <div class="boot-screen">
      <img class="boot-screen-logo" src="/app-icon.svg" alt="" />
      <div class="boot-screen-name">Cosmog</div>
      <Spinner size={32} />
      <div class="boot-screen-stage">{props.stage()}</div>
    </div>
  );
}
