import Spinner from "../../utils/Spinner";
import { t } from "../../i18n";

// Inline marker on a row whose object or folder is being deleted.
export function DeletingBadge() {
  return (
    <span class="deleting-badge" title={t("browse.deleting.badge")}>
      <Spinner size={11} />
      <span class="deleting-badge-text">{t("browse.deleting.badge")}</span>
    </span>
  );
}
