// Single source for text file types (New file options, MIME lookup, editor exts).
import { extOf } from "./fmt";

export interface TextType {
  ext: string;
  label: string;
  mime: string;
}

export const TEXT_TYPES: TextType[] = [
  { ext: "txt",  label: "Plain text",  mime: "text/plain" },
  { ext: "md",   label: "Markdown",    mime: "text/markdown" },
  { ext: "json", label: "JSON",        mime: "application/json" },
  { ext: "yaml", label: "YAML",        mime: "application/yaml" },
  { ext: "toml", label: "TOML",        mime: "application/toml" },
  { ext: "xml",  label: "XML",         mime: "application/xml" },
  { ext: "html", label: "HTML",        mime: "text/html" },
  { ext: "css",  label: "CSS",         mime: "text/css" },
  { ext: "js",   label: "JavaScript",  mime: "text/javascript" },
  { ext: "ts",   label: "TypeScript",  mime: "text/typescript" },
  { ext: "py",   label: "Python",      mime: "text/x-python" },
  { ext: "rs",   label: "Rust",        mime: "text/x-rust" },
  { ext: "go",   label: "Go",          mime: "text/x-go" },
  { ext: "sh",   label: "Shell",       mime: "application/x-sh" },
  { ext: "sql",  label: "SQL",         mime: "application/sql" },
  { ext: "ini",  label: "INI",         mime: "text/x-ini" },
  { ext: "env",  label: ".env",        mime: "text/plain" },
  { ext: "log",  label: "Log",         mime: "text/plain" },
];

const ALIASES: Record<string, string> = { yml: "yaml", htm: "html", conf: "ini", cfg: "ini" };

// previewable as text, not offered by New file
const VIEW_ONLY_EXTS = ["tsx", "jsx", "rb", "java", "c", "cpp", "h", "properties", "dockerfile"];

export const TEXT_EXTS = new Set([...TEXT_TYPES.map((t) => t.ext), ...Object.keys(ALIASES), ...VIEW_ONLY_EXTS]);

export function isTextMime(ct: string | null | undefined): boolean {
  const t = (ct ?? "").split(";")[0].trim().toLowerCase();
  return t.startsWith("text/") || /(json|xml|javascript|yaml|toml|sql|\/x-sh)$/.test(t);
}

const typeFor = (ext: string): TextType | undefined => {
  const e = ALIASES[ext.toLowerCase()] ?? ext.toLowerCase();
  return TEXT_TYPES.find((t) => t.ext === e);
};

export function mimeForExt(ext: string): string {
  return typeFor(ext)?.mime ?? "text/plain";
}

const MIME_ALIASES: Record<string, string> = {
  "application/javascript": "text/javascript", "application/x-javascript": "text/javascript",
  "text/yaml": "application/yaml", "text/x-yaml": "application/yaml", "application/x-yaml": "application/yaml",
  "text/xml": "application/xml", "text/x-sql": "application/sql", "application/x-sql": "application/sql",
  "text/x-shellscript": "application/x-sh", "text/x-sh": "application/x-sh",
  "text/x-markdown": "text/markdown", "text/x-toml": "application/toml",
};

/** Editor ext for a MIME type, "" when unknown. */
export function extForMime(ct: string | null | undefined): string {
  const t = (ct ?? "").split(";")[0].trim().toLowerCase();
  const mime = MIME_ALIASES[t] ?? t;
  return TEXT_TYPES.find((x) => x.mime === mime)?.ext ?? "";
}

export function textTypeForName(name: string): TextType | undefined {
  const ext = extOf(name.split("/").pop() ?? "");
  return ext ? typeFor(ext) : undefined;
}

/** Swap or append the last segment's extension. */
export function withExt(name: string, ext: string): string {
  const slash = name.lastIndexOf("/") + 1;
  const base = name.slice(slash);
  if (!base) return name;
  const old = textTypeForName(base) ? extOf(base) : "";
  const stem = old && base.length > old.length + 1 ? base.slice(0, -(old.length + 1)) : base;
  return `${name.slice(0, slash)}${stem}.${ext}`;
}
