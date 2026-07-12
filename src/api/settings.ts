import { invoke } from "@tauri-apps/api/core";
import type { AppSettings } from "../types";

export type SettingsPatch = Partial<{
  default_download_dir: string | null;
  transfer_concurrency: number;
  multipart_parallelism: number;
  multipart_threshold_bytes: number;
  part_size_bytes: number;
  prefix_sync_ttl_secs: number;
  presign_default_expires_secs: number;
  theme: string;
  show_hidden: boolean;
  confirm_destructive: boolean;
  http_proxy: string | null;
  custom_ca_path: string | null;
  request_log_ttl_days: number;
}>;

export const getSettings = (): Promise<AppSettings> => invoke("get_settings");

export const updateSettings = (patch: SettingsPatch): Promise<AppSettings> =>
  invoke("update_settings", { patch });

export const resetSettings = (): Promise<AppSettings> => invoke("reset_settings");
