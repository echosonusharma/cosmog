import { JSX } from "solid-js";

// Each icon below maps to /public/icons/ui/<name>.svg.
// Color comes from CSS `background-color` (set via parent `color` + mask trick).
// Size is set inline.

type IconProps = { size?: number; class?: string; style?: string };

function maskStyle(name: string, size: number, extra?: string) {
  const url = `url(/icons/ui/${name}.svg)`;
  return `width:${size}px;height:${size}px;mask-image:${url};-webkit-mask-image:${url};${extra ?? ""}`;
}

function I(name: string) {
  return (p: IconProps): JSX.Element => (
    <span class={`icon ${p.class ?? ""}`} style={maskStyle(name, p.size ?? 18, p.style)} />
  );
}

export const IconFolder       = I("folder");
export const IconFile         = I("file");
export const IconImage        = I("image");
export const IconVideo        = I("video");
export const IconAudio        = I("music");
export const IconDoc          = I("file-text");
export const IconArchive      = I("archive");
export const IconCode         = I("code-2");
export const IconBrowse       = I("layout-grid");
export const IconSearch       = I("search");
export const IconTransfer     = I("arrow-up-down");
export const IconSettings     = I("settings");
export const IconUp           = I("arrow-up");
export const IconDown         = I("arrow-down");
export const IconUpload       = I("upload");
export const IconDownload     = I("download");
export const IconRefresh      = I("refresh-cw");
export const IconBack         = I("arrow-left");
export const IconX            = I("x");
export const IconCheck        = I("check");
export const IconPlus         = I("plus");
export const IconHome         = I("home");
export const IconChevronR     = I("chevron-right");
export const IconChevronD     = I("chevron-down");
export const IconLink         = I("link-2");
export const IconTrash        = I("trash-2");
export const IconEdit         = I("pencil");
export const IconEye          = I("eye");
export const IconSidebar      = I("panel-left");
export const IconArrowUpLine  = I("arrow-up-from-line");
export const IconArrowDownLine= I("arrow-down-to-line");
export const IconBucket       = I("hard-drive");
export const IconAlertCircle  = I("alert-circle");
export const IconActivity     = I("activity");
export const IconColumns      = I("columns-2");
export const IconList         = I("list");
export const IconTable        = I("table-2");
export const IconBug          = I("bug");

// ── file type mapping ─────────────────────────────────────────────────────────

type Kind = "folder" | "image" | "video" | "audio" | "doc" | "archive" | "code" | "generic";

const EXT_MAP: Record<string, Kind> = {
  png: "image", jpg: "image", jpeg: "image", gif: "image", webp: "image", svg: "image",
  bmp: "image", ico: "image", tif: "image", tiff: "image", avif: "image", heic: "image",
  mp4: "video", mov: "video", mkv: "video", avi: "video", webm: "video", flv: "video", wmv: "video",
  mp3: "audio", wav: "audio", ogg: "audio", flac: "audio", m4a: "audio", aac: "audio",
  pdf: "doc", txt: "doc", md: "doc", doc: "doc", docx: "doc", rtf: "doc",
  xls: "doc", xlsx: "doc", csv: "doc", ppt: "doc", pptx: "doc",
  zip: "archive", tar: "archive", gz: "archive", bz2: "archive", "7z": "archive",
  rar: "archive", xz: "archive", tgz: "archive",
  js: "code", ts: "code", tsx: "code", jsx: "code", json: "code", yaml: "code", yml: "code",
  html: "code", css: "code", scss: "code", go: "code", rs: "code", py: "code", rb: "code",
  java: "code", c: "code", cpp: "code", h: "code", hpp: "code", sh: "code", sql: "code",
  toml: "code", xml: "code", lock: "code",
};

export function fileKind(name: string): Kind {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "generic";
  return EXT_MAP[name.slice(dot + 1).toLowerCase()] ?? "generic";
}

export function fileTypeLabel(name: string): string {
  const dot = name.lastIndexOf(".");
  if (dot < 0) return "File";
  return name.slice(dot + 1).toUpperCase();
}

export function FileIcon(props: { name: string; folder?: boolean; size?: number }): JSX.Element {
  const kind = () => (props.folder ? "folder" : fileKind(props.name));
  const size = props.size ?? 18;
  const cls = () => `obj-icon ${kind()}`;
  const url = () => {
    switch (kind()) {
      case "folder":  return "/icons/ui/folder.svg";
      case "image":   return "/icons/ui/image.svg";
      case "video":   return "/icons/ui/video.svg";
      case "audio":   return "/icons/ui/music.svg";
      case "doc":     return "/icons/ui/file-text.svg";
      case "archive": return "/icons/ui/archive.svg";
      case "code":    return "/icons/ui/code-2.svg";
      default:        return "/icons/ui/file.svg";
    }
  };
  return (
    <span class={cls()} style={{
      width: `${size}px`, height: `${size}px`,
      "mask-image": `url(${url()})`,
      "-webkit-mask-image": `url(${url()})`,
    }} />
  );
}

// ── provider icon (uses providers.json as single source of truth) ────────────

import { detectProvider, providerLabel } from "../providers";
export { detectProvider, providerLabel };

export function ProviderIcon(props: {
  account: { endpoint?: string | null; region?: string };
  size?: number;
}) {
  const def = () => detectProvider(props.account);
  const sz = props.size ?? 22;
  return (
    <span
      class={`provider-tile ${def().monochrome_icon ? "mono" : ""}`}
      style={{ width: `${sz}px`, height: `${sz}px` }}
    >
      <img src={def().iconUrl} alt={def().label} class="provider-tile-img" />
    </span>
  );
}

