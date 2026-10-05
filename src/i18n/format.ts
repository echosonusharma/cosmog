export type Params = Record<string, string | number>;

// "{name}" placeholders; unknown ones are left visible so gaps are noticed.
export function interpolate(msg: string, params?: Params): string {
  if (!params) return msg;
  return msg.replace(/\{(\w+)\}/g, (m, k) => (k in params ? String(params[k]) : m));
}

// "one" | "other" | ... for the language, used as a key suffix.
export function pluralCategory(lang: string, n: number): string {
  return new Intl.PluralRules(lang).select(n);
}
