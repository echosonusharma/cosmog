import { Show } from "solid-js";
import { describeError } from "./errors";
import { t } from "../i18n";

/** Description, error code and collapsible raw detail. The caller renders the title. */
export function ErrorBody(props: { error: unknown }) {
  const e = () => describeError(props.error);
  return (
    <>
      <p class="err-popup-msg">{e().desc}</p>
      <Show when={e().code}>
        <div class="err-popup-code">
          <span>{t("errors.code")}</span> <code>{e().code}</code>
        </div>
      </Show>
      <Show when={e().code && e().detail}>
        <details class="err-popup-details">
          <summary>{t("errors.details")}</summary>
          <pre>{e().detail}</pre>
        </details>
      </Show>
    </>
  );
}
