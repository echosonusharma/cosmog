import { invoke } from "@tauri-apps/api/core";

export interface EncryptionStatus {
  enabled: boolean;
  public_recipient: string | null;
}

export interface EnableResult {
  public_recipient: string;
  secret_identity: string;
}

export interface KeyExport {
  tool: string;
  version: number;
  encryption_format: string;
  encryption_algorithm: string;
  secret_identity: string;
  public_recipient: string;
  external_decrypt_cmd: string;
}

export const enableBucketEncryption = (
  accountId: string,
  bucket: string,
  allowRotate?: boolean,
  confirmPreviousKeySaved?: boolean,
): Promise<EnableResult> =>
  invoke("enable_bucket_encryption", {
    accountId,
    bucket,
    allowRotate: allowRotate ?? null,
    confirmPreviousKeySaved: confirmPreviousKeySaved ?? null,
  });

export const disableBucketEncryption = (
  accountId: string,
  bucket: string,
): Promise<void> =>
  invoke("disable_bucket_encryption", { accountId, bucket });

export const getBucketEncryptionStatus = (
  accountId: string,
  bucket: string,
): Promise<EncryptionStatus> =>
  invoke("get_bucket_encryption_status", { accountId, bucket });

export const exportEncryptionKey = (
  accountId: string,
  bucket: string,
): Promise<KeyExport> =>
  invoke("export_encryption_key", { accountId, bucket });

export const saveEncryptionKeyExport = (
  accountId: string,
  bucket: string,
  destPath: string,
): Promise<void> =>
  invoke("save_encryption_key_export", { accountId, bucket, destPath });

export const importEncryptionIdentity = (
  accountId: string,
  bucket: string,
  identityText: string,
): Promise<void> =>
  invoke("import_encryption_identity", { accountId, bucket, identityText });

export const importEncryptionIdentityFromFile = (
  accountId: string,
  bucket: string,
  srcPath: string,
): Promise<void> =>
  invoke("import_encryption_identity_from_file", { accountId, bucket, srcPath });

export const hasEncryptionIdentity = (
  accountId: string,
  bucket: string,
): Promise<boolean> =>
  invoke("has_encryption_identity", { accountId, bucket });
