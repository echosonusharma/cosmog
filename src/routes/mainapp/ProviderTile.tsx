import { detectProvider } from "../../providers";

// ── provider tile ─────────────────────────────────────────────────────────────

export function ProviderTile(props: {
  account: { endpoint?: string | null; region?: string };
  size?: "small" | "normal" | "large";
}) {
  const def = () => detectProvider(props.account);
  const sz = props.size ?? "normal";
  return (
    <span
      class={`provider-icon-tile${sz === "small" ? " small" : sz === "large" ? " large" : ""}`}
      style={{ background: def().color }}
      title={def().label}
    >
      <img src={def().iconUrl} alt={def().label} style="width:65%;height:65%;object-fit:contain;filter:brightness(0) invert(1)" />
    </span>
  );
}
