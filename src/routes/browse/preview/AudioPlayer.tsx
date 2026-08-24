import { createResource, createSignal, createEffect, Show, onCleanup } from "solid-js";
import { presignGet, previewObject } from "../../../api/objects";
import { formatBytes } from "../../../utils/fmt";
import { IconEye, IconPlay, IconPause, IconVolume, IconMute } from "../../../utils/icons";
import { extOf } from "../helpers";
import type { CachedObjectMeta } from "../../../types";

// Android WebView refuses to decode a Blob with an invalid/wildcard type
// (e.g. "audio/*"), so encrypted playback needs a concrete MIME per extension.
const AUDIO_MIME: Record<string, string> = {
  mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg", oga: "audio/ogg",
  m4a: "audio/mp4", aac: "audio/aac", flac: "audio/flac", opus: "audio/ogg",
  weba: "audio/webm",
};
function audioMime(name: string, fallback?: string | null): string {
  return AUDIO_MIME[extOf(name)] || (fallback && fallback.startsWith("audio/") ? fallback : "audio/mpeg");
}
function describeMediaError(e: MediaError | null): string {
  if (!e) return "unknown media error";
  const codes: Record<number, string> = {
    1: "aborted", 2: "network error", 3: "decode error", 4: "source/format not supported",
  };
  return `${codes[e.code] || `code ${e.code}`}${e.message ? `: ${e.message}` : ""}`;
}

// Encrypted audio can't be range-streamed: playing decrypts the whole object
// into an in-memory Blob. Past this cap we refuse and point at Download.
const ENCRYPTED_HARD_MAX = 100 * 1024 * 1024;

function fmtTime(s: number): string {
  if (!isFinite(s) || s < 0) return "0:00";
  const m = Math.floor(s / 60);
  const sec = Math.floor(s % 60);
  return `${m}:${sec.toString().padStart(2, "0")}`;
}

export function AudioPreview(props: { obj: CachedObjectMeta; encrypted?: boolean }) {
  const [armed, setArmed] = createSignal(false);
  const encTooBig = () => !!props.encrypted && props.obj.size > ENCRYPTED_HARD_MAX;

  // Unencrypted streams from a presigned URL (preload="none" defers the fetch).
  // Encrypted must decrypt whole up front, gated behind an explicit Load click.
  const shouldLoad = () => {
    if (props.encrypted === undefined) return false;
    if (props.encrypted) return armed() && !encTooBig();
    return true;
  };

  let priorBlob: string | null = null;
  const [src] = createResource(
    () =>
      shouldLoad()
        ? { a: props.obj.account_id, b: props.obj.bucket, k: props.obj.key, enc: props.encrypted }
        : null,
    async ({ a, b, k, enc }) => {
      if (enc) {
        const maxBytes = props.obj.size > 0 ? props.obj.size + 64 : ENCRYPTED_HARD_MAX;
        const r = await previewObject(a, b, k, maxBytes);
        const blob = new Blob([new Uint8Array(r.bytes)], { type: audioMime(k, r.content_type) });
        return URL.createObjectURL(blob);
      }
      return presignGet(a, b, k);
    },
  );

  // Latch the resolved src: createResource returns undefined mid-refetch, which
  // would unmount the player and flash a layout jump; prior blob is revoked
  // only once the new url has replaced it.
  const [displaySrc, setDisplaySrc] = createSignal<string | null>(null);
  createEffect(() => {
    const s = src();
    if (!s) return;
    if (priorBlob && priorBlob !== s && priorBlob.startsWith("blob:")) URL.revokeObjectURL(priorBlob);
    priorBlob = s.startsWith("blob:") ? s : null;
    setDisplaySrc(s);
  });
  onCleanup(() => { if (priorBlob) URL.revokeObjectURL(priorBlob); });

  let audio: HTMLAudioElement | undefined;
  let trackEl: HTMLDivElement | undefined;
  const [playing, setPlaying] = createSignal(false);
  const [cur, setCur] = createSignal(0);
  const [dur, setDur] = createSignal(0);
  const [vol, setVol] = createSignal(1);
  const [muted, setMuted] = createSignal(false);
  const [rate, setRate] = createSignal(1);
  const [playErr, setPlayErr] = createSignal<string | null>(null);

  const progress = () => (dur() > 0 ? cur() / dur() : 0);

  // Rate resets to 1 whenever the element loads a new src; onLoadedMetadata
  // reapplies the current pick.
  const RATES = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2];
  function cycleRate() {
    const i = RATES.indexOf(rate());
    const next = RATES[(i + 1) % RATES.length];
    setRate(next);
    if (audio) audio.playbackRate = next;
  }

  // Streamed media may report duration=Infinity until the engine resolves it;
  // latch it when finite. Do NOT seek to force it: seeking past the end on
  // Android WebView ends the track and kills playback.
  function readDuration() {
    if (!audio) return;
    const d = audio.duration;
    if (isFinite(d) && d > 0) setDur(d);
  }

  createEffect(() => {
    void props.obj.key;
    setArmed(false);
    setPlaying(false);
    setCur(0);
    setDur(0);
    setPlayErr(null);
    if (audio) { audio.pause(); audio.currentTime = 0; }
    // Tear down the latched player when the new track won't auto-load, else the
    // previous UI + blob linger. Don't read armed() here: arming would retrigger.
    if (props.encrypted !== false) {
      if (priorBlob) { URL.revokeObjectURL(priorBlob); priorBlob = null; }
      setDisplaySrc(null);
    }
  });

  function toggle() {
    if (!audio) return;
    if (!audio.paused) { audio.pause(); return; }
    setPlayErr(null);
    audio.play().catch((e) => setPlayErr(e?.message || String(e)));
  }

  function seekAt(clientX: number) {
    if (!audio || !trackEl || !isFinite(audio.duration)) return;
    const r = trackEl.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    audio.currentTime = ratio * audio.duration;
    setCur(audio.currentTime);
  }

  let endDrag: (() => void) | null = null;
  function onSeekDown(e: PointerEvent) {
    if (!trackEl) return;
    trackEl.setPointerCapture(e.pointerId);
    seekAt(e.clientX);
    const move = (ev: PointerEvent) => seekAt(ev.clientX);
    const teardown = () => {
      trackEl?.removeEventListener("pointermove", move);
      trackEl?.removeEventListener("pointerup", up);
      trackEl?.removeEventListener("pointercancel", up);
      endDrag = null;
    };
    const up = (ev: PointerEvent) => {
      try { trackEl?.releasePointerCapture(ev.pointerId); } catch { /* already released */ }
      teardown();
    };
    endDrag = teardown;
    trackEl.addEventListener("pointermove", move);
    trackEl.addEventListener("pointerup", up);
    trackEl.addEventListener("pointercancel", up);
  }
  // Drop an in-flight drag's listeners if the player unmounts mid-drag.
  onCleanup(() => endDrag?.());

  // OS "now playing" integration (Windows SMTC / macOS Now Playing / Linux
  // MPRIS) via the Media Session API; no-op where unsupported (e.g. WebKitGTK).
  function setupMediaSession() {
    if (!("mediaSession" in navigator)) return;
    const ms = navigator.mediaSession;
    try {
      ms.metadata = new MediaMetadata({
        title: props.obj.basename,
        artist: props.obj.bucket,
      });
    } catch { /* MediaMetadata unavailable */ }
    ms.setActionHandler("play", () => audio?.play().catch(() => {}));
    ms.setActionHandler("pause", () => audio?.pause());
    ms.setActionHandler("seekto", (d) => {
      if (audio && d.seekTime != null) { audio.currentTime = d.seekTime; setCur(audio.currentTime); }
    });
    ms.setActionHandler("seekbackward", (d) => {
      if (audio) audio.currentTime = Math.max(0, audio.currentTime - (d.seekOffset || 10));
    });
    ms.setActionHandler("seekforward", (d) => {
      if (audio) audio.currentTime = Math.min(audio.duration || Infinity, audio.currentTime + (d.seekOffset || 10));
    });
  }
  function updatePositionState() {
    if (!("mediaSession" in navigator) || !audio) return;
    if (!isFinite(audio.duration) || audio.duration <= 0) return;
    try {
      navigator.mediaSession.setPositionState({
        duration: audio.duration,
        position: Math.min(audio.currentTime, audio.duration),
        playbackRate: audio.playbackRate,
      });
    } catch { /* setPositionState unsupported */ }
  }
  onCleanup(() => {
    if (!("mediaSession" in navigator)) return;
    navigator.mediaSession.playbackState = "none";
    navigator.mediaSession.metadata = null;
  });

  function toggleMute() {
    if (!audio) return;
    audio.muted = !audio.muted;
    setMuted(audio.muted);
  }

  function onVol(e: Event) {
    const v = Number((e.currentTarget as HTMLInputElement).value);
    setVol(v);
    if (audio) { audio.volume = v; audio.muted = v === 0; setMuted(v === 0); }
  }

  return (
    <div class="preview-audio">
      <Show when={encTooBig()}>
        <span class="muted text-xs preview-audio-note">
          Encrypted audio too large to play in-app ({formatBytes(props.obj.size)}). Download it instead.
        </span>
      </Show>
      <Show when={props.encrypted && !encTooBig() && !armed()}>
        <div class="preview-load-hint">
          <span class="muted text-xs">
            Encrypted audio ({formatBytes(props.obj.size)}). Decrypts whole into memory.
          </span>
          <button class="btn-secondary preview-btn-inline" onClick={() => setArmed(true)}>
            <IconEye size={15} /> Load audio
          </button>
        </div>
      </Show>
      <Show when={src.loading && !displaySrc()}>
        <div class="preview-loader">
          <span class="spinner spinner-lg" />
          <span>{props.encrypted ? "Decrypting…" : "Loading…"}</span>
        </div>
      </Show>
      <Show when={src.error && !displaySrc()}>
        <div class="preview-err-inline">
          <div class="preview-err-inline-title">Playback failed</div>
          <div class="preview-err-inline-hint">{String(src.error)}</div>
        </div>
      </Show>
      <Show when={displaySrc()}>
        <div class="aplayer" classList={{ "is-switching": src.loading }}>
          <div class="aplayer-seek-row">
            <span class="aplayer-time">{fmtTime(cur())}</span>
            <div
              class="aplayer-track"
              ref={trackEl}
              onPointerDown={onSeekDown}
              classList={{ "is-disabled": dur() <= 0 }}
            >
              <div class="aplayer-fill" style={{ width: `${progress() * 100}%` }} />
              <div class="aplayer-thumb" style={{ left: `${progress() * 100}%` }} />
            </div>
            <span class="aplayer-time">{fmtTime(dur())}</span>
          </div>
          <div class="aplayer-ctrl-row">
            <div class="aplayer-ctrl-left">
              <button class="aplayer-speed" onClick={cycleRate} title="Playback speed">
                {rate()}x
              </button>
            </div>
            <button class="aplayer-play" onClick={toggle} title={playing() ? "Pause" : "Play"}>
              <Show when={playing()} fallback={<IconPlay size={17} />}><IconPause size={17} /></Show>
            </button>
            <div class="aplayer-ctrl-right">
              <button class="aplayer-vol-btn" onClick={toggleMute} title={muted() ? "Unmute" : "Mute"}>
                <Show when={muted() || vol() === 0} fallback={<IconVolume size={15} />}>
                  <IconMute size={15} />
                </Show>
              </button>
              <input
                class="aplayer-vol"
                type="range" min="0" max="1" step="0.05"
                value={muted() ? 0 : vol()}
                onInput={onVol}
                title="Volume"
              />
            </div>
          </div>
          <Show when={src.loading}>
            <div class="aplayer-switch-overlay"><span class="spinner" /></div>
          </Show>
          <Show when={playErr()}>
            <div class="aplayer-err">{playErr()}</div>
          </Show>
          <audio
            ref={audio}
            preload="none"
            src={displaySrc()!}
            onError={() => setPlayErr(describeMediaError(audio?.error ?? null))}
            onPlay={() => {
              setPlaying(true);
              if ("mediaSession" in navigator) navigator.mediaSession.playbackState = "playing";
              setupMediaSession();
              updatePositionState();
            }}
            onPause={() => {
              setPlaying(false);
              if ("mediaSession" in navigator) navigator.mediaSession.playbackState = "paused";
            }}
            onEnded={() => setPlaying(false)}
            onTimeUpdate={() => {
              if (!audio) return;
              setCur(audio.currentTime);
              if (dur() === 0 && isFinite(audio.duration) && audio.duration > 0) {
                setDur(audio.duration);
              }
              updatePositionState();
            }}
            onLoadedMetadata={() => {
              if (audio) audio.playbackRate = rate();
              readDuration();
              setupMediaSession();
              updatePositionState();
            }}
            onDurationChange={readDuration}
            onVolumeChange={() => { if (audio) { setVol(audio.volume); setMuted(audio.muted); } }}
          />
        </div>
      </Show>
    </div>
  );
}
