import { DEFAULT_LANG, SUPPORTED, type LangCode, type LangPref } from "./languages.ts";

// "zh-CN", "ja_JP", "es-419" -> base language if we ship it.
export function matchLanguage(tag: string): LangCode | null {
  const base = tag.toLowerCase().split(/[-_]/)[0];
  return (SUPPORTED as readonly string[]).includes(base) ? (base as LangCode) : null;
}

// First system language we have a translation for, else English.
export function detectLanguage(tags: readonly string[]): LangCode {
  for (const tag of tags) {
    const hit = matchLanguage(tag);
    if (hit) return hit;
  }
  return DEFAULT_LANG;
}

export function resolveLanguage(pref: LangPref, systemTags: readonly string[]): LangCode {
  return pref === "system" ? detectLanguage(systemTags) : pref;
}
