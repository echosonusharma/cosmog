import { createResource, createSignal, createEffect, Show, For } from "solid-js";
import { IconMcp, IconCopy, IconRefresh } from "../utils/icons";
import { toast } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import {
  mcpGetConfig, mcpSetConfig, mcpRegenerateToken,
  type McpConfig, type McpConfigPatch,
} from "../api/mcp";

type ClientDef = {
  id: string;
  label: string;
  hint: string;
  build: (url: string, token: string) => string;
};

// Per-client add snippets. Token is substituted at render (masked) and at copy
// (real), so the shown config respects the reveal toggle.
const CLIENTS: ClientDef[] = [
  {
    id: "claude-code",
    label: "Claude Code",
    hint: "Run in your terminal. Drop --scope user for project scope.",
    build: (url, token) =>
      `claude mcp add --transport http cosmog ${url} \\\n  --header "Authorization: Bearer ${token}" --scope user`,
  },
  {
    id: "claude-desktop",
    label: "Claude Desktop",
    hint: "Add to claude_desktop_config.json, then restart Claude Desktop.",
    build: (url, token) =>
      `{\n  "mcpServers": {\n    "cosmog": {\n      "type": "http",\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "codex",
    label: "Codex",
    hint: "Add to ~/.codex/config.toml, then export COSMOG_MCP_TOKEN in your shell.",
    build: (url, token) =>
      `[mcp_servers.cosmog]\nurl = "${url}"\nbearer_token_env_var = "COSMOG_MCP_TOKEN"\n# then in your shell: export COSMOG_MCP_TOKEN=${token}`,
  },
  {
    id: "cursor",
    label: "Cursor",
    hint: "Add to ~/.cursor/mcp.json (global) or .cursor/mcp.json (project).",
    build: (url, token) =>
      `{\n  "mcpServers": {\n    "cosmog": {\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "vscode",
    label: "VS Code",
    hint: "Add to .vscode/mcp.json. The top-level key is servers, not mcpServers.",
    build: (url, token) =>
      `{\n  "servers": {\n    "cosmog": {\n      "type": "http",\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "windsurf",
    label: "Windsurf",
    hint: "Add to ~/.codeium/windsurf/mcp_config.json. HTTP uses serverUrl.",
    build: (url, token) =>
      `{\n  "mcpServers": {\n    "cosmog": {\n      "serverUrl": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
];

export default function Mcp() {
  const [cfg, { refetch, mutate }] = createResource(mcpGetConfig);
  const [reveal, setReveal] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [portDraft, setPortDraft] = createSignal(4123);
  const [rootDraft, setRootDraft] = createSignal("");

  // Keep the editable fields in sync with loaded config.
  createEffect(() => {
    const c = cfg();
    if (c) {
      setPortDraft(c.port);
      setRootDraft(c.fs_root ?? "");
    }
  });

  async function patch(p: McpConfigPatch) {
    setBusy(true);
    try {
      const next = await mcpSetConfig(p);
      mutate(next);
      return true;
    } catch (e) {
      toast.err(e);
      await refetch();
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function setEnabled(v: boolean) {
    if (await patch({ enabled: v })) {
      toast.ok(v ? "MCP server started" : "MCP server stopped");
    }
  }

  async function acknowledge(v: boolean) {
    if (!v) return;
    await patch({ acknowledged: true });
  }

  async function toggleTool(name: string, enabled: boolean) {
    const c = cfg();
    if (!c) return;
    // Rebuild the disabled set from current tool state, then flip this one.
    const disabled = new Set(
      c.advertised_tools.filter((t) => !t.enabled).map((t) => t.name),
    );
    if (enabled) disabled.delete(name);
    else disabled.add(name);
    await patch({ disabled_tools: [...disabled] });
  }

  async function toggleAccount(id: string, enabled: boolean) {
    const c = cfg();
    if (!c) return;
    const disabled = new Set(
      c.accounts.filter((a) => !a.enabled).map((a) => a.id),
    );
    if (enabled) disabled.delete(id);
    else disabled.add(id);
    await patch({ disabled_accounts: [...disabled] });
  }

  async function savePort() {
    const p = portDraft();
    if (!Number.isInteger(p) || p < 1024 || p > 65535) {
      toast.err("Port must be between 1024 and 65535.");
      return;
    }
    if (await patch({ port: p })) toast.ok("Port updated");
  }

  async function saveRoot() {
    if (await patch({ fs_root: rootDraft().trim() })) toast.ok("Folder updated");
  }

  async function regenerate() {
    const ok = await confirmDialog({
      title: "Regenerate token?",
      body: "Any client using the current token will stop working until reconfigured.",
      confirmLabel: "Regenerate",
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await mcpRegenerateToken();
      await refetch();
      toast.ok("Token regenerated");
    } catch (e) {
      toast.err(e);
    } finally {
      setBusy(false);
    }
  }

  function copy(text: string, label: string) {
    navigator.clipboard.writeText(text).then(
      () => toast.ok(`${label} copied`),
      () => toast.err("Copy failed"),
    );
  }

  const [client, setClient] = createSignal(CLIENTS[0].id);
  const activeClient = () => CLIENTS.find((x) => x.id === client()) ?? CLIENTS[0];

  function maskedToken(c: McpConfig): string {
    return reveal() ? c.token : "•".repeat(Math.min(c.token.length, 40));
  }

  return (
    <div class="view-container">
      <div class="view-header">
        <span class="section-title">MCP Server</span>
      </div>

      <div class="mcp-body">
        <div class="mcp-intro">
          <div class="mcp-intro-badge"><IconMcp size={18} /></div>
          <div class="mcp-intro-text">
            <span class="mcp-intro-lead">Let a local AI drive Cosmog</span>
            <span class="mcp-intro-body">
              Exposes a local Model Context Protocol server so an AI client on this
              machine can list, search, and transfer your objects. Desktop only.
            </span>
          </div>
        </div>

        <Show when={cfg()}>
          {(c) => (
            <>
              {/* Consent gate. Settings stay hidden until acknowledged, and the
                  warning is never shown again once accepted. */}
              <Show when={!c().acknowledged}>
                <div class="mcp-warning">
                  <div class="mcp-warning-title">Read this before enabling</div>
                  <ul class="mcp-warning-list">
                    <li>A local AI client can list, search, upload, download, and, if you allow it, delete your objects using your stored credentials.</li>
                    <li>It opens a localhost port protected by a bearer token. Nothing is exposed to your network.</li>
                    <li>Write and delete are separate switches, both off by default.</li>
                  </ul>
                  <label class="mcp-ack">
                    <input
                      type="checkbox"
                      checked={false}
                      disabled={busy()}
                      onChange={(e) => acknowledge(e.currentTarget.checked)}
                    />
                    <span>I understand and want to enable the MCP server.</span>
                  </label>
                </div>
              </Show>

              <Show when={c().acknowledged}>
              <div class="settings-section">
                <div class="settings-section-title">Server</div>
                <label class="mcp-row">
                  <span class="mcp-row-label">Enable MCP server</span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().enabled}
                      disabled={busy()}
                      onChange={(e) => setEnabled(e.currentTarget.checked)}
                    />
                    <span class="mcp-switch-label">{c().enabled ? "On" : "Off"}</span>
                  </span>
                </label>

                <div class="mcp-row">
                  <span class="mcp-row-label">Port</span>
                  <span class="mcp-port">
                    <div class="num-field">
                      <input
                        type="number"
                        min={1024}
                        max={65535}
                        value={portDraft()}
                        disabled={busy()}
                        onInput={(e) => setPortDraft(parseInt(e.currentTarget.value, 10))}
                      />
                      <button type="button" class="num-field-btn" disabled={busy()} onClick={() => setPortDraft(Math.max(1024, (portDraft() || 1024) - 1))}>−</button>
                      <button type="button" class="num-field-btn" disabled={busy()} onClick={() => setPortDraft(Math.min(65535, (portDraft() || 1024) + 1))}>+</button>
                    </div>
                    <button class="btn-secondary" disabled={busy()} onClick={savePort}>Save</button>
                  </span>
                </div>
              </div>

              {/* Connection details, only meaningful when running. */}
              <Show when={c().enabled}>
                <div class="settings-section">
                  <div class="settings-section-title">Connection</div>

                  <Show when={!c().running}>
                    <div class="mcp-not-running">
                      The server is enabled but not listening. The port may be in use.
                      Try a different port.
                    </div>
                  </Show>

                  <div class="mcp-row">
                    <span class="mcp-row-label">Endpoint</span>
                    <span class="mcp-field">
                      <code class="mcp-code">{c().url}</code>
                      <button class="icon-btn" title="Copy" onClick={() => copy(c().url, "Endpoint")}>
                        <IconCopy size={14} />
                      </button>
                    </span>
                  </div>

                  <div class="mcp-row">
                    <span class="mcp-row-label">Bearer token</span>
                    <span class="mcp-field">
                      <code class="mcp-code mcp-token">{maskedToken(c())}</code>
                      <button class="btn-secondary" onClick={() => setReveal((v) => !v)}>
                        {reveal() ? "Hide" : "Reveal"}
                      </button>
                      <button class="icon-btn" title="Copy" onClick={() => copy(c().token, "Token")}>
                        <IconCopy size={14} />
                      </button>
                      <button class="icon-btn" title="Regenerate" disabled={busy()} onClick={regenerate}>
                        <IconRefresh size={14} />
                      </button>
                    </span>
                  </div>

                </div>

                <div class="settings-section">
                  <div class="settings-section-title">Connect a client</div>
                  <div class="mcp-tabs">
                    <For each={CLIENTS}>
                      {(cl) => (
                        <button
                          type="button"
                          classList={{ "mcp-tab": true, "mcp-tab-on": client() === cl.id }}
                          onClick={() => setClient(cl.id)}
                        >
                          {cl.label}
                        </button>
                      )}
                    </For>
                  </div>
                  <div class="mcp-field mcp-field-wide">
                    <pre class="mcp-code mcp-block">{activeClient().build(c().url, maskedToken(c()))}</pre>
                    <button
                      class="icon-btn"
                      title="Copy"
                      onClick={() => copy(activeClient().build(c().url, c().token), "Config")}
                    >
                      <IconCopy size={14} />
                    </button>
                  </div>
                  <div class="mcp-row-note mcp-hint">{activeClient().hint}</div>
                </div>
              </Show>

              <div class="settings-section">
                <div class="settings-section-title">Permissions</div>

                <label class="mcp-perm" classList={{ "mcp-perm-on": c().allow_write }}>
                  <span class="mcp-perm-text">
                    <span class="mcp-perm-title">Allow write</span>
                    <span class="mcp-perm-note">
                      Adds the upload and download tools. Off removes them from the MCP tool list.
                    </span>
                  </span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().allow_write}
                      disabled={busy()}
                      onChange={(e) => patch({ allow_write: e.currentTarget.checked })}
                    />
                    <span class="mcp-switch-label">{c().allow_write ? "On" : "Off"}</span>
                  </span>
                </label>

                <label class="mcp-perm mcp-perm-danger" classList={{ "mcp-perm-on": c().allow_delete }}>
                  <span class="mcp-perm-text">
                    <span class="mcp-perm-title">
                      Allow delete
                      <span class="mcp-danger-tag">destructive</span>
                    </span>
                    <span class="mcp-perm-note">
                      Adds the delete tool. Deletions are permanent and not reversible. Off removes it from the MCP tool list.
                    </span>
                  </span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().allow_delete}
                      disabled={busy()}
                      onChange={(e) => patch({ allow_delete: e.currentTarget.checked })}
                    />
                    <span class="mcp-switch-label">{c().allow_delete ? "On" : "Off"}</span>
                  </span>
                </label>
              </div>

              {/* Sandbox for the file tools. Upload/download refuse any path
                  outside this folder, so an untrusted object key cannot steer a
                  read or write elsewhere on disk. */}
              <Show when={c().allow_write}>
                <div class="settings-section">
                  <div class="settings-section-title">File access</div>
                  <div class="mcp-row-note mcp-tools-hint">
                    The upload and download tools can only touch files inside this folder. Leave it empty to block all file transfers.
                  </div>
                  <div class="mcp-row">
                    <span class="mcp-row-label">Allowed folder</span>
                    <span class="mcp-field mcp-field-wide">
                      <input
                        type="text"
                        class="mcp-text-input"
                        placeholder="/home/you/mcp-shared"
                        value={rootDraft()}
                        disabled={busy()}
                        onInput={(e) => setRootDraft(e.currentTarget.value)}
                      />
                      <button class="btn-secondary" disabled={busy()} onClick={saveRoot}>Save</button>
                    </span>
                  </div>
                  <Show when={!c().fs_root}>
                    <div class="mcp-not-running">
                      No folder set. Upload and download will be refused until you set one.
                    </div>
                  </Show>
                </div>
              </Show>

              <Show when={c().accounts.length > 0}>
                <div class="settings-section">
                  <div class="settings-section-title">Accounts</div>
                  <div class="mcp-row-note mcp-tools-hint">
                    Turn off any account to hide it from the AI and block all MCP access to it.
                  </div>
                  <div class="mcp-tools">
                    <For each={c().accounts}>
                      {(a) => (
                        <label class="mcp-tool-item" classList={{ "mcp-tool-off": !a.enabled }}>
                          <span class="mcp-tool-text">
                            <span class="mcp-account-name">{a.name}</span>
                            <span class="mcp-tool-desc">{a.id}</span>
                          </span>
                          <span class="mcp-switch">
                            <input
                              type="checkbox"
                              checked={a.enabled}
                              disabled={busy()}
                              onChange={(e) => toggleAccount(a.id, e.currentTarget.checked)}
                            />
                          </span>
                        </label>
                      )}
                    </For>
                  </div>
                </div>
              </Show>

              <div class="settings-section">
                <div class="settings-section-title">Advertised tools</div>
                <div class="mcp-row-note mcp-tools-hint">
                  Turn off any tool to remove it from what the AI can call.
                </div>
                <div class="mcp-tools">
                  <For each={c().advertised_tools}>
                    {(t) => (
                      <label class="mcp-tool-item" classList={{ "mcp-tool-off": !t.enabled }}>
                        <span class="mcp-tool-text">
                          <code class="mcp-tool">{t.name}</code>
                          <span class="mcp-tool-desc">{t.description}</span>
                        </span>
                        <span class="mcp-switch">
                          <input
                            type="checkbox"
                            checked={t.enabled}
                            disabled={busy()}
                            onChange={(e) => toggleTool(t.name, e.currentTarget.checked)}
                          />
                        </span>
                      </label>
                    )}
                  </For>
                </div>
              </div>
              </Show>
            </>
          )}
        </Show>
      </div>
    </div>
  );
}
