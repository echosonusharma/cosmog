import { invoke } from "@tauri-apps/api/core";
import type { ObjectPreview, ObjectVersion } from "../types";

export const listKeysUnderPrefix = (
  accountId: string,
  bucket: string,
  prefix: string,
): Promise<string[]> =>
  invoke("list_keys_under_prefix", { accountId, bucket, prefix });

export const deleteObject = (accountId: string, bucket: string, key: string): Promise<void> =>
  invoke("delete_object", { accountId, bucket, key });

export const createFolder = (accountId: string, bucket: string, prefix: string): Promise<void> =>
  invoke("create_folder", { accountId, bucket, prefix });

export const deleteObjects = (
  accountId: string,
  bucket: string,
  keys: string[],
): Promise<{ deleted: string[]; errors: Array<{ key: string; code: string; message: string }> }> =>
  invoke("delete_objects", { accountId, bucket, keys });

export const presignGet = (
  accountId: string,
  bucket: string,
  key: string,
  expiresSecs?: number,
): Promise<string> =>
  invoke("presign_get", { accountId, bucket, key, expiresSecs: expiresSecs ?? null });

export const copyObject = (
  accountId: string,
  srcBucket: string,
  srcKey: string,
  dstBucket: string,
  dstKey: string,
): Promise<void> =>
  invoke("copy_object", { accountId, srcBucket, srcKey, dstBucket, dstKey });

export const moveObject = (
  accountId: string,
  srcBucket: string,
  srcKey: string,
  dstBucket: string,
  dstKey: string,
): Promise<void> =>
  invoke("move_object", { accountId, srcBucket, srcKey, dstBucket, dstKey });

export const previewObject = (
  accountId: string,
  bucket: string,
  key: string,
  maxBytes?: number,
): Promise<ObjectPreview> =>
  invoke("preview_object", { accountId, bucket, key, maxBytes: maxBytes ?? null });

export const putObjectText = (
  accountId: string,
  bucket: string,
  key: string,
  content: string,
  contentType: string,
): Promise<void> =>
  invoke("put_object_text", { accountId, bucket, key, content, contentType });

export const putObjectBytes = (
  accountId: string,
  bucket: string,
  key: string,
  bytes: number[],
  contentType: string,
): Promise<void> =>
  invoke("put_object_bytes_cmd", { accountId, bucket, key, bytes, contentType });

export const listObjectVersions = (
  accountId: string,
  bucket: string,
  prefix?: string,
  continuation?: string,
): Promise<{ versions: ObjectVersion[]; continuation: string | null }> =>
  invoke("list_object_versions", {
    accountId, bucket,
    prefix: prefix ?? null,
    continuation: continuation ?? null,
  });

export const deleteObjectVersion = (
  accountId: string,
  bucket: string,
  key: string,
  versionId: string,
): Promise<void> =>
  invoke("delete_object_version", { accountId, bucket, key, versionId });
