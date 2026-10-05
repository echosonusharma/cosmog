import { Select } from "../../utils/Select";
import { LANG_FLAGS } from "../../i18n/flags";
import { LANG_NAMES, SUPPORTED, language, languagePref, setLanguage, t, type LangPref } from "../../i18n";
import { toast } from "../../state/toast";

// Applies immediately, like theme. "System" follows the device language.
export function LanguageSelect() {
  const options = () => [
    { value: "system", label: t("settings.language.system", { name: LANG_NAMES[language()] }), icon: LANG_FLAGS[language()] },
    ...SUPPORTED.map((c) => ({ value: c, label: LANG_NAMES[c], icon: LANG_FLAGS[c] })),
  ];

  async function choose(v: string) {
    try { await setLanguage(v as LangPref); } catch (e) { toast.err(e); }
  }

  return <Select value={languagePref()} options={options()} onChange={choose} />;
}
