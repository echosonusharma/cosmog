import { invoke } from "@tauri-apps/api/core";

export function clearAppData(): Promise<void> {
  return invoke("clear_app_data");
}
