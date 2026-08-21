// SPDX-License-Identifier: MPL-2.0
//! Native background removal (host.matte) - runs the full BiRefNet that the
//! browser/CLI cannot.
//!
//! ort-web is a single-thread wasm32 module: the full BiRefNet (Swin-L @1024²),
//! with its upcast fp32 weights (~490 MB fp16 → ~980 MB) plus a Swin-L's
//! activations, overruns the ~4 GB wasm32 ADDRESS space and `session.run()` aborts
//! with std::bad_alloc - on effectively any device. Native ONNX Runtime has no such
//! ceiling (onnxruntime-node ran this exact model clean in ~18 s CPU), so the
//! desktop shell routes it here via the `ort` crate.
//!
//! One forward pass, session cached per model file. Transport is the Tauri v2 raw
//! IPC: the JS side (bridge-overrides/matte.ts) sends the normalized NCHW input as
//! a RAW request body (f32 little-endian - no JSON blow-up on the ~12 MB tensor)
//! with `x-model-file` + `x-edge` headers, and we return the single-channel mask as
//! raw f32 bytes. The JS side has already materialised the model into app-data
//! (models/matte/<file>), so we load from there and never download.
//!
//! NOTE (build): the `ort` calls target ort 2.0. If a resolved patch differs, the
//! two likely spots are the output extraction (`try_extract_tensor` vs
//! `try_extract_raw_tensor`) and the input/output metadata field access
//! (`session.inputs[0].name`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::{AppHandle, Manager};

/// ORT sessions cached by absolute model path - a ~490 MB model loads once, then
/// every later cut-out reuses it. The outer Mutex guards creation; the inner
/// Mutex<Session> serialises a run (matte is one-at-a-time), so the heavy pass
/// never holds the creation lock.
type SessionMap = Mutex<HashMap<PathBuf, Arc<Mutex<Session>>>>;
static SESSIONS: OnceLock<SessionMap> = OnceLock::new();

fn sessions() -> &'static SessionMap {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_for(path: &PathBuf) -> Result<Arc<Mutex<Session>>, String> {
    let mut cache = sessions().lock().map_err(|_| "session cache poisoned".to_string())?;
    if let Some(s) = cache.get(path) {
        return Ok(Arc::clone(s));
    }
    let session = Session::builder()
        .map_err(|e| format!("ort builder: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("load model {}: {e}", path.display()))?;
    let arc = Arc::new(Mutex::new(session));
    cache.insert(path.clone(), Arc::clone(&arc));
    Ok(arc)
}

/// Little-endian bytes → f32s. Manual (not a bytemuck cast) because the IPC body is
/// a heap Vec<u8> with only 1-byte alignment, which a `cast_slice::<u8, f32>` would
/// panic on. JS Float32Array is always little-endian on our targets.
fn f32s_from_le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn f32s_to_le(vals: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(vals.len() * 4);
    for v in vals {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

fn infer(model_path: PathBuf, edge: usize, input: Vec<f32>) -> Result<Vec<f32>, String> {
    let expected = 3 * edge * edge;
    if input.len() != expected {
        return Err(format!("matte input length {} != expected {expected}", input.len()));
    }
    let session_arc = session_for(&model_path)?;
    let mut session = session_arc.lock().map_err(|_| "session lock poisoned".to_string())?;

    // Read both tensor names before run() so no borrow of `session` outlives it.
    let input_name = session.inputs[0].name.clone();
    let output_name = session.outputs[0].name.clone();

    let tensor = Tensor::from_array(([1_usize, 3, edge, edge], input))
        .map_err(|e| format!("build input tensor: {e}"))?;
    // `ort::inputs!` returns the input vec directly in ort 2.0.0-rc.10 (it was a Result in
    // earlier rcs); `run` takes it straight.
    let outputs = session
        .run(ort::inputs![input_name.as_str() => tensor])
        .map_err(|e| format!("ort run: {e}"))?;

    // Single-channel logit/prob mask, edge*edge f32 - the JS side activates + composes.
    let (_shape, data) = outputs[output_name.as_str()]
        .try_extract_tensor::<f32>()
        .map_err(|e| format!("extract output: {e}"))?;
    Ok(data.to_vec())
}

fn header(request: &Request<'_>, key: &str) -> Result<String, String> {
    request
        .headers()
        .get(key)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing header {key}"))
}

/// Run one native ORT forward pass. See the module docs for the transport contract.
#[tauri::command]
pub async fn matte_infer(app: AppHandle, request: Request<'_>) -> Result<Response, String> {
    let model_file = header(&request, "x-model-file")?;
    let edge: usize = header(&request, "x-edge")?
        .parse()
        .map_err(|_| "x-edge is not a positive integer".to_string())?;
    let body = match request.body() {
        InvokeBody::Raw(bytes) => bytes.clone(),
        InvokeBody::Json(_) => return Err("matte_infer expects a raw request body".into()),
    };
    drop(request); // don't hold the (non-Send) Request across the await below

    // Resolve the model the JS side materialised into app-data. Basename only, so a
    // stray header can't escape the models dir.
    let name = PathBuf::from(&model_file);
    let name = name.file_name().ok_or("bad model file name")?;
    let model_path = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir: {e}"))?
        .join("models")
        .join("matte")
        .join(name);
    if !model_path.exists() {
        return Err(format!("matte model not on disk: {}", model_path.display()));
    }

    let input = f32s_from_le(&body);
    let mask = tauri::async_runtime::spawn_blocking(move || infer(model_path, edge, input))
        .await
        .map_err(|e| format!("matte task panicked: {e}"))??;
    Ok(Response::new(f32s_to_le(&mask)))
}
