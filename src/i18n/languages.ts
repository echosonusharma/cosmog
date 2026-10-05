export const SUPPORTED = ["en", "zh", "ja", "es", "ru", "ko", "pt"] as const;
export type LangCode = (typeof SUPPORTED)[number];
export type LangPref = LangCode | "system";

export const DEFAULT_LANG: LangCode = "en";

// Native names, so a user can always find their own language.
export const LANG_NAMES: Record<LangCode, string> = {
  en: "English",
  zh: "简体中文",
  ja: "日本語",
  es: "Español",
  ru: "Русский",
  ko: "한국어",
  pt: "Português (Brasil)",
};

export function isLangPref(v: unknown): v is LangPref {
  return v === "system" || (SUPPORTED as readonly unknown[]).includes(v);
}
