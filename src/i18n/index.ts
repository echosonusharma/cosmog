import { createSignal } from "solid-js";
import en from "./locales/en/index.ts";
import { getPref, setPref } from "../state/prefs";
import { interpolate, pluralCategory, type Params } from "./format.ts";
import { DEFAULT_LANG, isLangPref, type LangCode, type LangPref } from "./languages.ts";
import { resolveLanguage } from "./resolve.ts";
import type { Messages, PluralKey, TKey } from "./types.ts";

export { LANG_NAMES, SUPPORTED, type LangCode, type LangPref } from "./languages.ts";
export type { TKey, PluralKey } from "./types.ts";

const PREF_KEY = "language";

const loaders: Record<LangCode, () => Promise<{ default: Messages }>> = {
  en: async () => ({ default: en }),
  zh: () => import("./locales/zh/index.ts"),
  ja: () => import("./locales/ja/index.ts"),
  es: () => import("./locales/es/index.ts"),
  ru: () => import("./locales/ru/index.ts"),
  ko: () => import("./locales/ko/index.ts"),
  pt: () => import("./locales/pt/index.ts"),
};

const dicts: Partial<Record<LangCode, Messages>> = { en };
const [lang, setLang] = createSignal<LangCode>(DEFAULT_LANG);
const [pref, setPrefSignal] = createSignal<LangPref>("system");

export { lang as language, pref as languagePref };

const systemTags = () => (navigator.languages?.length ? navigator.languages : [navigator.language ?? ""]);

let activateSeq = 0;

async function activate(code: LangCode) {
  const seq = ++activateSeq;
  if (!dicts[code]) {
    try { dicts[code] = (await loaders[code]()).default; } catch { code = DEFAULT_LANG; }
  }
  // A newer switch started while this one was loading; let it win.
  if (seq !== activateSeq) return;
  document.documentElement.lang = code;
  setLang(code);
}

// Call once at boot after initPrefs, before first render.
export async function initI18n() {
  const saved = getPref<unknown>(PREF_KEY, "system");
  setPrefSignal(isLangPref(saved) ? saved : "system");
  await activate(resolveLanguage(pref(), systemTags()));
  window.addEventListener("languagechange", () => {
    if (pref() === "system") void activate(resolveLanguage("system", systemTags()));
  });
}

export async function setLanguage(p: LangPref) {
  setPrefSignal(p);
  setPref(PREF_KEY, p);
  await activate(resolveLanguage(p, systemTags()));
}

// Reads the language signal, so JSX and effects re-run on switch. Safe in handlers too.
export function t(key: TKey, params?: Params): string {
  const code = lang();
  return interpolate(dicts[code]?.[key] ?? en[key] ?? key, params);
}

// Plural-aware: looks up "<base>_<category>", falls back to "_other". {count} is filled in.
export function tp(base: PluralKey, count: number, params?: Params): string {
  const code = lang();
  const cat = pluralCategory(code, count);
  const get = (suffix: string) => dicts[code]?.[`${base}_${suffix}`] ?? (en as Messages)[`${base}_${suffix}`];
  const msg = get(cat) ?? get("other") ?? base;
  return interpolate(msg, { count: count.toLocaleString(code), ...params });
}
