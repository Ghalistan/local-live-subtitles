mod audio;
mod cloud_speech;
mod diarizer;
mod elevenlabs;
mod engine;
mod furigana;
mod gemini;
mod models;
mod pipeline;
mod state;
mod translator;
#[cfg(target_os = "linux")]
mod tray;
mod ui;

use audio::{SpeechChunk, SpeechSegmenter, pcm_i16_le_to_f32};
use cloud_speech::{CloudSpeechEvent, CloudSpeechProvider, CloudSpeechTranscript};
use diarizer::{NemotronDiarizer, SpeakerTimeline};
use elevenlabs::{ElevenLabsModelInfo, fetch_elevenlabs_models};
use engine::{AsrEngine, transcribe};
use furigana::{FuriganaToken, annotate_furigana};
use futures_util::{SinkExt, StreamExt};
use gemini::{
    GeminiLiveAccumulator, GoogleModelInfo, LiveOutputMode, connect_google_live,
    encode_realtime_pcm_chunk, fetch_google_models, transcribe_chunk_with_gemini,
};
use models::{
    HY_MT3_FILENAME, LocalModelFile, delete_hy_mt3_model, delete_local_model,
    detect_local_model_direct_s2tt, ensure_file_by_source, hy_mt3_model_bytes, list_local_models,
};
use pipeline::{
    DiarizationModel, EndpointMechanism, LiveSpeechGate, PipelineConfig, VadModel, VadStage,
    build_segmenter_config, feed_diarizer_hop, resolve_chunk_speakers,
};
use serde::{Deserialize, Serialize};
use state::SharedState;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
use translator::{
    HyMt3Server, ensure_hy_mt3_model, ensure_llama_server_binary, has_llama_server_binary,
};

const DEFAULT_ADDR: &str = "127.0.0.1:8765";
const GPU_BACKEND: &str = if cfg!(target_os = "macos") {
    "metal"
} else {
    "vulkan"
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ClientMessage {
    Ping,
    ListModels,
    ListGoogleModels {
        #[serde(rename = "apiKey")]
        api_key: String,
    },
    ListElevenLabsModels,
    DownloadModel {
        filename: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    DownloadTranslator,
    CancelDownload,
    DeleteModel {
        filename: String,
    },
    DeleteTranslator,
    Start {
        #[serde(default)]
        provider: Option<String>,
        #[serde(default)]
        filename: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        language: String,
        #[serde(rename = "japaneseOnly", default)]
        japanese_only: Option<bool>,
        #[serde(default)]
        translate: bool,
        #[serde(default)]
        diarization: bool,
        #[serde(rename = "apiKey", default)]
        api_key: Option<String>,
        #[serde(rename = "googleModel", default)]
        google_model: Option<GoogleModelInfo>,
        #[serde(rename = "googleTargetLanguage", default)]
        google_target_language: Option<String>,
        #[serde(rename = "elevenLabsModelId", default)]
        eleven_labs_model_id: Option<String>,
        #[serde(rename = "translatorEngine", default)]
        translator_engine: Option<String>,
        #[serde(rename = "translatorTargetLanguage", default)]
        translator_target_language: Option<String>,
        #[serde(rename = "enableVadEndpointing", default)]
        enable_vad_endpointing: Option<bool>,
        #[serde(rename = "vadModel", default)]
        vad_model: Option<String>,
        #[serde(rename = "endpointMechanism", default)]
        endpoint_mechanism: Option<String>,
        #[serde(rename = "diarizationModel", default)]
        diarization_model: Option<String>,
    },
    SetDiarization {
        enabled: bool,
        #[serde(default)]
        model: Option<String>,
    },
    Stop,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum ServerMessage {
    Pong {
        backend: &'static str,
    },
    Models {
        #[serde(rename = "cacheDir")]
        cache_dir: String,
        files: Vec<LocalModelFile>,
        loaded: Vec<String>,
        #[serde(rename = "hyMt3Bytes", skip_serializing_if = "Option::is_none")]
        hy_mt3_bytes: Option<u64>,
        #[serde(rename = "hyMt3Ready")]
        hy_mt3_ready: bool,
    },
    GoogleModels {
        models: Vec<GoogleModelInfo>,
    },
    ElevenLabsModels {
        models: Vec<ElevenLabsModelInfo>,
    },
    Downloaded {
        filename: String,
    },
    Progress {
        progress: Option<u8>,
        label: String,
        #[serde(rename = "bytesPerSec", skip_serializing_if = "Option::is_none")]
        bytes_per_sec: Option<u64>,
    },
    Ready {
        device: &'static str,
        filename: String,
    },
    Activity {
        activity: String,
    },
    Transcript {
        text: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        tokens: Vec<FuriganaToken>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        speakers: Vec<u8>,
        #[serde(rename = "rawText")]
        raw_text: String,
        #[serde(rename = "isFinal")]
        is_final: bool,
        ms: u128,
    },
    Error {
        error: String,
    },
}

struct PendingChunk {
    chunk: SpeechChunk,
    is_final: bool,
}

#[derive(Default)]
struct InferSlot {
    running: bool,
    pending: Option<PendingChunk>,
}

struct TranslateJob {
    text: String,
    speakers: Vec<u8>,
    is_final: bool,
}

#[derive(Default)]
struct TranslateQueue {
    finals: VecDeque<TranslateJob>,
    pending_interim: Option<TranslateJob>,
}

enum ActiveSession {
    LocalWhisper {
        translator_task: Option<tokio::task::JoinHandle<()>>,
    },
    GoogleLive {
        audio_tx: mpsc::UnboundedSender<Vec<u8>>,
        task: tokio::task::JoinHandle<()>,
        translator_task: Option<tokio::task::JoinHandle<()>>,
    },
    GoogleChunked,
    CloudSpeech {
        audio_tx: mpsc::Sender<Vec<u8>>,
        task: tokio::task::JoinHandle<Result<(), String>>,
        translator_task: Option<tokio::task::JoinHandle<()>>,
    },
}

async fn cloud_session_result(session: &mut Option<ActiveSession>) -> Result<(), String> {
    match session {
        Some(ActiveSession::CloudSpeech { task, .. }) => task
            .await
            .map_err(|_| "Cloud transcription worker stopped unexpectedly.".to_string())?,
        _ => std::future::pending().await,
    }
}

fn stop_active_session(session: &mut Option<ActiveSession>) {
    match session.take() {
        Some(ActiveSession::LocalWhisper { translator_task }) => {
            if let Some(t) = translator_task {
                t.abort();
            }
        }
        Some(ActiveSession::GoogleLive {
            audio_tx,
            task,
            translator_task,
        }) => {
            drop(audio_tx);
            task.abort();
            if let Some(t) = translator_task {
                t.abort();
            }
        }
        Some(ActiveSession::CloudSpeech {
            task,
            translator_task,
            ..
        }) => {
            task.abort();
            if let Some(t) = translator_task {
                t.abort();
            }
        }
        Some(ActiveSession::GoogleChunked) | None => {}
    }
}

fn spawn_hy_mt3_worker(
    translator: Arc<HyMt3Server>,
    target_lang: String,
    allow_furigana: bool,
    ui_state: SharedState,
    out_tx: mpsc::UnboundedSender<Message>,
) -> (
    Arc<Mutex<TranslateQueue>>,
    Arc<Notify>,
    tokio::task::JoinHandle<()>,
) {
    let tq = Arc::new(Mutex::new(TranslateQueue::default()));
    let tn = Arc::new(Notify::new());
    let tq_w = Arc::clone(&tq);
    let tn_w = Arc::clone(&tn);

    let handle = tokio::spawn(async move {
        let mut context: VecDeque<String> = VecDeque::with_capacity(3);
        let mut last_interim_at: Option<Instant> = None;
        let mut last_interim_len: usize = 0;
        let mut logged_translations: u8 = 0;

        loop {
            let maybe_job = {
                if let Ok(mut q) = tq_w.lock() {
                    if let Some(f) = q.finals.pop_front() {
                        Some(f)
                    } else if let Some(ref interim) = q.pending_interim {
                        let len = interim.text.chars().count();
                        let elapsed_ok = last_interim_at.is_none_or(|t| {
                            t.elapsed() >= std::time::Duration::from_millis(450)
                        });
                        let len_ok = len.abs_diff(last_interim_len) >= 4;
                        if elapsed_ok && len_ok {
                            q.pending_interim.take()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            };

            let Some(job) = maybe_job else {
                let wait_dur = if let Ok(q) = tq_w.lock()
                    && q.pending_interim.is_some()
                {
                    last_interim_at.map(|t| {
                        std::time::Duration::from_millis(450).saturating_sub(t.elapsed())
                    })
                } else {
                    None
                };
                match wait_dur {
                    Some(d) if !d.is_zero() => {
                        let _ = tokio::time::timeout(d, tn_w.notified()).await;
                    }
                    _ => {
                        tn_w.notified().await;
                    }
                }
                continue;
            };

            let ctx_slice: Vec<String> = context.iter().cloned().collect();
            let started = Instant::now();
            match translator
                .translate(&job.text, &target_lang, &ctx_slice)
                .await
            {
                Ok(translated) => {
                    let ms = started.elapsed().as_millis();
                    if logged_translations < 3
                        || std::env::var_os("LIVE_SUBTITLES_DEBUG_TRANSLATOR").is_some()
                    {
                        eprintln!(
                            "[hy-mt3] ({ms}ms, final={}) {:?} => {:?}",
                            job.is_final, job.text, translated
                        );
                        logged_translations = logged_translations.saturating_add(1);
                    }
                    if job.is_final {
                        last_interim_at = None;
                        last_interim_len = 0;
                        if !translated.is_empty() {
                            if context.len() >= 3 {
                                context.pop_front();
                            }
                            context.push_back(job.text.clone());
                        }
                    } else {
                        last_interim_at = Some(Instant::now());
                        last_interim_len = job.text.chars().count();
                    }

                    if !translated.is_empty() {
                        let tokens = if allow_furigana {
                            annotate_furigana(&translated)
                        } else {
                            Vec::new()
                        };
                        ui_state.push_transcript(
                            translated.clone(),
                            tokens.clone(),
                            job.speakers.clone(),
                            job.is_final,
                            ms,
                        );
                        send_json(
                            &out_tx,
                            &ServerMessage::Transcript {
                                text: translated,
                                tokens,
                                speakers: job.speakers,
                                raw_text: job.text,
                                is_final: job.is_final,
                                ms,
                            },
                        );
                    }
                    if job.is_final {
                        let act = format!("Translated ({ms}ms)");
                        ui_state.set_activity(&act);
                        send_json(&out_tx, &ServerMessage::Activity { activity: act });
                    }
                }
                Err(err) => {
                    eprintln!("[hy-mt3] error: {err}");
                    let act = format!("Hy-MT3 retry: {err}");
                    ui_state.set_activity(&act);
                    send_json(&out_tx, &ServerMessage::Activity { activity: act });
                }
            }
        }
    });

    (tq, tn, handle)
}

async fn prepare_hy_mt3(
    engine: &AsrEngine,
    active_label: &str,
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
) -> Result<Arc<HyMt3Server>, String> {
    ui_state.set_status(
        "Loading",
        Some(active_label.to_string()),
        "Preparing Hy-MT3-1.8B translator...",
    );
    engine
        .get_or_start_hy_mt3(move |progress, label, bytes_per_sec| {
            ui_state.set_activity(label.clone());
            send_json(
                out_tx,
                &ServerMessage::Progress {
                    progress,
                    label,
                    bytes_per_sec,
                },
            );
        })
        .await
}

fn send_live_transcript(
    job: TranslateJob,
    allow_furigana: bool,
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
    translate_sink: Option<(&Mutex<TranslateQueue>, &Notify)>,
) {
    if let Some((queue, notify)) = translate_sink {
        if job.text.is_empty() {
            if job.is_final {
                return;
            }
            if let Ok(mut queue) = queue.lock() {
                queue.pending_interim = None;
            }
        } else {
            if let Ok(mut queue) = queue.lock() {
                if job.is_final {
                    queue.finals.push_back(job);
                    queue.pending_interim = None;
                } else {
                    queue.pending_interim = Some(job);
                }
            }
            notify.notify_one();
            return;
        }
    }

    let TranslateJob {
        text,
        speakers,
        is_final,
    } = job;
    let tokens = if allow_furigana && !text.is_empty() {
        annotate_furigana(&text)
    } else {
        Vec::new()
    };
    ui_state.push_transcript(text.clone(), tokens.clone(), speakers.clone(), is_final, 0);
    send_json(
        out_tx,
        &ServerMessage::Transcript {
            text: text.clone(),
            tokens,
            speakers,
            raw_text: text,
            is_final,
            ms: 0,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_google_live_payload(
    val: &serde_json::Value,
    acc: &mut GeminiLiveAccumulator,
    allow_furigana: bool,
    worker_samples: &AtomicU64,
    worker_timeline: &Mutex<SpeakerTimeline>,
    worker_state: &SharedState,
    worker_tx: &mpsc::UnboundedSender<Message>,
    debug_frames_remaining: &mut u8,
    translate_sink: Option<(&Mutex<TranslateQueue>, &Notify)>,
) -> bool {
    if let Some(err_msg) = val
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        let msg = err_msg.to_string();
        worker_state.set_status("Error", None, &msg);
        send_json(worker_tx, &ServerMessage::Error { error: msg });
        return true;
    }

    if let Some(sc) = val.get("serverContent") {
        let log_all = std::env::var_os("LIVE_SUBTITLES_DEBUG_GEMINI").is_some();
        if *debug_frames_remaining > 0 || log_all {
            let in_txt = sc
                .get("inputTranscription")
                .and_then(|v| v.get("text"))
                .and_then(serde_json::Value::as_str);
            let interim_txt = sc
                .get("interimInputTranscription")
                .and_then(|v| v.get("text"))
                .and_then(serde_json::Value::as_str);
            let out_txt = sc
                .get("outputTranscription")
                .and_then(|v| v.get("text"))
                .and_then(serde_json::Value::as_str);
            let mt_txt: String = sc
                .get("modelTurn")
                .and_then(|v| v.get("parts"))
                .and_then(serde_json::Value::as_array)
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.get("text").and_then(serde_json::Value::as_str))
                        .collect::<String>()
                })
                .unwrap_or_default();
            if in_txt.is_some()
                || interim_txt.is_some()
                || out_txt.is_some()
                || !mt_txt.is_empty()
            {
                eprintln!(
                    "[gemini-live] serverContent: in={in_txt:?} interim={interim_txt:?} out={out_txt:?} modelTurnText={:?}",
                    if mt_txt.is_empty() { None } else { Some(mt_txt.as_str()) }
                );
                if *debug_frames_remaining > 0 {
                    *debug_frames_remaining -= 1;
                }
            }
        }

        let events = acc.push_server_content(sc);
        if !events.is_empty() {
            worker_state.set_activity("Streaming subtitles");
            send_json(
                worker_tx,
                &ServerMessage::Activity {
                    activity: "Streaming subtitles".to_string(),
                },
            );
        }
        let now = worker_samples.load(Ordering::Relaxed);
        let speakers = worker_timeline
            .lock()
            .ok()
            .map(|tl| tl.active_speakers(now.saturating_sub(24_000), now))
            .unwrap_or_default();
        for (text, is_final) in events {
            send_live_transcript(
                TranslateJob { text, speakers: speakers.clone(), is_final },
                allow_furigana,
                worker_state,
                worker_tx,
                translate_sink,
            );
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn stream_cloud_speech<S>(
    provider: CloudSpeechProvider,
    ws: WebSocketStream<S>,
    mut audio_rx: mpsc::Receiver<Vec<u8>>,
    speaker_timeline: &Mutex<SpeakerTimeline>,
    stream_samples: &AtomicU64,
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
    allow_furigana: bool,
    translate_sink: Option<(&Mutex<TranslateQueue>, &Notify)>,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    const CHUNK_BYTES: usize = 4096; // 128 ms of 16 kHz mono PCM16.
    let label = provider.label();
    let (mut sink, mut source) = ws.split();
    let mut transcript = CloudSpeechTranscript::new(provider);
    let mut pcm = Vec::with_capacity(CHUNK_BYTES);
    let mut started_transcribing = false;

    loop {
        tokio::select! {
            bytes = audio_rx.recv() => {
                let Some(bytes) = bytes else { return Ok(()); };
                pcm.extend_from_slice(&bytes);
                let chunks = pcm.chunks_exact(CHUNK_BYTES);
                let sent_bytes = pcm.len() - chunks.remainder().len();
                for chunk in chunks {
                    tokio::time::timeout(Duration::from_secs(5), sink.send(provider.audio_message(chunk)))
                        .await
                        .map_err(|_| format!("{label} timed out receiving audio."))?
                        .map_err(|_| format!("{label} disconnected while receiving audio."))?;
                }
                pcm.drain(..sent_bytes);
            }
            frame = source.next() => {
                let events = match frame {
                    Some(Ok(Message::Text(text))) => transcript.handle(text.as_bytes())?,
                    Some(Ok(Message::Binary(bytes))) => transcript.handle(&bytes)?,
                    Some(Ok(Message::Close(_))) | None => return Err(format!("{label} closed the transcription session.")),
                    Some(Err(_)) => return Err(format!("{label} transcription connection failed.")),
                    _ => continue,
                };
                let now = stream_samples.load(Ordering::Relaxed);
                let speakers = speaker_timeline.lock().ok()
                    .map(|timeline| timeline.active_speakers(now.saturating_sub(24_000), now))
                    .unwrap_or_default();
                for event in events {
                    let (text, is_final) = match event {
                        CloudSpeechEvent::Interim(text) => (text, false),
                        CloudSpeechEvent::Final(text) => (text, true),
                        CloudSpeechEvent::Finished => return Err(format!("{label} ended the transcription session.")),
                    };
                    if !started_transcribing && !text.is_empty() {
                        started_transcribing = true;
                        ui_state.set_activity("Streaming subtitles");
                        send_json(out_tx, &ServerMessage::Activity { activity: "Streaming subtitles".into() });
                    }
                    send_live_transcript(
                        TranslateJob { text, speakers: speakers.clone(), is_final },
                        allow_furigana,
                        ui_state,
                        out_tx,
                        translate_sink,
                    );
                }
            }
        }
    }
}

fn report_session_error(
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
    error: String,
) {
    ui_state.set_status("Error", None, &error);
    send_json(out_tx, &ServerMessage::Error { error });
}

fn send_json(tx: &mpsc::UnboundedSender<Message>, msg: &ServerMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = tx.send(Message::Text(json.into()));
    }
}

enum CancellableOutcome<T> {
    Done(Result<T, String>),
    Cancelled,
    Disconnected,
}

async fn run_cancellable<T, Fut, S>(
    fut: Fut,
    shutdown_rx: &mut tokio::sync::watch::Receiver<u64>,
    ws_source: &mut S,
    should_cancel: fn(ClientMessage) -> bool,
) -> CancellableOutcome<T>
where
    Fut: std::future::Future<Output = Result<T, String>>,
    S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    let mut pinned = std::pin::pin!(fut);
    loop {
        tokio::select! {
            res = &mut pinned => return CancellableOutcome::Done(res),
            _ = shutdown_rx.changed() => return CancellableOutcome::Disconnected,
            next = ws_source.next() => match next {
                Some(Ok(Message::Text(ctrl))) => {
                    if serde_json::from_str(&ctrl).is_ok_and(should_cancel) {
                        return CancellableOutcome::Cancelled;
                    }
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                    return CancellableOutcome::Disconnected;
                }
                _ => {}
            }
        }
    }
}

async fn send_models_list(tx: &mpsc::UnboundedSender<Message>, engine: &AsrEngine) {
    match list_local_models().await {
        Ok((dir, files)) => {
            let loaded = engine.loaded_filenames().await;
            let hy_mt3_bytes = hy_mt3_model_bytes().await;
            let hy_mt3_ready = hy_mt3_bytes.is_some() && has_llama_server_binary();
            send_json(
                tx,
                &ServerMessage::Models {
                    cache_dir: dir.to_string_lossy().into_owned(),
                    files,
                    loaded,
                    hy_mt3_bytes,
                    hy_mt3_ready,
                },
            );
        }
        Err(error) => {
            send_json(tx, &ServerMessage::Error { error });
        }
    }
}

fn enqueue_chunk(
    slot: &Arc<Mutex<InferSlot>>,
    notify: &Arc<Notify>,
    chunk: SpeechChunk,
    is_final: bool,
) {
    if let Ok(mut guard) = slot.lock() {
        let should_replace = match &guard.pending {
            None => true,
            Some(prev) => is_final || !prev.is_final,
        };
        if should_replace {
            guard.pending = Some(PendingChunk { chunk, is_final });
        }
    }
    notify.notify_one();
}

fn stop_diarizer_worker(
    tx: &mut Option<mpsc::UnboundedSender<Vec<f32>>>,
    task: &mut Option<tokio::task::JoinHandle<()>>,
) {
    drop(tx.take());
    if let Some(handle) = task.take() {
        handle.abort();
    }
}

fn spawn_diarizer_worker(
    diarizer: Arc<NemotronDiarizer>,
    timeline: Arc<Mutex<SpeakerTimeline>>,
    base_offset: u64,
) -> (mpsc::UnboundedSender<Vec<f32>>, tokio::task::JoinHandle<()>) {
    diarizer.reset();
    if let Ok(mut guard) = timeline.lock() {
        guard.reset(base_offset);
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<f32>>();
    let handle = tokio::task::spawn_blocking(move || {
        while let Some(mut batch) = rx.blocking_recv() {
            while let Ok(more) = rx.try_recv() {
                batch.extend_from_slice(&more);
            }
            match diarizer.feed(&batch) {
                Ok(segs) if !segs.is_empty() => {
                    if let Ok(mut guard) = timeline.lock() {
                        guard.push_segments(&segs);
                    }
                }
                Err(err) => {
                    eprintln!("[server] Nemotron diarizer step error: {err}");
                }
                _ => {}
            }
        }
    });
    (tx, handle)
}

#[allow(clippy::too_many_arguments)]
async fn maybe_start_diarizer(
    engine: &AsrEngine,
    diar_model: DiarizationModel,
    timeline: &Arc<Mutex<SpeakerTimeline>>,
    base_offset: u64,
    diar_tx: &mut Option<mpsc::UnboundedSender<Vec<f32>>>,
    diar_task: &mut Option<tokio::task::JoinHandle<()>>,
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
) {
    if diar_model == DiarizationModel::Disabled {
        return;
    }
    let tx_diar = out_tx.clone();
    let state_diar = ui_state.clone();
    match engine
        .get_or_load_diarizer(move |progress, label, bytes_per_sec| {
            state_diar.set_activity(label.clone());
            send_json(
                &tx_diar,
                &ServerMessage::Progress {
                    progress,
                    label,
                    bytes_per_sec,
                },
            );
        })
        .await
    {
        Ok(diarizer) => {
            let (tx, task) = spawn_diarizer_worker(diarizer, Arc::clone(timeline), base_offset);
            *diar_tx = Some(tx);
            *diar_task = Some(task);
        }
        Err(err) => {
            eprintln!("[server] Speaker diarizer unavailable: {err}");
        }
    }
}

fn update_speech_indicator(
    speaking: bool,
    speech_active: &mut bool,
    infer_slot: &Arc<Mutex<InferSlot>>,
    ui_state: &SharedState,
    out_tx: &mpsc::UnboundedSender<Message>,
) {
    if speaking == *speech_active {
        return;
    }
    *speech_active = speaking;
    let busy = infer_slot.lock().map(|g| g.running).unwrap_or(false);
    if !busy {
        let label = if speaking {
            "Hearing speech..."
        } else {
            "Waiting for speech..."
        };
        ui_state.set_activity(label);
        send_json(
            out_tx,
            &ServerMessage::Activity {
                activity: label.to_string(),
            },
        );
    }
}

async fn handle_client(
    stream: TcpStream,
    engine: Arc<AsrEngine>,
    ui_state: SharedState,
    mut shutdown_rx: tokio::sync::watch::Receiver<u64>,
) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(err) => {
            eprintln!("[server] WebSocket handshake failed: {err}");
            return;
        }
    };

    let (mut ws_sink, mut ws_source) = ws_stream.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();

    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut active_session: Option<ActiveSession> = None;
    let mut pipeline_cfg = PipelineConfig::default();
    let mut vad_stage = VadStage::new(pipeline_cfg.enable_vad_endpointing, pipeline_cfg.vad_model, None);
    let mut live_gate = LiveSpeechGate::default();
    let speaker_timeline = Arc::new(Mutex::new(SpeakerTimeline::default()));
    let mut diar_tx: Option<mpsc::UnboundedSender<Vec<f32>>> = None;
    let mut diar_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut segmenter = SpeechSegmenter::new(build_segmenter_config(&pipeline_cfg, false));
    let mut speech_active = false;
    let stream_samples = Arc::new(AtomicU64::new(0));

    let infer_slot = Arc::new(Mutex::new(InferSlot::default()));
    let infer_notify = Arc::new(Notify::new());
    let mut infer_handle: Option<tokio::task::JoinHandle<()>> = None;

    'client: loop {
        let incoming = tokio::select! {
            _ = shutdown_rx.changed() => break,
            result = cloud_session_result(&mut active_session) => {
                stop_active_session(&mut active_session);
                stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                if let Err(error) = result {
                    report_session_error(&ui_state, &out_tx, error);
                }
                continue;
            }
            next = ws_source.next() => match next {
                Some(item) => item,
                None => break,
            },
        };
        let msg = match incoming {
            Ok(m) => m,
            Err(_) => break,
        };

        match msg {
            Message::Text(text) => {
                let parsed: Result<ClientMessage, _> = serde_json::from_str(&text);
                match parsed {
                    Ok(ClientMessage::Ping) => {
                        send_json(&out_tx, &ServerMessage::Pong { backend: GPU_BACKEND });
                    }
                    Ok(ClientMessage::ListModels) => {
                        send_models_list(&out_tx, &engine).await;
                    }
                    Ok(ClientMessage::ListGoogleModels { api_key }) => {
                        match fetch_google_models(&api_key).await {
                            Ok(models) => send_json(&out_tx, &ServerMessage::GoogleModels { models }),
                            Err(error) => send_json(&out_tx, &ServerMessage::Error { error }),
                        }
                    }
                    Ok(ClientMessage::ListElevenLabsModels) => {
                        match fetch_elevenlabs_models().await {
                            Ok(models) => send_json(&out_tx, &ServerMessage::ElevenLabsModels { models }),
                            Err(error) => send_json(&out_tx, &ServerMessage::Error { error }),
                        }
                    }
                    Ok(ClientMessage::DownloadModel { filename, repo, url }) => {
                        let tx_prog = out_tx.clone();
                        let fut = ensure_file_by_source(
                            &filename,
                            repo.as_deref(),
                            url.as_deref(),
                            move |progress, label, bytes_per_sec| {
                                send_json(
                                    &tx_prog,
                                    &ServerMessage::Progress {
                                        progress,
                                        label,
                                        bytes_per_sec,
                                    },
                                );
                            },
                        );
                        match run_cancellable(fut, &mut shutdown_rx, &mut ws_source, |msg| {
                            matches!(msg, ClientMessage::CancelDownload)
                        }).await {
                            CancellableOutcome::Disconnected => break 'client,
                            CancellableOutcome::Cancelled => continue,
                            CancellableOutcome::Done(Ok(_)) => {
                                send_json(&out_tx, &ServerMessage::Downloaded { filename });
                                send_models_list(&out_tx, &engine).await;
                            }
                            CancellableOutcome::Done(Err(error)) => {
                                send_json(&out_tx, &ServerMessage::Error { error });
                            }
                        }
                    }
                    Ok(ClientMessage::DownloadTranslator) => {
                        let tx_prog = out_tx.clone();
                        let fut = async move {
                            let mut emit = move |progress, label, bytes_per_sec| {
                                send_json(
                                    &tx_prog,
                                    &ServerMessage::Progress {
                                        progress,
                                        label,
                                        bytes_per_sec,
                                    },
                                );
                            };
                            ensure_llama_server_binary(&mut emit).await?;
                            ensure_hy_mt3_model(&mut emit).await?;
                            Ok::<_, String>(())
                        };
                        match run_cancellable(fut, &mut shutdown_rx, &mut ws_source, |msg| {
                            matches!(msg, ClientMessage::CancelDownload)
                        }).await {
                            CancellableOutcome::Disconnected => break 'client,
                            CancellableOutcome::Cancelled => continue,
                            CancellableOutcome::Done(Ok(())) => {
                                send_json(
                                    &out_tx,
                                    &ServerMessage::Downloaded {
                                        filename: HY_MT3_FILENAME.to_string(),
                                    },
                                );
                                send_models_list(&out_tx, &engine).await;
                            }
                            CancellableOutcome::Done(Err(error)) => {
                                send_json(&out_tx, &ServerMessage::Error { error });
                            }
                        }
                    }
                    Ok(ClientMessage::CancelDownload) => {}
                    Ok(ClientMessage::DeleteModel { filename }) => {
                        engine.unload_model(&filename).await;
                        match delete_local_model(&filename).await {
                            Ok(()) => send_models_list(&out_tx, &engine).await,
                            Err(error) => send_json(&out_tx, &ServerMessage::Error { error }),
                        }
                    }
                    Ok(ClientMessage::DeleteTranslator) => {
                        engine.unload_hy_mt3().await;
                        match delete_hy_mt3_model().await {
                            Ok(()) => send_models_list(&out_tx, &engine).await,
                            Err(error) => send_json(&out_tx, &ServerMessage::Error { error }),
                        }
                    }
                    Ok(ClientMessage::Start {
                        provider,
                        filename,
                        repo,
                        url,
                        language,
                        japanese_only,
                        translate,
                        diarization,
                        api_key,
                        google_model,
                        google_target_language,
                        eleven_labs_model_id,
                        translator_engine,
                        translator_target_language,
                        enable_vad_endpointing,
                        vad_model,
                        endpoint_mechanism,
                        diarization_model,
                    }) => {
                        if let Some(handle) = infer_handle.take() {
                            handle.abort();
                        }
                        stop_active_session(&mut active_session);
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        if let Ok(mut guard) = infer_slot.lock() {
                            guard.running = false;
                            guard.pending = None;
                        }
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(0);
                        }
                        vad_stage.reset();
                        live_gate.reset();
                        speech_active = false;
                        stream_samples.store(0, Ordering::Relaxed);

                        let cloud_provider = provider.as_deref().and_then(CloudSpeechProvider::from_id);
                        pipeline_cfg = PipelineConfig {
                            enable_vad_endpointing: cloud_provider.is_none()
                                && enable_vad_endpointing.unwrap_or(true),
                            vad_model: VadModel::parse(vad_model.as_deref()),
                            endpoint_mechanism: EndpointMechanism::parse(endpoint_mechanism.as_deref()),
                            diarization_model: DiarizationModel::parse(
                                diarization_model.as_deref(),
                                diarization,
                            ),
                        };
                        eprintln!(
                            "[pipeline] vad_ep={} vad={:?} endpoint={:?} diar={:?}",
                            pipeline_cfg.enable_vad_endpointing,
                            pipeline_cfg.vad_model,
                            pipeline_cfg.endpoint_mechanism,
                            pipeline_cfg.diarization_model
                        );

                        let hy_mt3_target = translator_target_language
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty() && *s != "same")
                            .unwrap_or("en")
                            .to_string();

                        if let Some(cloud_provider) = cloud_provider {
                            let label = cloud_provider.label();
                            let key = api_key.as_deref().unwrap_or_default().trim();
                            if key.is_empty() {
                                report_session_error(
                                    &ui_state,
                                    &out_tx,
                                    format!(
                                        "Set your {label} API key in Settings before starting."
                                    ),
                                );
                                continue;
                            }

                            let model_id = match cloud_provider.model_id(eleven_labs_model_id.as_deref()) {
                                Ok(id) => id,
                                Err(error) => {
                                    report_session_error(&ui_state, &out_tx, error);
                                    continue;
                                }
                            };
                            let use_hy_mt3 = translator_engine.as_deref() == Some("hy-mt3");
                            let start = async {
                                let hy_mt3_server = if use_hy_mt3 {
                                    Some(prepare_hy_mt3(&engine, label, &ui_state, &out_tx).await?)
                                } else {
                                    None
                                };
                                maybe_start_diarizer(
                                    &engine,
                                    pipeline_cfg.diarization_model,
                                    &speaker_timeline,
                                    0,
                                    &mut diar_tx,
                                    &mut diar_task,
                                    &ui_state,
                                    &out_tx,
                                )
                                .await;
                                let connecting = format!("Connecting to {label}...");
                                ui_state.set_status("Loading", Some(label.into()), &connecting);
                                send_json(
                                    &out_tx,
                                    &ServerMessage::Progress {
                                        progress: None,
                                        label: connecting,
                                        bytes_per_sec: None,
                                    },
                                );
                                let ws = cloud_provider.connect(key, &language, model_id).await?;
                                Ok((ws, hy_mt3_server))
                            };
                            let (ws, hy_mt3_server) = match run_cancellable(
                                start,
                                &mut shutdown_rx,
                                &mut ws_source,
                                |msg| matches!(msg, ClientMessage::Stop),
                            )
                            .await
                            {
                                CancellableOutcome::Done(Ok(session)) => session,
                                CancellableOutcome::Done(Err(error)) => {
                                    stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                                    report_session_error(&ui_state, &out_tx, error);
                                    continue;
                                }
                                CancellableOutcome::Cancelled => {
                                    stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                                    ui_state.set_status(
                                        "Idle",
                                        None,
                                        "Waiting for browser extension...",
                                    );
                                    continue;
                                }
                                CancellableOutcome::Disconnected => {
                                    ui_state.set_status(
                                        "Idle",
                                        None,
                                        "Waiting for browser extension...",
                                    );
                                    break 'client;
                                }
                            };
                            let allow_furigana = !use_hy_mt3 || hy_mt3_target == "ja";
                            let (translate_queue, translate_notify, translator_task) =
                                if let Some(server) = hy_mt3_server {
                                    let (queue, notify, task) = spawn_hy_mt3_worker(
                                        server,
                                        hy_mt3_target,
                                        allow_furigana,
                                        ui_state.clone(),
                                        out_tx.clone(),
                                    );
                                    (Some(queue), Some(notify), Some(task))
                                } else {
                                    (None, None, None)
                                };
                            let (audio_tx, audio_rx) = mpsc::channel(64);
                            let worker_state = ui_state.clone();
                            let worker_tx = out_tx.clone();
                            let worker_timeline = Arc::clone(&speaker_timeline);
                            let worker_samples = Arc::clone(&stream_samples);
                            ui_state.set_status(
                                "Listening",
                                Some(label.into()),
                                "Waiting for speech...",
                            );
                            send_json(
                                &out_tx,
                                &ServerMessage::Ready {
                                    device: cloud_provider.id(),
                                    filename: model_id.into(),
                                },
                            );
                            let task = tokio::spawn(async move {
                                let translate_sink =
                                    translate_queue.as_deref().zip(translate_notify.as_deref());
                                stream_cloud_speech(
                                    cloud_provider,
                                    ws,
                                    audio_rx,
                                    &worker_timeline,
                                    &worker_samples,
                                    &worker_state,
                                    &worker_tx,
                                    allow_furigana,
                                    translate_sink,
                                )
                                .await
                            });
                            active_session = Some(ActiveSession::CloudSpeech {
                                audio_tx,
                                task,
                                translator_task,
                            });
                            continue;
                        }

                        if provider.as_deref() == Some("google") {
                            let key = api_key.unwrap_or_default().trim().to_string();
                            if key.is_empty() {
                                let msg = "Set your Gemini API key in Settings before starting.".to_string();
                                ui_state.set_status("Error", None, &msg);
                                send_json(&out_tx, &ServerMessage::Error { error: msg });
                                continue;
                            }
                            let Some(g_model) = google_model else {
                                let msg = "Load and select a Google model before starting.".to_string();
                                ui_state.set_status("Error", None, &msg);
                                send_json(&out_tx, &ServerMessage::Error { error: msg });
                                continue;
                            };

                            let vad_ctx = if pipeline_cfg.enable_vad_endpointing
                                && pipeline_cfg.vad_model == VadModel::SileroV5
                            {
                                let tx_vad = out_tx.clone();
                                engine
                                    .get_or_load_vad(move |progress, label, bytes_per_sec| {
                                        send_json(
                                            &tx_vad,
                                            &ServerMessage::Progress {
                                                progress,
                                                label,
                                                bytes_per_sec,
                                            },
                                        );
                                    })
                                    .await
                            } else {
                                None
                            };
                            vad_stage.configure(
                                pipeline_cfg.enable_vad_endpointing,
                                pipeline_cfg.vad_model,
                                vad_ctx,
                            );

                            maybe_start_diarizer(
                                &engine,
                                pipeline_cfg.diarization_model,
                                &speaker_timeline,
                                0,
                                &mut diar_tx,
                                &mut diar_task,
                                &ui_state,
                                &out_tx,
                            )
                            .await;

                            let target_lang = google_target_language
                                .unwrap_or_else(|| "same".to_string());
                            let use_hy_mt3 = (g_model.kind == "transcribe-live"
                                || g_model.kind == "transcribe")
                                && translator_engine.as_deref() == Some("hy-mt3");
                            let source_lang = if language.is_empty() {
                                "auto".to_string()
                            } else {
                                language.clone()
                            };
                            let allow_furigana = if use_hy_mt3 {
                                hy_mt3_target == "ja"
                            } else if g_model.kind == "transcribe-live" || g_model.kind == "transcribe" {
                                true
                            } else {
                                target_lang == "same" || target_lang == "ja"
                            };
                            let active_label = format!("Google · {}", g_model.display_name);

                            let hy_mt3_server = if use_hy_mt3 {
                                match prepare_hy_mt3(&engine, &active_label, &ui_state, &out_tx).await {
                                    Ok(server) => Some(server),
                                    Err(error) => {
                                        report_session_error(&ui_state, &out_tx, error);
                                        continue;
                                    }
                                }
                            } else {
                                None
                            };

                            eprintln!(
                                "[gemini] start: model={} kind={} transport={} lang={} target={} translator={}",
                                g_model.id,
                                g_model.kind,
                                g_model.transport,
                                source_lang,
                                if use_hy_mt3 { &hy_mt3_target } else { &target_lang },
                                if use_hy_mt3 { "hy-mt3" } else { "disabled" }
                            );

                            if g_model.transport == "live" {
                                ui_state.set_status(
                                    "Loading",
                                    Some(active_label.clone()),
                                    "Connecting to Google Gemini...",
                                );
                                send_json(
                                    &out_tx,
                                    &ServerMessage::Progress {
                                        progress: None,
                                        label: "Connecting to Google Gemini...".to_string(),
                                        bytes_per_sec: None,
                                    },
                                );

                                match connect_google_live(&key, &g_model, &language, &target_lang).await {
                                    Ok(ws) => {
                                        let (mut g_sink, mut g_stream) = ws.split();
                                        let (audio_tx, mut audio_rx) = mpsc::unbounded_channel::<Vec<u8>>();
                                        let output_mode = if g_model.kind == "translate-live" {
                                            LiveOutputMode::OutputOnly
                                        } else if g_model.kind == "live" && target_lang != "same" {
                                            LiveOutputMode::OutputOrModelTurn
                                        } else {
                                            LiveOutputMode::Input
                                        };
                                        let worker_tx = out_tx.clone();
                                        let worker_state = ui_state.clone();
                                        let worker_timeline = Arc::clone(&speaker_timeline);
                                        let worker_samples = Arc::clone(&stream_samples);

                                        let (translate_queue, translate_notify, translator_task) =
                                            if let Some(ref server) = hy_mt3_server {
                                                let (tq, tn, handle) = spawn_hy_mt3_worker(
                                                    Arc::clone(server),
                                                    hy_mt3_target.clone(),
                                                    allow_furigana,
                                                    ui_state.clone(),
                                                    out_tx.clone(),
                                                );
                                                (Some(tq), Some(tn), Some(handle))
                                            } else {
                                                (None, None, None)
                                            };

                                        let task = tokio::spawn(async move {
                                            let mut acc = GeminiLiveAccumulator::new(output_mode, 140);
                                            let mut pcm_buf: Vec<u8> = Vec::with_capacity(8192);
                                            let mut debug_frames_remaining: u8 = 3;

                                            loop {
                                                tokio::select! {
                                                    maybe_bytes = audio_rx.recv() => {
                                                        match maybe_bytes {
                                                            Some(bytes) => {
                                                                pcm_buf.extend_from_slice(&bytes);
                                                                while let Ok(more) = audio_rx.try_recv() {
                                                                    pcm_buf.extend_from_slice(&more);
                                                                    if pcm_buf.len() >= 16384 {
                                                                        break;
                                                                    }
                                                                }
                                                                if pcm_buf.len() >= 4096 {
                                                                    let payload = encode_realtime_pcm_chunk(&pcm_buf);
                                                                    pcm_buf.clear();
                                                                    if g_sink.send(Message::Text(payload.into())).await.is_err() {
                                                                        break;
                                                                    }
                                                                }
                                                            }
                                                            None => {
                                                                if !pcm_buf.is_empty() {
                                                                    let payload = encode_realtime_pcm_chunk(&pcm_buf);
                                                                    pcm_buf.clear();
                                                                    let _ = g_sink.send(Message::Text(payload.into())).await;
                                                                }
                                                                let _ = g_sink.close().await;
                                                                break;
                                                            }
                                                        }
                                                    }
                                                    frame = g_stream.next() => {
                                                        let t_sink = translate_queue
                                                            .as_deref()
                                                            .zip(translate_notify.as_deref());
                                                        match frame {
                                                            Some(Ok(Message::Text(txt))) => {
                                                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(txt.as_str())
                                                                    && handle_google_live_payload(
                                                                        &val,
                                                                        &mut acc,
                                                                        allow_furigana,
                                                                        &worker_samples,
                                                                        &worker_timeline,
                                                                        &worker_state,
                                                                        &worker_tx,
                                                                        &mut debug_frames_remaining,
                                                                        t_sink,
                                                                    )
                                                                {
                                                                    break;
                                                                }
                                                            }
                                                            Some(Ok(Message::Binary(bytes))) => {
                                                                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&bytes)
                                                                    && handle_google_live_payload(
                                                                        &val,
                                                                        &mut acc,
                                                                        allow_furigana,
                                                                        &worker_samples,
                                                                        &worker_timeline,
                                                                        &worker_state,
                                                                        &worker_tx,
                                                                        &mut debug_frames_remaining,
                                                                        t_sink,
                                                                    )
                                                                {
                                                                    break;
                                                                }
                                                            }
                                                            Some(Ok(Message::Close(frame))) => {
                                                                let msg = match frame {
                                                                    Some(f) if !f.reason.is_empty() => format!("Google Gemini Live closed [{}] ({})", f.code, f.reason),
                                                                    Some(f) => format!("Google Gemini Live closed [{}].", f.code),
                                                                    None => "Google Gemini Live connection closed.".to_string(),
                                                                };
                                                                worker_state.set_status("Error", None, &msg);
                                                                send_json(&worker_tx, &ServerMessage::Error { error: msg });
                                                                break;
                                                            }
                                                            Some(Ok(_)) => {}
                                                            Some(Err(err)) => {
                                                                let msg = format!("Google Gemini Live error: {err}");
                                                                worker_state.set_status("Error", None, &msg);
                                                                send_json(&worker_tx, &ServerMessage::Error { error: msg });
                                                                break;
                                                            }
                                                            None => {
                                                                let msg = "Google Gemini Live connection closed.".to_string();
                                                                worker_state.set_status("Error", None, &msg);
                                                                send_json(&worker_tx, &ServerMessage::Error { error: msg });
                                                                break;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        });

                                        active_session = Some(ActiveSession::GoogleLive {
                                            audio_tx,
                                            task,
                                            translator_task,
                                        });
                                        ui_state.set_status(
                                            "Listening",
                                            Some(active_label),
                                            "Waiting for speech...",
                                        );
                                        send_json(
                                            &out_tx,
                                            &ServerMessage::Ready {
                                                device: "google",
                                                filename: g_model.id,
                                            },
                                        );
                                    }
                                    Err(error) => {
                                        ui_state.set_status("Error", None, &error);
                                        send_json(&out_tx, &ServerMessage::Error { error });
                                    }
                                }
                            } else {
                                segmenter = SpeechSegmenter::new(build_segmenter_config(&pipeline_cfg, true));

                                let worker_slot = Arc::clone(&infer_slot);
                                let worker_notify = Arc::clone(&infer_notify);
                                let worker_tx = out_tx.clone();
                                let worker_state = ui_state.clone();
                                let worker_timeline = Arc::clone(&speaker_timeline);
                                let worker_model = g_model.clone();
                                let worker_lang = if language.is_empty() {
                                    "auto".to_string()
                                } else {
                                    language.clone()
                                };

                                infer_handle = Some(tokio::spawn(async move {
                                    let http_client = reqwest::Client::builder()
                                        .timeout(std::time::Duration::from_secs(30))
                                        .build()
                                        .unwrap_or_default();
                                    let mut last_context = String::new();
                                    let mut translate_ctx: VecDeque<String> =
                                        VecDeque::with_capacity(3);
                                    let chunk_cascade = hy_mt3_server.is_some()
                                        && worker_model.kind == "transcribe";
                                    let chunk_target_lang = if worker_model.kind == "transcribe" {
                                        "same".to_string()
                                    } else {
                                        target_lang.clone()
                                    };

                                    loop {
                                        let next = {
                                            let mut guard = worker_slot.lock().ok();
                                            match guard.as_mut() {
                                                Some(g) => {
                                                    if let Some(chunk) = g.pending.take() {
                                                        g.running = true;
                                                        Some(chunk)
                                                    } else {
                                                        g.running = false;
                                                        None
                                                    }
                                                }
                                                None => None,
                                            }
                                        };

                                        let Some(pending) = next else {
                                            worker_notify.notified().await;
                                            continue;
                                        };

                                        let sample_len = pending.chunk.samples.len();
                                        let sec = sample_len as f32 / 16000.0;
                                        let act = if chunk_cascade || chunk_target_lang != "same" {
                                            format!("Translating ({sec:.1}s)...")
                                        } else {
                                            format!("Transcribing ({sec:.1}s)...")
                                        };
                                        worker_state.set_activity(&act);
                                        send_json(&worker_tx, &ServerMessage::Activity { activity: act });

                                        let started = Instant::now();
                                        let start_sample = pending.chunk.start_sample;
                                        let end_sample = pending.chunk.end_sample;

                                        let asr_fut = transcribe_chunk_with_gemini(
                                            &http_client,
                                            &key,
                                            &worker_model,
                                            &pending.chunk.samples,
                                            &worker_lang,
                                            &chunk_target_lang,
                                            &last_context,
                                            |interim_text| {
                                                if chunk_cascade {
                                                    return;
                                                }
                                                let speakers = worker_timeline
                                                    .lock()
                                                    .ok()
                                                    .map(|tl| tl.active_speakers(start_sample, end_sample))
                                                    .unwrap_or_default();
                                                let tokens = if allow_furigana {
                                                    annotate_furigana(interim_text)
                                                } else {
                                                    Vec::new()
                                                };
                                                worker_state.push_transcript(
                                                    interim_text.to_string(),
                                                    tokens.clone(),
                                                    speakers.clone(),
                                                    false,
                                                    0,
                                                );
                                                send_json(
                                                    &worker_tx,
                                                    &ServerMessage::Transcript {
                                                        text: interim_text.to_string(),
                                                        tokens,
                                                        speakers,
                                                        raw_text: interim_text.to_string(),
                                                        is_final: false,
                                                        ms: 0,
                                                    },
                                                );
                                            },
                                        );
                                        let diar_fut = resolve_chunk_speakers(
                                            Arc::clone(&worker_timeline),
                                            start_sample,
                                            end_sample,
                                        );
                                        let (res, speakers) = tokio::join!(asr_fut, diar_fut);

                                        if let Ok(mut guard) = worker_slot.lock() {
                                            guard.running = false;
                                        }

                                        let final_res = match res {
                                            Ok(src_text) if chunk_cascade && !src_text.is_empty() => {
                                                if let Some(ref server) = hy_mt3_server {
                                                    let ctx_slice: Vec<String> =
                                                        translate_ctx.iter().cloned().collect();
                                                    match server
                                                        .translate(&src_text, &hy_mt3_target, &ctx_slice)
                                                        .await
                                                    {
                                                        Ok(translated) => {
                                                            if !translated.is_empty() {
                                                                if translate_ctx.len() >= 3 {
                                                                    translate_ctx.pop_front();
                                                                }
                                                                translate_ctx.push_back(src_text.clone());
                                                            }
                                                            Ok((translated, src_text))
                                                        }
                                                        Err(err) => Err(err),
                                                    }
                                                } else {
                                                    Ok((src_text.clone(), src_text))
                                                }
                                            }
                                            Ok(text) => Ok((text.clone(), text)),
                                            Err(err) => Err(err),
                                        };

                                        let ms = started.elapsed().as_millis();
                                        match final_res {
                                            Ok((text, raw_text)) => {
                                                if !text.is_empty() {
                                                    let tail: String = text
                                                        .chars()
                                                        .rev()
                                                        .take(160)
                                                        .collect::<Vec<_>>()
                                                        .into_iter()
                                                        .rev()
                                                        .collect();
                                                    last_context = tail;
                                                    let tokens = if allow_furigana {
                                                        annotate_furigana(&text)
                                                    } else {
                                                        Vec::new()
                                                    };
                                                    worker_state.push_transcript(
                                                        text.clone(),
                                                        tokens.clone(),
                                                        speakers.clone(),
                                                        true,
                                                        ms,
                                                    );
                                                    send_json(
                                                        &worker_tx,
                                                        &ServerMessage::Transcript {
                                                            text,
                                                            tokens,
                                                            speakers,
                                                            raw_text,
                                                            is_final: true,
                                                            ms,
                                                        },
                                                    );
                                                }
                                                let done_act = format!("Last chunk: {ms}ms");
                                                worker_state.set_activity(&done_act);
                                                send_json(
                                                    &worker_tx,
                                                    &ServerMessage::Activity { activity: done_act },
                                                );
                                            }
                                            Err(error) => {
                                                worker_state.set_status("Error", None, &error);
                                                send_json(&worker_tx, &ServerMessage::Error { error });
                                            }
                                        }
                                    }
                                }));

                                active_session = Some(ActiveSession::GoogleChunked);
                                ui_state.set_status(
                                    "Listening",
                                    Some(active_label),
                                    "Waiting for speech...",
                                );
                                send_json(
                                    &out_tx,
                                    &ServerMessage::Ready {
                                        device: "google",
                                        filename: g_model.id,
                                    },
                                );
                            }
                            continue;
                        }

                        segmenter = SpeechSegmenter::new(build_segmenter_config(&pipeline_cfg, false));

                        let force_ja = japanese_only
                            .unwrap_or_else(|| filename.to_ascii_lowercase().contains("kotoba"));
                        let active_language = if force_ja {
                            Some("ja".to_string())
                        } else if language == "auto" {
                            None
                        } else {
                            Some(language)
                        };

                        let vad_ctx = if pipeline_cfg.enable_vad_endpointing
                            && pipeline_cfg.vad_model == VadModel::SileroV5
                        {
                            let tx_vad = out_tx.clone();
                            engine
                                .get_or_load_vad(move |progress, label, bytes_per_sec| {
                                    send_json(
                                        &tx_vad,
                                        &ServerMessage::Progress {
                                            progress,
                                            label,
                                            bytes_per_sec,
                                        },
                                    );
                                })
                                .await
                        } else {
                            None
                        };
                        vad_stage.configure(
                            pipeline_cfg.enable_vad_endpointing,
                            pipeline_cfg.vad_model,
                            vad_ctx,
                        );

                        maybe_start_diarizer(
                            &engine,
                            pipeline_cfg.diarization_model,
                            &speaker_timeline,
                            0,
                            &mut diar_tx,
                            &mut diar_task,
                            &ui_state,
                            &out_tx,
                        )
                        .await;

                        let tx_model = out_tx.clone();
                        let state_progress = ui_state.clone();
                        ui_state.set_status("Loading", Some(filename.clone()), "Loading model...");
                        match engine
                            .get_or_load_model(
                                &filename,
                                repo.as_deref(),
                                url.as_deref(),
                                move |progress, label, bytes_per_sec| {
                                    state_progress.set_activity(label.clone());
                                    send_json(
                                        &tx_model,
                                        &ServerMessage::Progress {
                                            progress,
                                            label,
                                            bytes_per_sec,
                                        },
                                    );
                                },
                            )
                            .await
                        {
                            Ok((loaded_filename, model)) => {
                                let model_supports_s2tt = detect_local_model_direct_s2tt(
                                    &models::cache_dir().join(&loaded_filename),
                                    &loaded_filename,
                                )
                                .await
                                .unwrap_or(true);
                                let use_hy_mt3 = !model_supports_s2tt
                                    && translator_engine.as_deref() == Some("hy-mt3");

                                let (translate_queue, translate_notify, translator_task) =
                                    if use_hy_mt3 {
                                        match prepare_hy_mt3(
                                            &engine, &loaded_filename, &ui_state, &out_tx,
                                        ).await {
                                            Ok(server) => {
                                                let (tq, tn, handle) = spawn_hy_mt3_worker(
                                                    server,
                                                    hy_mt3_target.clone(),
                                                    hy_mt3_target == "ja",
                                                    ui_state.clone(),
                                                    out_tx.clone(),
                                                );
                                                (Some(tq), Some(tn), Some(handle))
                                            }
                                            Err(error) => {
                                                report_session_error(&ui_state, &out_tx, error);
                                                continue;
                                            }
                                        }
                                    } else {
                                        (None, None, None)
                                    };

                                active_session = Some(ActiveSession::LocalWhisper {
                                    translator_task,
                                });
                                ui_state.set_status(
                                    "Listening",
                                    Some(loaded_filename.clone()),
                                    "Waiting for speech...",
                                );
                                let worker_translate = translate
                                    && !force_ja
                                    && model_supports_s2tt
                                    && active_language.as_deref() != Some("en");

                                let worker_slot = Arc::clone(&infer_slot);
                                let worker_notify = Arc::clone(&infer_notify);
                                let worker_tx = out_tx.clone();
                                let worker_state = ui_state.clone();
                                let worker_lang = active_language;
                                let worker_timeline = Arc::clone(&speaker_timeline);

                                infer_handle = Some(tokio::spawn(async move {
                                    loop {
                                        let next = {
                                            let mut guard = worker_slot.lock().ok();
                                            match guard.as_mut() {
                                                Some(g) => {
                                                    if let Some(chunk) = g.pending.take() {
                                                        g.running = true;
                                                        Some(chunk)
                                                    } else {
                                                        g.running = false;
                                                        None
                                                    }
                                                }
                                                None => None,
                                            }
                                        };

                                        let Some(pending) = next else {
                                            worker_notify.notified().await;
                                            continue;
                                        };

                                        let sample_len = pending.chunk.samples.len();
                                        let sec = sample_len as f32 / 16000.0;
                                        let act = if worker_translate {
                                            format!("Translating ({sec:.1}s)...")
                                        } else {
                                            format!("Transcribing ({sec:.1}s)...")
                                        };
                                        worker_state.set_activity(&act);
                                        send_json(
                                            &worker_tx,
                                            &ServerMessage::Activity { activity: act },
                                        );

                                        let samples = pending.chunk.samples;
                                        let model_clone = Arc::clone(&model);
                                        let lang_clone = worker_lang.clone();
                                        let skip_local_furigana =
                                            worker_translate || translate_queue.is_some();
                                        let asr_fut = tokio::task::spawn_blocking(move || {
                                            transcribe(
                                                &model_clone,
                                                &samples,
                                                lang_clone.as_deref(),
                                                worker_translate,
                                            )
                                            .map(|(text, raw_text, ms)| {
                                                let tokens = if text.is_empty() || skip_local_furigana {
                                                    Vec::new()
                                                } else {
                                                    annotate_furigana(&text)
                                                };
                                                (text, tokens, raw_text, ms)
                                            })
                                        });
                                        let diar_fut = resolve_chunk_speakers(
                                            Arc::clone(&worker_timeline),
                                            pending.chunk.start_sample,
                                            pending.chunk.end_sample,
                                        );
                                        let (asr_res, speakers) = tokio::join!(asr_fut, diar_fut);

                                        if let Ok(mut guard) = worker_slot.lock() {
                                            guard.running = false;
                                        }

                                        match asr_res {
                                            Ok(Ok((text, tokens, raw_text, ms))) => {
                                                if let (Some(tq), Some(tn)) = (
                                                    translate_queue.as_deref(),
                                                    translate_notify.as_deref(),
                                                ) {
                                                    if !text.is_empty() {
                                                        if let Ok(mut q) = tq.lock() {
                                                            let job = TranslateJob {
                                                                text,
                                                                speakers,
                                                                is_final: pending.is_final,
                                                            };
                                                            if pending.is_final {
                                                                q.finals.push_back(job);
                                                                q.pending_interim = None;
                                                            } else {
                                                                q.pending_interim = Some(job);
                                                            }
                                                        }
                                                        tn.notify_one();
                                                    }
                                                } else {
                                                    if !text.is_empty() {
                                                        worker_state.push_transcript(
                                                            text.clone(),
                                                            tokens.clone(),
                                                            speakers.clone(),
                                                            pending.is_final,
                                                            ms,
                                                        );
                                                        send_json(
                                                            &worker_tx,
                                                            &ServerMessage::Transcript {
                                                                text,
                                                                tokens,
                                                                speakers,
                                                                raw_text,
                                                                is_final: pending.is_final,
                                                                ms,
                                                            },
                                                        );
                                                    }
                                                    let done_act = format!("Last chunk: {ms}ms");
                                                    worker_state.set_activity(&done_act);
                                                    send_json(
                                                        &worker_tx,
                                                        &ServerMessage::Activity { activity: done_act },
                                                    );
                                                }
                                            }
                                            Ok(Err(err)) => {
                                                worker_state.set_status("Error", None, &err);
                                                send_json(
                                                    &worker_tx,
                                                    &ServerMessage::Error { error: err },
                                                );
                                            }
                                            Err(err) => {
                                                let msg = format!("Inference task failed: {err}");
                                                worker_state.set_status("Error", None, &msg);
                                                send_json(
                                                    &worker_tx,
                                                    &ServerMessage::Error { error: msg },
                                                );
                                            }
                                        }
                                    }
                                }));

                                send_json(
                                    &out_tx,
                                    &ServerMessage::Ready {
                                        device: GPU_BACKEND,
                                        filename: loaded_filename,
                                    },
                                );
                            }
                            Err(error) => {
                                ui_state.set_status("Error", None, &error);
                                send_json(&out_tx, &ServerMessage::Error { error });
                            }
                        }
                    }
                    Ok(ClientMessage::SetDiarization { enabled, model }) => {
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        pipeline_cfg.diarization_model =
                            DiarizationModel::parse(model.as_deref(), enabled);
                        let current_pos = if matches!(
                            active_session,
                            Some(ActiveSession::GoogleLive { .. } | ActiveSession::CloudSpeech { .. }),
                        ) {
                            stream_samples.load(Ordering::Relaxed)
                        } else {
                            segmenter.stream_pos() + vad_stage.buffered_samples() as u64
                        };
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(current_pos);
                        }
                        if pipeline_cfg.diarization_model != DiarizationModel::Disabled {
                            maybe_start_diarizer(
                                &engine,
                                pipeline_cfg.diarization_model,
                                &speaker_timeline,
                                current_pos,
                                &mut diar_tx,
                                &mut diar_task,
                                &ui_state,
                                &out_tx,
                            )
                            .await;
                        }
                    }
                    Ok(ClientMessage::Stop) => {
                        if let Some(handle) = infer_handle.take() {
                            handle.abort();
                        }
                        stop_active_session(&mut active_session);
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        if let Ok(mut guard) = infer_slot.lock() {
                            guard.running = false;
                            guard.pending = None;
                        }
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(0);
                        }
                        segmenter.reset_stream();
                        vad_stage.reset();
                        live_gate.reset();
                        speech_active = false;
                        stream_samples.store(0, Ordering::Relaxed);
                        ui_state.set_status("Idle", None, "Waiting for browser extension...");
                    }
                    Err(err) => {
                        send_json(
                            &out_tx,
                            &ServerMessage::Error {
                                error: format!("Invalid message: {err}"),
                            },
                        );
                    }
                }
            }
            Message::Binary(bytes) => {
                if active_session.is_none() {
                    continue;
                }

                if let Some(ActiveSession::CloudSpeech { audio_tx, .. }) = active_session.as_ref() {
                    if bytes.is_empty() || bytes.len() % 2 != 0 {
                        continue;
                    }
                    stream_samples.fetch_add((bytes.len() / 2) as u64, Ordering::Relaxed);
                    if let Some(tx) = diar_tx.as_ref() {
                        let _ = tx.send(pcm_i16_le_to_f32(&bytes));
                    }
                    if let Err(mpsc::error::TrySendError::Full(_)) =
                        audio_tx.try_send(bytes.to_vec())
                    {
                        stop_active_session(&mut active_session);
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        report_session_error(
                            &ui_state,
                            &out_tx,
                            "Cloud transcription could not keep up with the audio stream.".into(),
                        );
                    }
                    continue;
                }

                if let Some(ActiveSession::GoogleLive { audio_tx, .. }) = active_session.as_ref() {
                    if !pipeline_cfg.enable_vad_endpointing {
                        let samples = pcm_i16_le_to_f32(&bytes);
                        stream_samples.fetch_add(samples.len() as u64, Ordering::Relaxed);
                        if let Some(tx) = diar_tx.as_ref() {
                            let _ = tx.send(samples);
                        }
                        let _ = audio_tx.send(bytes.to_vec());
                        continue;
                    }

                    let hops = vad_stage.push_pcm16(&bytes).await;
                    for hop in hops {
                        stream_samples.fetch_add(hop.f32_samples.len() as u64, Ordering::Relaxed);
                        let gate_out = live_gate.push_hop(&hop);
                        if let Some(tx) = diar_tx.as_ref() {
                            let _ = tx.send(gate_out.diar_f32);
                        }
                        update_speech_indicator(
                            gate_out.is_speaking,
                            &mut speech_active,
                            &infer_slot,
                            &ui_state,
                            &out_tx,
                        );
                        if !gate_out.pcm16.is_empty() {
                            let _ = audio_tx.send(gate_out.pcm16);
                        }
                    }
                    continue;
                }

                let hops = vad_stage.push_pcm16(&bytes).await;
                let vad_enabled = vad_stage.is_enabled();
                for hop in hops {
                    stream_samples.fetch_add(hop.f32_samples.len() as u64, Ordering::Relaxed);
                    let out = segmenter.push(&hop.f32_samples, hop.speech_prob);
                    let speaking = segmenter.is_speaking();
                    feed_diarizer_hop(
                        diar_tx.as_ref(),
                        &hop.f32_samples,
                        hop.is_voiced || speaking,
                        vad_enabled,
                    );
                    update_speech_indicator(
                        speaking,
                        &mut speech_active,
                        &infer_slot,
                        &ui_state,
                        &out_tx,
                    );

                    if let Some(final_chunk) = out.final_chunk {
                        enqueue_chunk(&infer_slot, &infer_notify, final_chunk, true);
                    } else if matches!(active_session, Some(ActiveSession::LocalWhisper { .. }))
                        && let Some(interim_chunk) = out.interim_chunk
                    {
                        enqueue_chunk(&infer_slot, &infer_notify, interim_chunk, false);
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    if let Some(handle) = infer_handle {
        handle.abort();
    }
    let was_active = active_session.is_some();
    stop_active_session(&mut active_session);
    stop_diarizer_worker(&mut diar_tx, &mut diar_task);
    if was_active {
        ui_state.set_status("Idle", None, "Waiting for browser extension...");
    }
    writer.abort();
}

async fn accept_or_pending(
    listener: Option<&TcpListener>,
) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
    match listener {
        Some(l) => l.accept().await,
        None => std::future::pending().await,
    }
}

async fn run_server(initial_addr: String, ui_state: SharedState) {
    let initial_listener = TcpListener::bind(&initial_addr)
        .await
        .unwrap_or_else(|e| panic!("Failed to bind {initial_addr}: {e}"));
    println!(
        "Live Subtitles ({GPU_BACKEND}) server listening on ws://{initial_addr}"
    );

    #[cfg(target_os = "linux")]
    {
        tokio::spawn(tray::spawn_tray(ui_state.clone()));
        tray::notify_started(&initial_addr);
    }

    let engine = Arc::new(AsrEngine::new());
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(0_u64);
    let mut generation: u64 = 0;
    let mut current_addr = initial_addr;
    let mut listener: Option<TcpListener> = Some(initial_listener);
    let restart_notify = ui_state.restart_notify();

    loop {
        tokio::select! {
            accept_res = accept_or_pending(listener.as_ref()) => {
                match accept_res {
                    Ok((stream, _)) => {
                        let engine_clone = Arc::clone(&engine);
                        let state_clone = ui_state.clone();
                        let client_shutdown = shutdown_rx.clone();
                        tokio::spawn(async move {
                            handle_client(stream, engine_clone, state_clone, client_shutdown).await;
                        });
                    }
                    Err(err) => {
                        eprintln!("[server] Accept error: {err}");
                    }
                }
            }
            _ = restart_notify.notified() => {
                while let Some(new_addr) = ui_state.take_restart_request() {
                    if new_addr == current_addr {
                        drop(listener.take());
                        generation = generation.wrapping_add(1);
                        let _ = shutdown_tx.send(generation);

                        match TcpListener::bind(&new_addr).await {
                            Ok(new_l) => {
                                listener = Some(new_l);
                                ui_state.set_status("Idle", None, "Waiting for browser extension...");
                                ui_state.set_server_notice(Some((
                                    false,
                                    format!("Restarted on ws://{new_addr}"),
                                )));
                                println!("Live Subtitles ({GPU_BACKEND}) server restarted on ws://{new_addr}");
                            }
                            Err(err) => {
                                let msg = format!("Failed to bind {new_addr}: {err}");
                                ui_state.set_status("Error", None, &msg);
                                ui_state.set_server_notice(Some((true, msg)));
                            }
                        }
                    } else {
                        match TcpListener::bind(&new_addr).await {
                            Ok(new_l) => {
                                drop(listener.take());
                                generation = generation.wrapping_add(1);
                                let _ = shutdown_tx.send(generation);
                                listener = Some(new_l);
                                current_addr = new_addr.clone();
                                ui_state.set_server_addr(new_addr.clone());
                                ui_state.set_status("Idle", None, "Waiting for browser extension...");
                                ui_state.set_server_notice(Some((
                                    false,
                                    format!("Listening on ws://{new_addr}"),
                                )));
                                println!("Live Subtitles ({GPU_BACKEND}) server restarted on ws://{new_addr}");
                            }
                            Err(err) => {
                                ui_state.set_server_notice(Some((
                                    true,
                                    format!("Failed to bind {new_addr}: {err}"),
                                )));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn run_desktop_loop<R, F>(
    ui_state: &SharedState,
    runtime: &mut Option<R>,
    is_linux: bool,
    is_macos: bool,
    mut open_window: F,
) where
    F: FnMut(SharedState, &mut dyn FnMut()) -> Result<(), String>,
{
    loop {
        if ui_state.should_show_window() {
            let mut on_exit = || {
                // AppKit can terminate the process without returning from run_window.
                if is_macos {
                    drop(runtime.take());
                }
            };
            if let Err(err) = open_window(ui_state.clone(), &mut on_exit) {
                eprintln!("[ui] Failed to open desktop window: {err}");
                break;
            }
            ui_state.mark_window_closed();
            if !is_linux {
                break;
            }
        }
        if !ui_state.wait_until_show_or_quit() {
            break;
        }
    }
}

fn main() {
    let addr = std::env::var("LIVE_SUBTITLES_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let headless = std::env::args().any(|a| a == "--headless");
    let ui_state = SharedState::new(addr.clone());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to start Tokio runtime");

    if headless {
        rt.block_on(run_server(addr, ui_state));
        return;
    }

    rt.spawn(run_server(addr, ui_state.clone()));
    let mut runtime = Some(rt);

    run_desktop_loop(
        &ui_state,
        &mut runtime,
        cfg!(target_os = "linux"),
        cfg!(target_os = "macos"),
        |state, on_exit| ui::run_window(state, on_exit).map_err(|e| e.to_string()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn open_state_for_test() -> SharedState {
        let state = SharedState::new("127.0.0.1:8765".to_string());
        if !state.should_show_window() {
            state.toggle_window();
        }
        assert!(state.should_show_window());
        state
    }

    #[test]
    fn serializes_progress_speed_and_deserializes_cancel_download() {
        let json = serde_json::to_string(&ServerMessage::Progress {
            progress: Some(42),
            label: "Downloading ggml-large-v3.bin".to_string(),
            bytes_per_sec: Some(12_582_912),
        })
        .unwrap();
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(val["type"], "progress");
        assert_eq!(val["progress"], 42);
        assert_eq!(val["bytesPerSec"], 12_582_912);

        let parsed: ClientMessage = serde_json::from_str(r#"{"type":"cancelDownload"}"#).unwrap();
        assert!(matches!(parsed, ClientMessage::CancelDownload));

        let list_google: ClientMessage =
            serde_json::from_str(r#"{"type":"listGoogleModels","apiKey":"AIza123"}"#).unwrap();
        assert!(matches!(list_google, ClientMessage::ListGoogleModels { .. }));

        let start_google: ClientMessage = serde_json::from_str(
            r#"{
                "type": "start",
                "provider": "google",
                "apiKey": "AIza123",
                "language": "ja",
                "googleTargetLanguage": "en",
                "googleTranslatorModelId": "gemini-2.5-flash-lite",
                "diarization": true,
                "googleModel": {
                    "id": "gemini-3.5-transcribe-live",
                    "name": "models/gemini-3.5-transcribe-live",
                    "displayName": "Gemini 3.5 Transcribe Live",
                    "description": "",
                    "supportedGenerationMethods": ["bidiGenerateContent"],
                    "transport": "live",
                    "kind": "transcribe-live"
                }
            }"#,
        )
        .unwrap();
        assert!(matches!(start_google, ClientMessage::Start { .. }));

        let google_resp = serde_json::to_value(&ServerMessage::GoogleModels { models: Vec::new() }).unwrap();
        assert_eq!(google_resp["type"], "googleModels");
    }

    #[test]
    fn reads_elevenlabs_model_requests_and_serializes_catalog_responses() {
        let list: ClientMessage =
            serde_json::from_str(r#"{"type":"listElevenLabsModels"}"#).unwrap();
        assert!(matches!(list, ClientMessage::ListElevenLabsModels));

        let start: ClientMessage = serde_json::from_str(
            r#"{"type":"start","provider":"elevenlabs","elevenLabsModelId":"scribe_v3_live"}"#,
        )
        .unwrap();
        assert!(matches!(
            start,
            ClientMessage::Start { eleven_labs_model_id: Some(id), .. } if id == "scribe_v3_live"
        ));

        let response =
            serde_json::to_value(ServerMessage::ElevenLabsModels { models: Vec::new() }).unwrap();
        assert_eq!(response["type"], "elevenLabsModels");
        assert_eq!(response["models"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn cloud_capture_requires_a_key_for_the_selected_provider() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(0);
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            handle_client(
                stream,
                Arc::new(AsrEngine::new()),
                SharedState::new(addr.to_string()),
                shutdown_rx,
            )
            .await;
        });

        let (mut client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        for provider in [CloudSpeechProvider::Soniox, CloudSpeechProvider::ElevenLabs] {
            client
                .send(Message::Text(
                    serde_json::json!({
                        "type": "start",
                        "provider": provider.id(),
                        "apiKey": " ",
                        "language": "ja",
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(3), client.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let payload: serde_json::Value =
                serde_json::from_str(response.to_text().unwrap()).unwrap();
            assert_eq!(payload["type"], "error");
            assert_eq!(
                payload["error"],
                format!(
                    "Set your {} API key in Settings before starting.",
                    provider.label()
                )
            );
        }
        client.close(None).await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cloud_start_cancels_when_client_stops_or_disconnects() {
        let (_shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(0);
        let stop = Message::Text(r#"{"type":"stop"}"#.into());
        for (message, stopped) in [(stop, true), (Message::Close(None), false)] {
            let mut source = futures_util::stream::iter([Ok(message)]);
            let outcome = run_cancellable(
                std::future::pending::<Result<(), String>>(),
                &mut shutdown_rx,
                &mut source,
                |msg| matches!(msg, ClientMessage::Stop),
            )
            .await;
            if stopped {
                assert!(matches!(outcome, CancellableOutcome::Cancelled));
            } else {
                assert!(matches!(outcome, CancellableOutcome::Disconnected));
            }
        }
    }

    #[tokio::test]
    async fn cloud_stream_batches_audio_and_relays_interim_and_final_subtitles() {
        use tokio_tungstenite::tungstenite::protocol::Role;

        tokio::time::timeout(Duration::from_secs(3), async {
            let provider = CloudSpeechProvider::Soniox;
            let (client, server) = tokio::io::duplex(8192);
            let ws = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
            let mut upstream = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
            let (audio_tx, audio_rx) = mpsc::channel(4);
            let (out_tx, mut out_rx) = mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                stream_cloud_speech(
                    provider,
                    ws,
                    audio_rx,
                    &Mutex::new(SpeakerTimeline::default()),
                    &AtomicU64::new(2048),
                    &SharedState::new(String::new()),
                    &out_tx,
                    false,
                    None,
                ).await
            });
            for _ in 0..4 {
                audio_tx.send(vec![0; 1024]).await.unwrap();
            }
            assert_eq!(upstream.next().await.unwrap().unwrap(), Message::Binary(vec![0; 4096].into()));
            for (frame, expected, is_final) in [
                (r#"{"tokens":[{"text":"hello", "is_final":false}]}"#, "hello", false),
                (r#"{"tokens":[{"text":"Hello.", "is_final":true},{"text":"<end>", "is_final":true}]}"#, "Hello.", true),
            ] {
                upstream.send(Message::Text(frame.into())).await.unwrap();
                loop {
                    let message = out_rx.recv().await.unwrap();
                    let payload: serde_json::Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
                    if payload["type"] == "transcript" {
                        assert_eq!(payload["text"], expected);
                        assert_eq!(payload["rawText"], expected);
                        assert_eq!(payload["isFinal"], is_final);
                        break;
                    }
                }
            }
            drop(audio_tx);
            assert!(task.await.unwrap().is_ok());
        }).await.unwrap();
    }

    #[test]
    fn macos_drops_runtime_inside_window_on_exit_and_stops_loop() {
        let state = open_state_for_test();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut runtime = Some(DropFlag(Arc::clone(&dropped)));
        let mut opens = 0;

        run_desktop_loop(&state, &mut runtime, false, true, |_s, on_exit| {
            opens += 1;
            assert!(!dropped.load(Ordering::SeqCst));
            on_exit();
            assert!(dropped.load(Ordering::SeqCst));
            Ok(())
        });

        assert_eq!(opens, 1);
        assert!(runtime.is_none());
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn linux_keeps_runtime_across_tray_hide_and_reopen_until_quit() {
        let state = open_state_for_test();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut runtime = Some(DropFlag(Arc::clone(&dropped)));
        let mut opens = 0;
        let mut reopener = None;

        run_desktop_loop(&state, &mut runtime, true, false, |s, on_exit| {
            opens += 1;
            on_exit();
            assert!(!dropped.load(Ordering::SeqCst));

            if opens == 1 {
                let s_clone = s.clone();
                reopener = Some(std::thread::spawn(move || {
                    while s_clone.snapshot().window_open {
                        std::thread::yield_now();
                    }
                    s_clone.toggle_window();
                }));
            } else {
                s.request_quit();
            }
            Ok(())
        });

        if let Some(handle) = reopener {
            handle.join().unwrap();
        }

        assert_eq!(opens, 2);
        assert!(runtime.is_some());
        assert!(!dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn windows_exits_loop_on_window_close_without_early_on_exit_drop() {
        let state = open_state_for_test();
        let dropped = Arc::new(AtomicBool::new(false));
        let mut runtime = Some(DropFlag(Arc::clone(&dropped)));
        let mut opens = 0;

        run_desktop_loop(&state, &mut runtime, false, false, |_s, on_exit| {
            opens += 1;
            on_exit();
            assert!(!dropped.load(Ordering::SeqCst));
            Ok(())
        });

        assert_eq!(opens, 1);
        assert!(runtime.is_some());
        assert!(!dropped.load(Ordering::SeqCst));
    }
}
