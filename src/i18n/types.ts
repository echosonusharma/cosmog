import type en from "./locales/en/index.ts";

export type Messages = Record<string, string>;
export type TKey = keyof typeof en;
// Plural families: "<base>_one" / "<base>_other" etc. exist; callers pass the base.
export type PluralKey = { [K in TKey]: K extends `${infer B}_other` ? B : never }[TKey];
// Languages with more plural forms than English (ru: few, many) add keys English does not have.
type ExtraPlural = `${PluralKey}_${"zero" | "two" | "few" | "many"}`;
// Non-English files may lag behind; missing keys fall back to English.
export type Partial_ = Partial<Record<TKey | ExtraPlural, string>>;
