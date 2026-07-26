import { invoke } from "@tauri-apps/api/core";
import type { Bucket, CorsConfig } from "../types";

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

export const getBucketPolicy = (
  accountId: string,
  name: string,
): Promise<string | null> =>
  invoke("get_bucket_policy", { accountId, name });

export const putBucketPolicy = (
  accountId: string,
  name: string,
  policy: string,
): Promise<void> =>
  invoke("put_bucket_policy", { accountId, name, policy });

export const deleteBucketPolicy = (
  accountId: string,
  name: string,
): Promise<void> =>
  invoke("delete_bucket_policy", { accountId, name });

export const getBucketCors = (
  accountId: string,
  name: string,
): Promise<CorsConfig | null> =>
  invoke("get_bucket_cors", { accountId, name });

export const putBucketCors = (
  accountId: string,
  name: string,
  cors: CorsConfig,
): Promise<void> =>
  invoke("put_bucket_cors", { accountId, name, cors });

export const deleteBucketCors = (
  accountId: string,
  name: string,
): Promise<void> =>
  invoke("delete_bucket_cors", { accountId, name });
