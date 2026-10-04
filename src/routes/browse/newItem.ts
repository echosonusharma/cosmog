import { headObject } from "../../api/objects";
import { errCode } from "../../utils/errors";

/** HEAD check; only not-found means absent, other errors reject. */
export const objectExists = (accountId: string, bucket: string, key: string): Promise<boolean> =>
  headObject(accountId, bucket, key).then(() => true, (e) => {
    if (errCode(e) === "not_found") return false;
    throw e;
  });

export const prefixToPath = (prefix: string): string =>
  prefix ? prefix.replace(/\/$/, "") + "/" : "";

export const focusEnd = (el: HTMLInputElement) =>
  setTimeout(() => { el.focus(); el.setSelectionRange(el.value.length, el.value.length); }, 0);
