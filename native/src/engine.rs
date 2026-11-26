use crate::audio::clean_transcript;
use crate::diarizer::NemotronDiarizer;
use crate::models::{
    DEFAULT_FILENAME, DEFAULT_REPO, ensure_diarizer_file, ensure_file_by_source, ensure_vad_file,
};
use crate::translator::{HyMt3Server, ensure_hy_mt3_model, ensure_llama_server_binary};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams,
};

/// Holds a persistent WhisperState (which owns an Arc to WhisperContext) so Vulkan
/// KV-cache and scratch buffers are allocated once on load and reused across chunks.
pub struct LoadedWhisper {
    state: Mutex<WhisperState>,
}

pub struct AsrEngine {
    models: tokio::sync::Mutex<HashMap<String, Arc<LoadedWhisper>>>,
    vad: tokio::sync::Mutex<Option<Arc<Mutex<WhisperVadContext>>>>,
    diarizer: tokio::sync::Mutex<Option<Arc<NemotronDiarizer>>>,
    translator: tokio::sync::Mutex<Option<Arc<HyMt3Server>>>,
}

impl AsrEngine {
    pub fn new() -> Self {
        Self {
            models: tokio::sync::Mutex::new(HashMap::new()),
            vad: tokio::sync::Mutex::new(None),
            diarizer: tokio::sync::Mutex::new(None),
            translator: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn get_or_start_hy_mt3<F>(
        &self,
        mut on_progress: F,
    ) -> Result<Arc<HyMt3Server>, String>
    where
        F: FnMut(Option<u8>, String, Option<u64>) + Send,
    {
        let mut guard = self.translator.lock().await;
        if let Some(existing) = guard.as_ref()
            && existing.is_alive()
        {
            return Ok(Arc::clone(existing));
        }

        let exe = ensure_llama_server_binary(&mut on_progress).await?;
        let model = ensure_hy_mt3_model(&mut on_progress).await?;
        on_progress(None, "Loading Hy-MT3-1.8B onto GPU...".to_string(), None);

        let server = HyMt3Server::start(&exe, &model).await?;
        let arc = Arc::new(server);
        *guard = Some(Arc::clone(&arc));
        Ok(arc)
    }

    pub async fn get_or_load_vad<F>(&self, on_progress: F) -> Option<Arc<Mutex<WhisperVadContext>>>
    where
        F: FnMut(Option<u8>, String, Option<u64>) + Send,
    {
        let mut guard = self.vad.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Some(Arc::clone(existing));
        }

        let path = match ensure_vad_file(on_progress).await {
            Ok(p) => p,
            Err(err) => {
                eprintln!("[server] Silero VAD download failed, using RMS fallback: {err}");
                return None;
            }
        };

        let path_str = path.to_string_lossy().into_owned();
        let loaded = tokio::task::spawn_blocking(move || {
            let mut params = WhisperVadContextParams::new();
            params.set_use_gpu(false);
            params.set_n_threads(2);
            WhisperVadContext::new(&path_str, params)
        })
        .await;

        match loaded {
            Ok(Ok(ctx)) => {
                let arc = Arc::new(Mutex::new(ctx));
                *guard = Some(Arc::clone(&arc));
                Some(arc)
            }
            Ok(Err(err)) => {
                eprintln!("[server] Silero VAD load failed, using RMS fallback: {err}");
                None
            }
            Err(err) => {
                eprintln!("[server] Silero VAD task panicked: {err}");
                None
            }
        }
    }

    pub async fn get_or_load_diarizer<F>(&self, on_progress: F) -> Result<Arc<NemotronDiarizer>, String>
    where
        F: FnMut(Option<u8>, String, Option<u64>) + Send,
    {
        let mut guard = self.diarizer.lock().await;
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }

        let path = ensure_diarizer_file(on_progress).await?;
        let session = tokio::task::spawn_blocking(move || NemotronDiarizer::load(&path))
            .await
            .map_err(|e| format!("Diarizer load task panicked: {e}"))??;

        let arc = Arc::new(session);
        *guard = Some(Arc::clone(&arc));
        Ok(arc)
    }

    pub async fn get_or_load_model<F>(
        &self,
        filename: &str,
        repo: Option<&str>,
        url: Option<&str>,
        mut on_progress: F,
    ) -> Result<(String, Arc<LoadedWhisper>), String>
    where
        F: FnMut(Option<u8>, String, Option<u64>) + Send,
    {
        let clean_name = filename.trim();
        let (eff_name, eff_repo) = if clean_name.is_empty() {
            (DEFAULT_FILENAME, Some(DEFAULT_REPO))
        } else {
            (clean_name, repo)
        };

        {
            let guard = self.models.lock().await;
            if let Some(existing) = guard.get(eff_name) {
                return Ok((eff_name.to_string(), Arc::clone(existing)));
            }
        }

        on_progress(None, format!("Preparing {eff_name}..."), None);
        let path = ensure_file_by_source(eff_name, eff_repo, url, &mut on_progress).await?;
        on_progress(None, format!("Loading {eff_name} onto Vulkan GPU..."), None);

        let path_str = path.to_string_lossy().into_owned();
        let loaded_name = eff_name.to_string();
        let loaded = tokio::task::spawn_blocking(move || {
            let mut params = WhisperContextParameters::default();
            params.use_gpu(true);
            params.flash_attn(true);
            let ctx = WhisperContext::new_with_params(&path_str, params)
                .map_err(|e| format!("Failed to initialize Whisper model {loaded_name}: {e}"))?;
            let state = ctx
                .create_state()
                .map_err(|e| format!("Failed to create WhisperState for {loaded_name}: {e}"))?;
            Ok::<_, String>(LoadedWhisper {
                state: Mutex::new(state),
            })
        })
        .await
        .map_err(|e| format!("Model load task panicked: {e}"))??;

        let arc = Arc::new(loaded);
        let mut guard = self.models.lock().await;
        guard.insert(eff_name.to_string(), Arc::clone(&arc));
        Ok((eff_name.to_string(), arc))
    }

    pub async fn unload_model(&self, filename: &str) {
        let mut guard = self.models.lock().await;
        guard.remove(filename);
    }

    pub async fn unload_hy_mt3(&self) {
        let mut guard = self.translator.lock().await;
        guard.take();
    }

    pub async fn loaded_filenames(&self) -> Vec<String> {
        let guard = self.models.lock().await;
        let mut names: Vec<String> = guard.keys().cloned().collect();
        names.sort();
        names
    }
}

pub fn detect_speech_prob(vad: &Mutex<WhisperVadContext>, window: &[f32]) -> Option<f32> {
    let mut guard = vad.lock().ok()?;
    if guard.detect_speech(window).is_err() {
        return None;
    }
    let probs = guard.probabilities();
    if probs.is_empty() {
        return None;
    }
    let tail_start = probs.len().saturating_sub(4);
    probs[tail_start..]
        .iter()
        .copied()
        .reduce(f32::max)
}

pub fn transcribe(
    model: &LoadedWhisper,
    samples: &[f32],
    language: Option<&str>,
    translate: bool,
) -> Result<(String, String, u128), String> {
    let t0 = Instant::now();
    let mut state = model
        .state
        .lock()
        .map_err(|_| "Whisper state lock poisoned".to_string())?;

    let threads = std::thread::available_parallelism()
        .map(|n| n.get().min(8) as i32)
        .unwrap_or(4);

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(language);
    params.set_translate(translate);
    params.set_n_threads(threads);
    params.set_single_segment(true);
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    params.set_max_tokens(64);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);

    state
        .full(params, samples)
        .map_err(|e| format!("Whisper inference failed: {e}"))?;

    let mut raw = String::new();
    for segment in state.as_iter() {
        raw.push_str(&segment.to_string());
    }
    let cleaned = clean_transcript(&raw);
    let ms = t0.elapsed().as_millis();
    Ok((cleaned, raw, ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_backend_links_and_rejects_missing_model() {
        let mut params = WhisperContextParameters::default();
        params.use_gpu(true);
        params.flash_attn(true);
        let res = WhisperContext::new_with_params("__nonexistent_whisper_model__.bin", params);
        assert!(res.is_err());
    }
}
