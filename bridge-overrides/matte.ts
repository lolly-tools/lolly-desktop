// SPDX-License-Identifier: MPL-2.0
/**
 * Desktop matte override — the whole reason the full BiRefNet exists as a tier.
 *
 * The full BiRefNet (Swin-L @1024²) cannot run in the browser/CLI: ort-web is a
 * single-thread wasm32 module, and the model's upcast fp32 weights (~490 MB fp16 →
 * ~980 MB) plus a Swin-L's activations blow past the ~4 GB wasm32 address ceiling,
 * so `session.run()` aborts with std::bad_alloc — on effectively ANY device (it's
 * an address-space limit, not a RAM one). It runs fine under NATIVE ONNX Runtime
 * (onnxruntime-node proved it: ~18 s CPU), which is exactly what the desktop shell
 * can host. So this override routes the ONE native-only model (MATTE_NATIVE_ONLY)
 * to a Rust `matte_infer` command (src-tauri/src/matte.rs), and delegates every
 * other model to the shared wasm runner unchanged.
 *
 * Swapped in for shells/web/src/bridge/matte.ts via vite.config.js's
 * overrideBridgeModules ('matte'). It carries the SAME export name (createMatteAPI)
 * so bridge/index.ts's `import('./matte.ts')` site is byte-identical.
 *
 * The pre/post geometry is imported from the web runner (lib/matter.ts), so the
 * letterbox/normalize/compose math has one source of truth across wasm and native —
 * only the inference call in the middle differs. Model bytes come through the SAME
 * fetch-once/IndexedDB cache the wasm path and the offline manager use; the native
 * side just needs the file on disk, so we materialise it once into app-data and let
 * Rust load (and cache) the ORT session from there.
 */
import type {
  MatteAPI, MatteFeasibility, MatteFrame, MatteModelId, MatteModelInfo, MatteOpts, MatteProgress,
} from '@lolly-tools/core/host-v1';
import { createWasmMatteAPI } from '../../web/src/lib/matte-wasm-api.ts';
import {
  preprocessMatte, postprocessMatte, abortError, ModelNotInstalledError,
} from '../../web/src/lib/matter.ts';
import { createDebugLogger, createModelFetcher, type FetchProgress } from '../../web/src/lib/ort.ts';
import {
  MATTE_DEFAULT_MODEL, MATTE_MODEL_CACHE_VERSION, MATTE_MODEL_DIR, MATTE_MODEL_FILES,
  MATTE_MODEL_SPEC, MATTE_MODEL_STORE, MATTE_NATIVE_ONLY, matteModelsFor,
} from '../../web/src/lib/matte-models.ts';
import { exists, mkdir, writeFile, BaseDirectory } from '@tauri-apps/plugin-fs';
import { invoke } from '@tauri-apps/api/core';

const dbg = createDebugLogger({ tag: 'matte-native', storageKey: 'lolly:matte:debug', globalFlag: '__MATTE_DEBUG__' });
// Same store/dir/version as the wasm runner + the offline pre-download: one cache,
// no double download. The native path reads these bytes then materialises them to disk.
const fetchModelBytes = createModelFetcher({
  store: MATTE_MODEL_STORE, dir: MATTE_MODEL_DIR, version: MATTE_MODEL_CACHE_VERSION, dbg,
});

/** The app-data directory (relative to BaseDirectory.AppData) the native ORT session
 *  loads models from — mirrors the `/models/<dir>/` fetch layout. Rust resolves the
 *  SAME path via app_data_dir().join("models").join(dir). */
const MATTE_MODEL_APPDATA_DIR = `models/${MATTE_MODEL_DIR}`;

const isNativeModel = (id?: MatteModelId): boolean => MATTE_NATIVE_ONLY[id ?? MATTE_DEFAULT_MODEL] === true;

// Native ORT handles the fixed 1024² model comfortably; these only rule out an
// absurd OUTPUT (the cutout is the source size, capped by maxEdge) — mirrors the
// abs guards in lib/matter.ts canRun so behaviour is consistent across backends.
const ABS_MAX_EDGE = 12000;
const ABS_MAX_PIXELS = 40_000_000;

function canRunNative(src: { width: number; height: number }, opts: MatteOpts = {}): MatteFeasibility {
  const longEdge = Math.max(src.width, src.height);
  const cap = Math.min(longEdge, opts.maxEdge ?? longEdge);
  const scale = cap / longEdge;
  const outW = Math.round(src.width * scale), outH = Math.round(src.height * scale);
  if (outW > ABS_MAX_EDGE || outH > ABS_MAX_EDGE || outW * outH > ABS_MAX_PIXELS) {
    return {
      ok: false, reason: 'too-large', message: 'This image is too large to process on this device.',
      suggestedMaxEdge: Math.min(ABS_MAX_EDGE, Math.floor(Math.sqrt(ABS_MAX_PIXELS))),
    };
  }
  return { ok: true };
}

/** Are the model's bytes available locally (disk OR the IndexedDB cache)? Never
 *  downloads — the consent line and offline manager own that. */
async function nativeModelCached(id: MatteModelId): Promise<boolean> {
  const file = MATTE_MODEL_FILES[id];
  try {
    if (await exists(`${MATTE_MODEL_APPDATA_DIR}/${file}`, { baseDir: BaseDirectory.AppData })) return true;
  } catch { /* fs unavailable — fall through to the IDB check */ }
  return !!(await fetchModelBytes(file, true));
}

/** Ensure the model file is on disk for native ORT, downloading (with progress)
 *  into the shared IndexedDB cache first if needed, then materialising it once into
 *  app-data. Returns false when the bytes aren't on device and can't be fetched
 *  (offline / not vendored) — the caller turns that into ModelNotInstalledError. */
async function ensureModelOnDisk(
  id: MatteModelId, onDownload?: (p: FetchProgress) => void, signal?: AbortSignal,
): Promise<boolean> {
  const file = MATTE_MODEL_FILES[id];
  const rel = `${MATTE_MODEL_APPDATA_DIR}/${file}`;
  try {
    if (await exists(rel, { baseDir: BaseDirectory.AppData })) return true;
  } catch { /* fs probe failed — try to (re)materialise below */ }

  const bytes = await fetchModelBytes(file, false, onDownload);
  if (!bytes) return false;
  if (signal?.aborted) throw abortError();

  try { await mkdir(MATTE_MODEL_APPDATA_DIR, { baseDir: BaseDirectory.AppData, recursive: true }); }
  catch { /* already exists — mkdir is best-effort */ }
  await writeFile(rel, new Uint8Array(bytes), { baseDir: BaseDirectory.AppData });
  dbg('materialise', { file, bytes: bytes.byteLength });
  return true;
}

/** Run one forward pass through native ORT. Sends the normalized NCHW input as a
 *  RAW request body (no JSON blow-up on the ~12 MB tensor) with the model file +
 *  square edge in headers; Rust resolves the model path, runs the cached session,
 *  and returns the raw single-channel mask as f32 bytes. */
async function invokeMatteInfer(file: string, edge: number, input: Float32Array): Promise<Float32Array> {
  // A Uint8Array view of exactly the tensor's bytes — Tauri sends it as the raw
  // request body (InvokeBody::Raw on the Rust side), no JSON array serialization.
  const body = new Uint8Array(input.buffer, input.byteOffset, input.byteLength);
  const out = await invoke<ArrayBuffer>('matte_infer', body, {
    headers: { 'x-model-file': file, 'x-edge': String(edge) },
  });
  return new Float32Array(out);
}

async function runNative(frame: MatteFrame, opts: MatteOpts = {}): Promise<MatteFrame> {
  const id = opts.model ?? MATTE_DEFAULT_MODEL;
  const spec = MATTE_MODEL_SPEC[id];
  const signal = opts.signal;
  const onProgress = opts.onProgress;
  const checkAbort = (): void => { if (signal?.aborted) throw abortError(); };

  checkAbort();
  const ready = await ensureModelOnDisk(
    id, (p) => onProgress?.({ phase: 'download', loaded: p.loaded, total: p.total }), signal);
  if (!ready) throw new ModelNotInstalledError(id);
  checkAbort();

  const pre = preprocessMatte(frame, spec, opts);
  checkAbort();

  // The native run is long (~18 s CPU) but off the main thread (Rust blocking task),
  // so the UI stays live; report indeterminate inference (no sub-step progress).
  onProgress?.({ phase: 'inference' } as MatteProgress);
  const raw = await invokeMatteInfer(MATTE_MODEL_FILES[id], pre.edge, pre.input);
  checkAbort();

  const out = postprocessMatte(raw, pre);
  onProgress?.({ phase: 'inference', fraction: 1 });
  return out;
}

/**
 * Desktop MatteAPI: the full staged roster, native-only models via Rust ORT and
 * every other model via the shared wasm runner (byte-identical to web).
 */
export function createMatteAPI(): MatteAPI {
  const wasm = createWasmMatteAPI();
  return {
    ...wasm,
    // The desktop shell can run every staged model (native lifts the wasm ceiling).
    // bridge/index.ts already offers the full set on Tauri via matteModelsFor(true);
    // this keeps the api itself consistent for any direct caller.
    models: (): MatteModelInfo[] => matteModelsFor(true).map((m) => ({ ...m })),
    cached: (id: MatteModelId): Promise<boolean> =>
      isNativeModel(id) ? nativeModelCached(id) : wasm.cached(id),
    canRun: (src, opts): Promise<MatteFeasibility> =>
      isNativeModel(opts?.model) ? Promise.resolve(canRunNative(src, opts)) : wasm.canRun(src, opts),
    run: (frame, opts): Promise<MatteFrame> =>
      isNativeModel(opts?.model) ? runNative(frame, opts) : wasm.run(frame, opts),
  };
}
