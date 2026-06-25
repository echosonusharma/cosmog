import { invoke } from "@tauri-apps/api/core";

export interface Account {
  id: string;
  name: string;
  protocol: string;
  endpoint: string | null;
  region: string;
  access_key_id: string;
  addressing_style: string;
  created_at: number;
  updated_at: number;
}

export interface AddAccountInput {
  name: string;
  protocol: string;
  endpoint?: string;
  region: string;
  access_key_id: string;
  secret_access_key: string;
  addressing_style?: string;
}

export const listAccounts = (): Promise<Account[]> =>
  invoke("list_accounts");

export const addAccount = (input: AddAccountInput): Promise<Account> =>
  invoke("add_account", { input });

export const testAccount = (id: string): Promise<number> =>
  invoke("test_account", { id });

export const deleteAccount = (id: string): Promise<void> =>
  invoke("delete_account", { id });

export interface UpdateAccountInput {
  name?: string;
  // Outer Option = field present; inner Option = explicit clear (null) vs value
  endpoint?: string | null;
  region?: string;
  access_key_id?: string;
  addressing_style?: string;
  /// Pass only when rotating the secret.
  secret_access_key?: string;
}

export const updateAccount = (id: string, input: UpdateAccountInput): Promise<Account> =>
  invoke("update_account", { id, input });
