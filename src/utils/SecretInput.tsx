import { createSignal, JSX, splitProps } from "solid-js";
import { t } from "../i18n";
import { IconEye, IconEyeOff } from "./icons";

// Password field with a show/hide toggle. Starts hidden.
export function SecretInput(props: Omit<JSX.InputHTMLAttributes<HTMLInputElement>, "type">) {
  const [shown, setShown] = createSignal(false);
  const [local, rest] = splitProps(props, ["class"]);
  const label = () => (shown() ? t("common.hideSecret") : t("common.showSecret"));
  return (
    <div class="secret-input">
      <input
        {...rest}
        class={`field secret-input-field ${local.class ?? ""}`}
        type={shown() ? "text" : "password"}
        autocomplete="off"
        autocapitalize="off"
        spellcheck={false}
      />
      <button
        type="button"
        class="secret-input-toggle"
        title={label()}
        aria-label={label()}
        aria-pressed={shown()}
        disabled={props.disabled}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => setShown((v) => !v)}
      >
        {shown() ? <IconEyeOff size={16} /> : <IconEye size={16} />}
      </button>
    </div>
  );
}
