import { createSignal, createResource, createEffect, For, Show } from "solid-js";
import { createStore } from "solid-js/store";
import { getBucketCors, putBucketCors, deleteBucketCors } from "../../../api/buckets";
import { toast } from "../../../state/toast";
import { IconPlus, IconTrash, IconChevronD } from "../../../utils/icons";
import type { CorsRule, CorsConfig } from "../../../types";
import { classifyBucketError, deniedMessage } from "./errors";
import { capWarning } from "./providerCaps";
import { DocLink } from "./DocLink";
import { validateCorsRules } from "../../../validation";

const METHODS = ["GET", "PUT", "POST", "DELETE", "HEAD"] as const;

// max_age_seconds is an i32 on the Rust side; anything larger fails to
// deserialize at the Tauri boundary with an opaque error. Guard it up front.
const MAX_AGE_LIMIT = 2147483647;

// Editable rule shape: origins/headers held as raw text for the textareas,
// methods as a string[] toggle set. Serialized back to CorsRule on save.
interface DraftRule {
  id: string;
  origins: string;
  methods: string[];
  headers: string;
  exposeHeaders: string;
  maxAge: string;
}

function splitList(s: string): string[] {
  return s
    .split(/[\n,]/)
    .map((x) => x.trim())
    .filter(Boolean);
}

function ruleToDraft(r: CorsRule): DraftRule {
  return {
    id: r.id ?? "",
    origins: r.allowed_origins.join("\n"),
    methods: [...r.allowed_methods],
    headers: r.allowed_headers.join("\n"),
    exposeHeaders: r.expose_headers.join("\n"),
    maxAge: r.max_age_seconds == null ? "" : String(r.max_age_seconds),
  };
}

function draftToRule(d: DraftRule): CorsRule {
  const maxAge = d.maxAge.trim();
  return {
    id: d.id.trim() || null,
    allowed_origins: splitList(d.origins),
    allowed_methods: d.methods,
    allowed_headers: splitList(d.headers),
    expose_headers: splitList(d.exposeHeaders),
    max_age_seconds: maxAge === "" ? null : Number(maxAge),
  };
}

function emptyDraft(): DraftRule {
  return { id: "", origins: "", methods: ["GET"], headers: "", exposeHeaders: "", maxAge: "" };
}

export function CorsTab(props: {
  accountId: string;
  bucket: string;
  providerId: string;
  providerLabel: string;
  onChanged: () => void;
}) {
  const [loaded, { refetch }] = createResource<
    { config: CorsConfig | null; denied: boolean; unsupported: boolean },
    { a: string; b: string }
  >(
    () => ({ a: props.accountId, b: props.bucket }),
    async ({ a, b }) => {
      try {
        const c = await getBucketCors(a, b);
        return { config: c, denied: false, unsupported: false };
      } catch (e) {
        const kind = classifyBucketError(e);
        if (kind === "unsupported") return { config: null, denied: false, unsupported: true };
        if (kind === "denied") return { config: null, denied: true, unsupported: false };
        throw e;
      }
    },
  );

  const [rules, setRules] = createStore<DraftRule[]>([]);
  // Bumped to force a re-seed from the last-loaded server config (Reset).
  const [seedTick, setSeedTick] = createSignal(0);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal("");

  // Seed the editable store from the loaded config. createResource hands back a
  // fresh object on every (re)fetch, so tracking loaded() re-seeds after a
  // save/refetch; seedTick re-seeds on an explicit Reset.
  createEffect(() => {
    seedTick();
    const l = loaded();
    if (!l || l.unsupported || l.denied) return;
    const drafts = (l.config?.rules ?? []).map(ruleToDraft);
    setRules(drafts);
  });

  function addRule() {
    setRules(rules.length, emptyDraft());
  }

  function removeRule(i: number) {
    setRules((prev) => prev.filter((_, idx) => idx !== i));
  }

  function toggleMethod(i: number, m: string) {
    setRules(i, "methods", (cur) =>
      cur.includes(m) ? cur.filter((x) => x !== m) : [...cur, m],
    );
  }

  // Themed stepper for max-age. Steps by a minute; clamps to [0, i32 max].
  function stepMaxAge(i: number, delta: number) {
    const cur = parseInt(rules[i].maxAge, 10);
    const base = Number.isFinite(cur) ? cur : 0;
    const next = Math.min(MAX_AGE_LIMIT, Math.max(0, base + delta));
    setRules(i, "maxAge", String(next));
  }

  function validate(): string | null {
    return validateCorsRules(rules);
  }

  async function handleSave() {
    setErr("");
    const v = validate();
    if (v) { setErr(v); return; }
    const config: CorsConfig = { rules: rules.map(draftToRule) };
    setBusy(true);
    try {
      await putBucketCors(props.accountId, props.bucket, config);
      toast.ok("CORS saved", `CORS configuration for "${props.bucket}" updated`);
      await refetch();
      props.onChanged();
    } catch (e) {
      setErr(errText(e, "put"));
    } finally {
      setBusy(false);
    }
  }

  async function handleDelete() {
    setErr("");
    setBusy(true);
    try {
      await deleteBucketCors(props.accountId, props.bucket);
      toast.ok("CORS removed", `CORS configuration for "${props.bucket}" deleted`);
      setRules([]);
      await refetch();
      props.onChanged();
    } catch (e) {
      setErr(errText(e, "put"));
    } finally {
      setBusy(false);
    }
  }

  function handleReset() {
    setErr("");
    setSeedTick((n) => n + 1);
  }

  function errText(e: unknown, op: "get" | "put"): string {
    const kind = classifyBucketError(e);
    if (kind === "denied") return deniedMessage("cors", op);
    if (kind === "unsupported") return "Not supported by this provider";
    return (e as Error)?.message ?? String(e);
  }

  const snap = () => loaded.latest ?? loaded();

  return (
    <div class="bcfg-tab">
      <Show when={!(loaded.loading && loaded.latest == null)} fallback={<div class="bcfg-loading"><span class="spinner spinner-lg" /><span>Loading CORS…</span></div>}>
        <Show when={!(loaded.error && loaded.latest == null)} fallback={<div class="status-msg err">{errText(loaded.error, "get")}</div>}>
          <Show when={snap()!.unsupported}>
            <div class="status-msg warn">Not supported by this provider</div>
          </Show>
          <Show when={snap()!.denied}>
            <div class="status-msg err">{deniedMessage("cors", "get")}</div>
          </Show>

          <Show when={!snap()!.unsupported && !snap()!.denied}>
            <Show when={capWarning(props.providerId, props.providerLabel, "cors")}>
              {(w) => <div class="status-msg warn bcfg-provider-warn">{w()}</div>}
            </Show>
            <div class="modal-sub bcfg-hint">
              Cross-origin rules control which web origins may call this bucket from a browser.
              No rules means CORS is not configured. <DocLink providerId={props.providerId} tab="cors" />
            </div>

            <div class="bcfg-cors-rules">
              <Show when={rules.length === 0}>
                <div class="empty-state bcfg-cors-empty">No CORS rules. Add one below.</div>
              </Show>
              <For each={rules}>
                {(rule, i) => (
                  <div class="bcfg-cors-rule">
                    <div class="bcfg-cors-rule-head">
                      <span class="bcfg-cors-rule-title">Rule {i() + 1}</span>
                      <button
                        class="icon-btn danger"
                        title="Remove rule"
                        disabled={busy()}
                        onClick={() => removeRule(i())}
                      >
                        <IconTrash size={14} />
                      </button>
                    </div>

                    <label class="bcfg-field">
                      <span class="bcfg-field-label">Allowed origins (one per line or comma-separated)</span>
                      <textarea
                        class="bcfg-textarea"
                        rows={2}
                        placeholder={"https://example.com\nhttps://app.example.com"}
                        value={rule.origins}
                        onInput={(e) => setRules(i(), "origins", e.currentTarget.value)}
                      />
                    </label>

                    <div class="bcfg-field">
                      <span class="bcfg-field-label">Allowed methods</span>
                      <div class="bcfg-methods">
                        <For each={METHODS}>
                          {(m) => (
                            <label class="bcfg-method">
                              <input
                                type="checkbox"
                                checked={rule.methods.includes(m)}
                                onChange={() => toggleMethod(i(), m)}
                              />
                              <span>{m}</span>
                            </label>
                          )}
                        </For>
                      </div>
                    </div>

                    <label class="bcfg-field">
                      <span class="bcfg-field-label">Allowed headers</span>
                      <textarea
                        class="bcfg-textarea"
                        rows={2}
                        placeholder="*"
                        value={rule.headers}
                        onInput={(e) => setRules(i(), "headers", e.currentTarget.value)}
                      />
                    </label>

                    <label class="bcfg-field">
                      <span class="bcfg-field-label">Expose headers</span>
                      <textarea
                        class="bcfg-textarea"
                        rows={2}
                        placeholder="ETag"
                        value={rule.exposeHeaders}
                        onInput={(e) => setRules(i(), "exposeHeaders", e.currentTarget.value)}
                      />
                    </label>

                    <div class="bcfg-cors-row">
                      <label class="bcfg-field bcfg-field-inline">
                        <span class="bcfg-field-label">Max age (seconds)</span>
                        <div class="bcfg-stepper">
                          <input
                            class="bcfg-input bcfg-stepper-input"
                            type="number"
                            min="0"
                            placeholder="3600"
                            value={rule.maxAge}
                            onInput={(e) => setRules(i(), "maxAge", e.currentTarget.value)}
                          />
                          <div class="bcfg-stepper-btns">
                            <button
                              type="button"
                              class="bcfg-step-btn"
                              title="Increase"
                              disabled={busy()}
                              onClick={() => stepMaxAge(i(), 60)}
                            >
                              <IconChevronD size={13} class="bcfg-step-up" />
                            </button>
                            <button
                              type="button"
                              class="bcfg-step-btn"
                              title="Decrease"
                              disabled={busy()}
                              onClick={() => stepMaxAge(i(), -60)}
                            >
                              <IconChevronD size={13} />
                            </button>
                          </div>
                        </div>
                      </label>
                      <label class="bcfg-field bcfg-field-inline">
                        <span class="bcfg-field-label">ID (optional)</span>
                        <input
                          class="bcfg-input"
                          type="text"
                          placeholder="rule-id"
                          value={rule.id}
                          onInput={(e) => setRules(i(), "id", e.currentTarget.value)}
                        />
                      </label>
                    </div>
                  </div>
                )}
              </For>
            </div>

            <button class="btn-secondary text-xs bcfg-add-rule" disabled={busy()} onClick={addRule}>
              <IconPlus size={13} /> Add rule
            </button>

            <Show when={err()}><div class="status-msg err bcfg-status">{err()}</div></Show>

            <div class="btn-row bcfg-actions">
              <div class="bcfg-actions-grp">
                <button class="btn-secondary text-xs" disabled={busy()} onClick={handleReset}>Reset</button>
                <button class="enc-disable-btn bcfg-danger-btn" disabled={busy()} onClick={handleDelete}>
                  {busy() ? "Working…" : "Delete all"}
                </button>
              </div>
              <div class="bcfg-actions-grp">
                <button class="btn-primary text-xs" disabled={busy() || rules.length === 0} onClick={handleSave}>
                  {busy() ? "Saving…" : "Save"}
                </button>
              </div>
            </div>
          </Show>
        </Show>
      </Show>
    </div>
  );
}
