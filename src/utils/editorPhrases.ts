import { t } from "../i18n";

// CodeMirror looks up its built-in labels by their English text.
export function cmPhrases(): Record<string, string> {
  return {
    "Find": t("preview.cm.find"),
    "Replace": t("preview.cm.replaceField"),
    "next": t("preview.cm.next"),
    "previous": t("preview.cm.previous"),
    "all": t("preview.cm.all"),
    "match case": t("preview.cm.matchCase"),
    "regexp": t("preview.cm.regexp"),
    "by word": t("preview.cm.byWord"),
    "replace": t("preview.cm.replace"),
    "replace all": t("preview.cm.replaceAll"),
    "close": t("preview.cm.close"),
    "Go to line": t("preview.cm.goToLine"),
    "go": t("preview.cm.go"),
    "current match": t("preview.cm.currentMatch"),
    "on line": t("preview.cm.onLine"),
    "replaced match on line $": t("preview.cm.replacedMatch"),
    "replaced $ matches": t("preview.cm.replacedMatches"),
    "Diagnostics": t("preview.cm.diagnostics"),
    "No diagnostics": t("preview.cm.noDiagnostics"),
    "Control character": t("preview.cm.controlChar"),
    "Selection deleted": t("preview.cm.selectionDeleted"),
    "folded code": t("preview.cm.foldedCode"),
    "Folded lines": t("preview.cm.foldedLines"),
    "Unfolded lines": t("preview.cm.unfoldedLines"),
    "Fold line": t("preview.cm.foldLine"),
    "Unfold line": t("preview.cm.unfoldLine"),
    "to": t("preview.cm.to"),
    "unfold": t("preview.cm.unfold"),
    "Completions": t("preview.cm.completions"),
  };
}
