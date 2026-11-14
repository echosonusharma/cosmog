import { createMemo, For, Show } from "solid-js";
import { Donut } from "../charts/Donut";
import { foldTop } from "../charts/palette";
import { formatBytes } from "../../../utils/fmt";
import { appSettings } from "../../../state/settings";
import { language, t, tp, type TKey } from "../../../i18n";
import type { DirStat, FolderAnalysis } from "../../../types";
import { foldKinds } from "./kinds";

const num = (n: number) => n.toLocaleString(language());
const extLabel = (ext: string) =>
  ext === "" ? t("folderUpload.summary.noExt") : ext === "*" ? t("folderUpload.summary.otherExt") : ext;

function DirList(props: { title: string; dirs: DirStat[] }) {
  return (
    <Show when={props.dirs.length > 0}>
      <Show when={props.title}><div class="stats-section-title">{props.title}</div></Show>
      <div class="stats-largest">
        <For each={props.dirs}>
          {(d) => (
            <div class="fu-dir-row" title={d.rel_path}>
              <span class="stats-largest-name truncate">{d.rel_path}</span>
              <span class="stats-bar-count">{tp("folderUpload.summary.dirFiles", d.files)}</span>
              <span class="stats-bar-val">{formatBytes(d.bytes)}</span>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

export function AnalysisSummary(props: { a: FolderAnalysis }) {
  const kinds = createMemo(() => foldKinds(props.a.by_ext));
  const segments = createMemo(() => foldTop(kinds(), 6, props.a.total_bytes,
    (k) => t(`folderUpload.kind.${k.kind}` as TKey), (k) => k.bytes, t("folderUpload.kind.other")));
  const exts = createMemo(() => props.a.by_ext.slice(0, 8));
  const maxExt = createMemo(() => Math.max(1, ...exts().map((e) => e.bytes)));
  const total = () => props.a.total_bytes || 1;
  const tempSpace = () => props.a.largest_file_bytes * (appSettings()?.transfer_concurrency ?? 3);

  return (
    <div class="fu-summary">
      <div class="stats-summary">
        <div class="stats-figure">
          <span class="stats-figure-val">{num(props.a.files)}</span>
          <span class="stats-figure-lbl">{t("folderUpload.summary.files")}</span>
        </div>
        <div class="stats-figure">
          <span class="stats-figure-val">{formatBytes(props.a.total_bytes)}</span>
          <span class="stats-figure-lbl">{t("folderUpload.summary.totalSize")}</span>
        </div>
        <div class="stats-figure">
          <span class="stats-figure-val">{num(props.a.dirs)}</span>
          <span class="stats-figure-lbl">{t("folderUpload.summary.folders")}</span>
        </div>
      </div>

      <Show when={props.a.empty_dirs > 0}>
        <div class="fu-note">{tp("folderUpload.summary.emptyDirs", props.a.empty_dirs)}</div>
      </Show>
      <Show when={props.a.zero_byte_files > 0}>
        <div class="fu-note">{tp("folderUpload.summary.zeroByte", props.a.zero_byte_files)}</div>
      </Show>

      <Show when={props.a.warnings.length > 0}>
        <div class="fu-warnings" role="alert">
          <div class="fu-warnings-title">{t("folderUpload.warning.title")}</div>
          <For each={props.a.warnings}>
            {(w) => <div class="fu-warning">{t(`folderUpload.warning.${w}` as TKey, { size: formatBytes(tempSpace()) })}</div>}
          </For>
        </div>
      </Show>

      <Show when={props.a.total_bytes > 0}>
        <div class="stats-cols">
          <div class="stats-col">
            <div class="stats-section-title">{t("folderUpload.summary.byKind")}</div>
            <div class="stats-donut-wrap">
              <Donut segments={segments()} size={112} thickness={16} />
              <div class="stats-legend">
                <For each={segments()}>
                  {(seg) => (
                    <div class="stats-legend-row">
                      <span class="stats-legend-dot" style={{ background: seg.color }} />
                      <span class="stats-legend-name truncate">{seg.label}</span>
                      <span class="stats-legend-pct">{Math.round((seg.value / total()) * 100)}%</span>
                    </div>
                  )}
                </For>
              </div>
            </div>
          </div>
          <div class="stats-col">
            <div class="stats-section-title">{t("folderUpload.summary.byExt")}</div>
            <div class="stats-bars">
              <For each={exts()}>
                {(e) => (
                  <div class="stats-bar-row">
                    <span class="stats-bar-label truncate">{extLabel(e.ext)}</span>
                    <div class="stats-bar-track">
                      <div class="stats-bar-fill" style={{ width: `${(e.bytes / maxExt()) * 100}%` }} />
                    </div>
                    <span class="stats-bar-val">{formatBytes(e.bytes)}</span>
                    <span class="stats-bar-count">{num(e.files)}</span>
                  </div>
                )}
              </For>
            </div>
          </div>
        </div>
      </Show>

      <Show when={props.a.hidden.files > 0}>
        <div class="fu-card">
          <div class="fu-card-title">{t("folderUpload.summary.hidden")}</div>
          <div class="fu-note">{tp("folderUpload.summary.hiddenBody", props.a.hidden.files, { size: formatBytes(props.a.hidden.bytes) })}</div>
          <DirList title="" dirs={props.a.hidden.top_dirs} />
        </div>
      </Show>

      <DirList title={t("folderUpload.summary.largestFolders")} dirs={props.a.top_dirs} />

      <Show when={props.a.largest_files.length > 0}>
        <div class="stats-section-title">{t("folderUpload.summary.largestFiles")}</div>
        <div class="stats-largest">
          <For each={props.a.largest_files}>
            {(f) => (
              <div class="stats-largest-row" title={f.rel_path}>
                <span class="stats-largest-name truncate">{f.rel_path}</span>
                <span class="stats-bar-val">{formatBytes(f.size)}</span>
              </div>
            )}
          </For>
        </div>
      </Show>

      <Show when={props.a.windows_unsafe_names > 0}>
        <div class="fu-note">{tp("folderUpload.summary.windowsUnsafe", props.a.windows_unsafe_names)}</div>
      </Show>
      <Show when={props.a.deepest}>
        {(d) => <div class="fu-note faint truncate" title={d().rel_path}>
          {tp("folderUpload.summary.deepest", d().depth, { path: d().rel_path })}
        </div>}
      </Show>
    </div>
  );
}
