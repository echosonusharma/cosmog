import { createSignal, For, Show, onMount, onCleanup } from "solid-js";
import { IconChevronD } from "./icons";
import { t } from "../i18n";

export interface SelectOption {
  value: string;
  label: string;
  icon?: string;
}

export function Select(props: {
  value: string;
  options: SelectOption[];
  placeholder?: string;
  disabled?: boolean;
  style?: string;
  class?: string;
  onChange: (v: string) => void;
}) {
  const [open, setOpen] = createSignal(false);
  let ref!: HTMLDivElement;

  onMount(() => {
    const close = (e: MouseEvent) => { if (!ref.contains(e.target as Node)) setOpen(false); };
    document.addEventListener("mousedown", close);
    onCleanup(() => document.removeEventListener("mousedown", close));
  });

  const current = () => props.options.find((o) => o.value === props.value);
  const label = () => current()?.label ?? props.placeholder ?? t("common.select");

  return (
    <div ref={ref} class={`custom-select${props.disabled ? " disabled" : ""}${open() ? " open" : ""}${props.class ? ` ${props.class}` : ""}`}
         style={typeof props.style === "string" ? props.style : undefined}
         onClick={() => { if (!props.disabled) setOpen((v) => !v); }}>
      <Show when={current()?.icon}>{(src) => <img class="custom-select-icon" src={src()} alt="" />}</Show>
      <span class="custom-select-value">{label()}</span>
      <IconChevronD size={13} class="custom-select-chevron" />
      <Show when={open()}>
        <div class="custom-select-menu" onClick={(e) => e.stopPropagation()}>
          <Show when={props.placeholder}>
            <div class={`custom-select-item placeholder${props.value === "" ? " selected" : ""}`}
                 onClick={() => { props.onChange(""); setOpen(false); }}>
              {props.placeholder}
            </div>
          </Show>
          <For each={props.options}>
            {(opt) => (
              <div class={`custom-select-item${opt.value === props.value ? " selected" : ""}`}
                   onClick={() => { props.onChange(opt.value); setOpen(false); }}>
                <Show when={opt.icon}>{(src) => <img class="custom-select-icon" src={src()} alt="" />}</Show>
                {opt.label}
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
