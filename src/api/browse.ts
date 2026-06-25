import { invoke } from "@tauri-apps/api/core";
import type { BrowseResult } from "../types";

export const browsePrefix = (
  accountId: string,
  bucket: string,
  prefix: string,
  force = false,
): Promise<BrowseResult> => invoke("browse_prefix", { accountId, bucket, prefix, force });
