import { Show } from "solid-js";
import { IconX } from "../../../utils/icons";

export function Lightbox(props: {
  open: boolean;
  src: string;
  alt: string;
  onClose: () => void;
}) {
  return (
    <Show when={props.open}>
      <div class="lightbox" onClick={props.onClose}>
        <img src={props.src} alt={props.alt} onClick={(e) => e.stopPropagation()} />
        <button class="lightbox-close icon-btn" onClick={props.onClose}><IconX size={20} /></button>
      </div>
    </Show>
  );
}
