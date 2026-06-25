import { invoke } from "@tauri-apps/api/core";

export interface LogTail {
  path: string;
  bytes_read: number;
  content: string;
}

export const getLogTail = (maxBytes?: number): Promise<LogTail> =>
  invoke("get_log_tail", { maxBytes: maxBytes ?? null });

export const getLogDir = (): Promise<string> =>
  invoke("get_log_dir");
