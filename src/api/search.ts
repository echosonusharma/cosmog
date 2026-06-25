import { invoke, Channel } from "@tauri-apps/api/core";
import type { BucketIndexStatus, BucketStats, SearchQuery, SearchResult } from "../types";

export const searchObjects = (query: SearchQuery): Promise<SearchResult> =>
  invoke("search_objects", { query });

export const bucketIndexStatus = (accountId: string, bucket: string): Promise<BucketIndexStatus> =>
  invoke("bucket_index_status", { accountId, bucket });

export const enableBucketIndex = (
  accountId: string,
  bucket: string,
): Promise<{ upserted: number; removed: number }> => {
  const ch = new Channel<unknown>();
  ch.onmessage = () => {};
  return invoke("enable_bucket_index", { accountId, bucket, onEvent: ch });
};

export const disableBucketIndex = (accountId: string, bucket: string): Promise<void> =>
  invoke("disable_bucket_index", { accountId, bucket });

export const cancelBucketScan = (accountId: string, bucket: string): Promise<void> =>
  invoke("cancel_bucket_scan", { accountId, bucket });

export const reindexBucket = (
  accountId: string,
  bucket: string,
): Promise<{ upserted: number; removed: number }> => {
  const ch = new Channel<unknown>();
  ch.onmessage = () => {};
  return invoke("reindex_bucket", { accountId, bucket, onEvent: ch });
};

export const bucketStats = (accountId: string, bucket: string): Promise<BucketStats> =>
  invoke("bucket_stats", { accountId, bucket });

export const syncPrefix = (
  accountId: string,
  bucket: string,
  prefix: string,
  recursive: boolean,
): Promise<{ upserted: number; removed: number }> =>
  invoke("sync_prefix", { accountId, bucket, prefix, recursive });
