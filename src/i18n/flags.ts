import gb from "./flags/gb.svg";
import cn from "./flags/cn.svg";
import jp from "./flags/jp.svg";
import es from "./flags/es.svg";
import ru from "./flags/ru.svg";
import kr from "./flags/kr.svg";
import br from "./flags/br.svg";
import type { LangCode } from "./languages.ts";

// SVG, not emoji: Windows has no flag emoji glyphs.
export const LANG_FLAGS: Record<LangCode, string> = { en: gb, zh: cn, ja: jp, es, ru, ko: kr, pt: br };
