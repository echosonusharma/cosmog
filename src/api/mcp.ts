import { invoke } from "@tauri-apps/api/core";

export interface McpConfig {
  enabled: boolean;
  port: number;
  allow_write: boolean;
  allow_delete: boolean;
  bind_all_accounts: boolean;
  acknowledged: boolean;
  fs_root: string | null;
  token: string;
  running: boolean;
  running_port: number | null;
  url: string;
  advertised_tools: McpTool[];
  accounts: McpAccount[];
}

export interface McpTool {
  name: string;
  description: string;
  enabled: boolean;
}

export interface McpAccount {
  id: string;
  name: string;
  enabled: boolean;
}

export type McpConfigPatch = Partial<{
  enabled: boolean;
  port: number;
  allow_write: boolean;
  allow_delete: boolean;
  bind_all_accounts: boolean;
  acknowledged: boolean;
  disabled_tools: string[];
  disabled_accounts: string[];
  fs_root: string;
}>;

export const mcpGetConfig = (): Promise<McpConfig> => invoke("mcp_get_config");

export const mcpSetConfig = (patch: McpConfigPatch): Promise<McpConfig> =>
  invoke("mcp_set_config", { patch });

export const mcpRegenerateToken = (): Promise<string> =>
  invoke("mcp_regenerate_token");

export const mcpStatus = (): Promise<McpConfig> => invoke("mcp_status");
