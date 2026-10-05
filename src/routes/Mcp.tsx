import { createResource, createSignal, createEffect, Show, For } from "solid-js";
import { IconMcp, IconCopy, IconRefresh } from "../utils/icons";
import { toast } from "../state/toast";
import { confirmDialog } from "../state/confirm";
import {
  mcpGetConfig, mcpSetConfig, mcpRegenerateToken,
  type McpConfig, type McpConfigPatch,
} from "../api/mcp";
import { t } from "../i18n";
import { mcpFsRootSchema, mcpPortSchema, parseSchema } from "../validation";

type ClientDef = {
  id: string;
  label: string;
  hint: () => string;
  build: (url: string, token: string) => string;
};

// Per-client add snippets. Token is substituted at render (masked) and at copy
// (real), so the shown config respects the reveal toggle.
const CLIENTS: ClientDef[] = [
  {
    id: "claude-code",
    label: "Claude Code",
    hint: () => t("mcp.client.claudeCode.hint"),
    build: (url, token) =>
      `claude mcp add --transport http cosmog ${url} \\\n  --header "Authorization: Bearer ${token}" --scope user`,
  },
  {
    id: "claude-desktop",
    label: "Claude Desktop",
    hint: () => t("mcp.client.claudeDesktop.hint"),
    build: (url, token) =>
      `{\n  "mcpServers": {\n    "cosmog": {\n      "type": "http",\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "codex",
    label: "Codex",
    hint: () => t("mcp.client.codex.hint"),
    build: (url, token) =>
      `[mcp_servers.cosmog]\nurl = "${url}"\nbearer_token_env_var = "COSMOG_MCP_TOKEN"\n# then in your shell: export COSMOG_MCP_TOKEN=${token}`,
  },
  {
    id: "cursor",
    label: "Cursor",
    hint: () => t("mcp.client.cursor.hint"),
    build: (url, token) =>
      `{\n  "mcpServers": {\n    "cosmog": {\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "vscode",
    label: "VS Code",
    hint: () => t("mcp.client.vscode.hint"),
    build: (url, token) =>
      `{\n  "servers": {\n    "cosmog": {\n      "type": "http",\n      "url": "${url}",\n      "headers": { "Authorization": "Bearer ${token}" }\n    }\n  }\n}`,
  },
  {
    id: "windsurf",
    label: "Windsurf",
    hint: () => t("mcp.client.windsurf.hint"),
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
      toast.ok(
        v ? t("mcp.toast.started") : t("mcp.toast.stopped"),
        v ? t("mcp.toast.startedBody") : t("mcp.toast.stoppedBody"),
      );
    }
  }

  async function acknowledge(v: boolean) {
    if (!v) return;
    await patch({ acknowledged: true });
  }

  async function toggleTool(name: string, enabled: boolean) {
    const c = cfg();
    if (!c) return;
    const disabled = new Set(
      c.advertised_tools.filter((x) => !x.enabled).map((x) => x.name),
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
    const result = parseSchema(mcpPortSchema, portDraft());
    if (!result.success) {
      toast.err(result.message);
      return;
    }
    if (await patch({ port: result.data })) toast.ok(t("mcp.toast.portUpdated"), t("mcp.toast.portUpdatedBody", { port: result.data }));
  }

  async function saveRoot() {
    const result = parseSchema(mcpFsRootSchema, rootDraft());
    if (!result.success) {
      toast.err(result.message);
      return;
    }
    const root = rootDraft().trim();
    if (await patch({ fs_root: root })) {
      toast.ok(
        t("mcp.toast.folderUpdated"),
        root ? t("mcp.toast.folderUpdatedBody", { root }) : t("mcp.toast.folderCleared"),
      );
    }
  }

  async function regenerate() {
    const ok = await confirmDialog({
      title: t("mcp.token.regenTitle"),
      body: t("mcp.token.regenBody"),
      confirmLabel: t("mcp.token.regenerate"),
      danger: true,
    });
    if (!ok) return;
    setBusy(true);
    try {
      await mcpRegenerateToken();
      await refetch();
      toast.ok(t("mcp.toast.tokenRegen"), t("mcp.toast.tokenRegenBody"));
    } catch (e) {
      toast.err(e);
    } finally {
      setBusy(false);
    }
  }

  function copy(text: string, what: "endpoint" | "token" | "config") {
    navigator.clipboard.writeText(text).then(
      () => toast.ok(t(`mcp.toast.copied.${what}`), t("mcp.toast.copiedBody")),
      () => toast.err(t("mcp.toast.copyFailed")),
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
        <span class="section-title">{t("mcp.title")}</span>
      </div>

      <div class="mcp-body">
        <div class="mcp-intro">
          <div class="mcp-intro-badge"><IconMcp size={18} /></div>
          <div class="mcp-intro-text">
            <span class="mcp-intro-lead">{t("mcp.intro.lead")}</span>
            <span class="mcp-intro-body">
              {t("mcp.intro.body")}
            </span>
          </div>
        </div>

        <Show when={cfg()}>
          {(c) => (
            <>
              {/* Consent gate: settings stay hidden until acknowledged, never shown again once accepted. */}
              <Show when={!c().acknowledged}>
                <div class="mcp-warning">
                  <div class="mcp-warning-title">{t("mcp.warning.title")}</div>
                  <ul class="mcp-warning-list">
                    <li>{t("mcp.warning.access")}</li>
                    <li>{t("mcp.warning.port")}</li>
                    <li>{t("mcp.warning.switches")}</li>
                  </ul>
                  <label class="mcp-ack">
                    <input
                      type="checkbox"
                      checked={false}
                      disabled={busy()}
                      onChange={(e) => acknowledge(e.currentTarget.checked)}
                    />
                    <span>{t("mcp.warning.ack")}</span>
                  </label>
                </div>
              </Show>

              <Show when={c().acknowledged}>
              <div class="settings-section">
                <div class="settings-section-title">{t("mcp.server.title")}</div>
                <label class="mcp-row">
                  <span class="mcp-row-label">{t("mcp.server.enable")}</span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().enabled}
                      disabled={busy()}
                      onChange={(e) => setEnabled(e.currentTarget.checked)}
                    />
                    <span class="mcp-switch-label">{c().enabled ? t("mcp.on") : t("mcp.off")}</span>
                  </span>
                </label>

                <div class="mcp-row">
                  <span class="mcp-row-label">{t("mcp.server.port")}</span>
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
                    <button class="btn-secondary" disabled={busy()} onClick={savePort}>{t("common.save")}</button>
                  </span>
                </div>
              </div>

              <Show when={c().enabled}>
                <div class="settings-section">
                  <div class="settings-section-title">{t("mcp.connection.title")}</div>

                  <Show when={!c().running}>
                    <div class="mcp-not-running">
                      {t("mcp.connection.notRunning")}
                    </div>
                  </Show>

                  <div class="mcp-row">
                    <span class="mcp-row-label">{t("mcp.connection.endpoint")}</span>
                    <span class="mcp-field">
                      <code class="mcp-code">{c().url}</code>
                      <button class="icon-btn" title={t("common.copy")} aria-label={t("common.copy")} onClick={() => copy(c().url, "endpoint")}>
                        <IconCopy size={14} />
                      </button>
                    </span>
                  </div>

                  <div class="mcp-row">
                    <span class="mcp-row-label">{t("mcp.connection.token")}</span>
                    <span class="mcp-field">
                      <code class="mcp-code mcp-token">{maskedToken(c())}</code>
                      <button class="btn-secondary" onClick={() => setReveal((v) => !v)}>
                        {reveal() ? t("mcp.token.hide") : t("mcp.token.reveal")}
                      </button>
                      <button class="icon-btn" title={t("common.copy")} aria-label={t("common.copy")} onClick={() => copy(c().token, "token")}>
                        <IconCopy size={14} />
                      </button>
                      <button class="icon-btn" title={t("mcp.token.regenerate")} aria-label={t("mcp.token.regenerate")} disabled={busy()} onClick={regenerate}>
                        <IconRefresh size={14} />
                      </button>
                    </span>
                  </div>

                </div>

                <div class="settings-section">
                  <div class="settings-section-title">{t("mcp.client.title")}</div>
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
                      title={t("common.copy")}
                      aria-label={t("common.copy")}
                      onClick={() => copy(activeClient().build(c().url, c().token), "config")}
                    >
                      <IconCopy size={14} />
                    </button>
                  </div>
                  <div class="mcp-row-note mcp-hint">{activeClient().hint()}</div>
                </div>
              </Show>

              <div class="settings-section">
                <div class="settings-section-title">{t("mcp.perm.title")}</div>

                <label class="mcp-perm" classList={{ "mcp-perm-on": c().allow_write }}>
                  <span class="mcp-perm-text">
                    <span class="mcp-perm-title">{t("mcp.perm.write")}</span>
                    <span class="mcp-perm-note">
                      {t("mcp.perm.writeNote")}
                    </span>
                  </span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().allow_write}
                      disabled={busy()}
                      onChange={(e) => patch({ allow_write: e.currentTarget.checked })}
                    />
                    <span class="mcp-switch-label">{c().allow_write ? t("mcp.on") : t("mcp.off")}</span>
                  </span>
                </label>

                <label class="mcp-perm mcp-perm-danger" classList={{ "mcp-perm-on": c().allow_delete }}>
                  <span class="mcp-perm-text">
                    <span class="mcp-perm-title">
                      {t("mcp.perm.delete")}
                      <span class="mcp-danger-tag">{t("mcp.perm.destructive")}</span>
                    </span>
                    <span class="mcp-perm-note">
                      {t("mcp.perm.deleteNote")}
                    </span>
                  </span>
                  <span class="mcp-switch">
                    <input
                      type="checkbox"
                      checked={c().allow_delete}
                      disabled={busy()}
                      onChange={(e) => patch({ allow_delete: e.currentTarget.checked })}
                    />
                    <span class="mcp-switch-label">{c().allow_delete ? t("mcp.on") : t("mcp.off")}</span>
                  </span>
                </label>
              </div>

              {/* Sandbox for file tools: upload/download refuse any path outside this folder, so
                  an untrusted object key cannot steer a read or write elsewhere on disk. */}
              <Show when={c().allow_write}>
                <div class="settings-section">
                  <div class="settings-section-title">{t("mcp.file.title")}</div>
                  <div class="mcp-row-note mcp-tools-hint">
                    {t("mcp.file.hint")}
                  </div>
                  <div class="mcp-row">
                    <span class="mcp-row-label">{t("mcp.file.folder")}</span>
                    <span class="mcp-field mcp-field-wide">
                      <input
                        type="text"
                        class="mcp-text-input"
                        placeholder="/home/you/mcp-shared"
                        value={rootDraft()}
                        disabled={busy()}
                        onInput={(e) => setRootDraft(e.currentTarget.value)}
                      />
                      <button class="btn-secondary" disabled={busy()} onClick={saveRoot}>{t("common.save")}</button>
                    </span>
                  </div>
                  <Show when={!c().fs_root}>
                    <div class="mcp-not-running">
                      {t("mcp.file.notSet")}
                    </div>
                  </Show>
                </div>
              </Show>

              <Show when={c().accounts.length > 0}>
                <div class="settings-section">
                  <div class="settings-section-title">{t("mcp.accounts.title")}</div>
                  <div class="mcp-row-note mcp-tools-hint">
                    {t("mcp.accounts.hint")}
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
                <div class="settings-section-title">{t("mcp.tools.title")}</div>
                <div class="mcp-row-note mcp-tools-hint">
                  {t("mcp.tools.hint")}
                </div>
                <div class="mcp-tools">
                  <For each={c().advertised_tools}>
                    {(tool) => (
                      <label class="mcp-tool-item" classList={{ "mcp-tool-off": !tool.enabled }}>
                        <span class="mcp-tool-text">
                          <code class="mcp-tool">{tool.name}</code>
                          <span class="mcp-tool-desc">{tool.description}</span>
                        </span>
                        <span class="mcp-switch">
                          <input
                            type="checkbox"
                            checked={tool.enabled}
                            disabled={busy()}
                            onChange={(e) => toggleTool(tool.name, e.currentTarget.checked)}
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
