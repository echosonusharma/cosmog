import { invoke } from "@tauri-apps/api/core";
import type { Bucket } from "../types";

export const listBuckets = (accountId: string): Promise<Bucket[]> =>
  invoke("list_buckets", { accountId });

export const createBucket = (
  accountId: string,
  name: string,
  region?: string,
): Promise<void> =>
  invoke("create_bucket", { accountId, name, region: region ?? null });

export const deleteBucket = (accountId: string, name: string): Promise<void> =>
  invoke("delete_bucket", { accountId, name });

export const getBucketVersioning = (
  accountId: string,
  name: string,
): Promise<boolean> =>
  invoke("get_bucket_versioning", { accountId, name });

export const putBucketVersioning = (
  accountId: string,
  name: string,
  enabled: boolean,
): Promise<void> =>
  invoke("put_bucket_versioning", { accountId, name, enabled });

export const getBucketLocation = (
  accountId: string,
  name: string,
): Promise<string | null> =>
  invoke("get_bucket_location", { accountId, name });
