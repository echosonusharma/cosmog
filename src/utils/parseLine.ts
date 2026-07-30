import { logSource, type LogSource } from "./logSource";

export interface ParsedLine {
  ts: string;
  level: string;
  span: string | null;
  target: string | null;
  fields: Record<string, string>;
  msg: string;
  json: unknown | null;
  // Derived once at parse time so rows/filters never recompute it per render.
  source: LogSource;
}

const ANSI_RE = /\[[0-9;]*m/g;
function stripAnsi(s: string) { return s.replace(ANSI_RE, ""); }
const SPAN_RE = /^([a-zA-Z_][a-zA-Z0-9_]*)\{([^}]*)\}:\s*/;

function parseFields(s: string): Record<string, string> {
  const out: Record<string, string> = {};
  const re = /(\w+)=("([^"]*)"|(\S+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(s)) !== null) {
    out[m[1]] = m[3] ?? m[4] ?? "";
  }
  return out;
}

function tryJson(s: string): unknown | null {
  const t = s.trim();
  if (!(t.startsWith("{") && t.endsWith("}")) && !(t.startsWith("[") && t.endsWith("]"))) return null;
  try { return JSON.parse(t); } catch { return null; }
}

export function parseLine(raw: string): ParsedLine | null {
  const clean = stripAnsi(raw).trim();
  if (!clean) return null;
  const m = clean.match(/^(\d{4}-\d{2}-\d{2}T[\d:.]+Z?)\s+(INFO|DEBUG|WARN|ERROR|TRACE)\s+(.+)$/);
  let ts = "", level = "DEBUG", rest = clean;
  if (m) { ts = m[1].replace("T", " ").replace(/\.\d+Z?$/, ""); level = m[2]; rest = m[3]; }
  let span: string | null = null;
  let fields: Record<string, string> = {};
  const spanMatch = rest.match(SPAN_RE);
  if (spanMatch) {
    span = spanMatch[1];
    fields = parseFields(spanMatch[2]);
    rest = rest.slice(spanMatch[0].length);
  }
  const colonIdx = rest.indexOf(": ");
  let msg = rest;
  let target: string | null = null;
  if (colonIdx > 0 && /^[a-zA-Z_][\w:]*$/.test(rest.slice(0, colonIdx))) {
    target = rest.slice(0, colonIdx);
    msg = rest.slice(colonIdx + 2);
  }
  return { ts, level, span, target, fields, msg, json: tryJson(msg), source: logSource(target, msg) };
}

export function levelClass(l: string): string {
  switch (l) {
    case "INFO": return "info";
    case "WARN": return "warn";
    case "ERROR": return "error";
    default: return "debug";
  }
}
