// Single shared loader. SVG arc with round caps and plain rotation;
// the old .spinner border+clip wipe cut notches mid-cycle on thin rings.
export default function Spinner(props: { size?: number; class?: string }) {
  const size = () => props.size ?? 13;
  return (
    <svg
      class={`spinner-svg${props.class ? ` ${props.class}` : ""}`}
      width={size()}
      height={size()}
      viewBox="0 0 32 32"
      role="status"
      aria-label="Loading"
    >
      <circle
        cx="16" cy="16" r="13" fill="none" stroke="currentColor"
        stroke-width="4" stroke-linecap="round" stroke-dasharray="27 55"
      />
    </svg>
  );
}
