mod audio;
mod diarizer;
mod engine;
mod furigana;
mod models;
mod state;
#[cfg(target_os = "linux")]
mod tray;
mod ui;

use audio::{SegmenterConfig, SpeechChunk, SpeechSegmenter, pcm_i16_le_to_f32};
use diarizer::{NemotronDiarizer, SpeakerTimeline};
use engine::{AsrEngine, LoadedWhisper, detect_speech_prob, transcribe};
use furigana::{FuriganaToken, annotate_furigana};
use futures_util::{SinkExt, StreamExt};
use models::{
    LocalModelFile, delete_local_model, detect_local_model_direct_s2tt, ensure_file_by_source,
    list_local_models,
};
use serde::{Deserialize, Serialize};
use state::SharedState;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_ADDR: &str = "127.0.0.1:8765";
const VAD_WINDOW_SAMPLES: usize = 8192;
const VAD_HOP_SAMPLES: usize = 2048;
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
    DownloadModel {
        filename: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        url: Option<String>,
    },
    CancelDownload,
    DeleteModel {
        filename: String,
    },
    Start {
        #[serde(default)]
        filename: String,
        #[serde(default)]
        repo: Option<String>,
        #[serde(default)]
        url: Option<String>,
        language: String,
        #[serde(rename = "japaneseOnly", default)]
        japanese_only: Option<bool>,
        #[serde(default)]
        translate: bool,
        #[serde(default)]
        diarization: bool,
    },
    SetDiarization {
        enabled: bool,
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

fn send_json(tx: &mpsc::UnboundedSender<Message>, msg: &ServerMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = tx.send(Message::Text(json.into()));
    }
}

async fn send_models_list(tx: &mpsc::UnboundedSender<Message>, engine: &AsrEngine) {
    match list_local_models().await {
        Ok((dir, files)) => {
            let loaded = engine.loaded_filenames().await;
            send_json(
                tx,
                &ServerMessage::Models {
                    cache_dir: dir.to_string_lossy().into_owned(),
                    files,
                    loaded,
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

    let mut active_ctx: Option<Arc<LoadedWhisper>> = None;
    let mut vad_ctx = None;
    let speaker_timeline = Arc::new(Mutex::new(SpeakerTimeline::default()));
    let mut diar_tx: Option<mpsc::UnboundedSender<Vec<f32>>> = None;
    let mut diar_task: Option<tokio::task::JoinHandle<()>> = None;
    let mut segmenter = SpeechSegmenter::new(SegmenterConfig::default());
    let mut vad_ring: VecDeque<f32> = VecDeque::with_capacity(VAD_WINDOW_SAMPLES + VAD_HOP_SAMPLES);
    let mut pending_vad_samples: Vec<f32> = Vec::with_capacity(VAD_HOP_SAMPLES * 2);
    let mut speech_active = false;

    let infer_slot = Arc::new(Mutex::new(InferSlot::default()));
    let infer_notify = Arc::new(Notify::new());
    let mut infer_handle: Option<tokio::task::JoinHandle<()>> = None;

    'client: loop {
        let incoming = tokio::select! {
            _ = shutdown_rx.changed() => break,
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
                    Ok(ClientMessage::DownloadModel { filename, repo, url }) => {
                        let tx_prog = out_tx.clone();
                        let mut cancelled = false;
                        let mut disconnected = false;
                        let download_res = {
                            let mut download_fut = std::pin::pin!(ensure_file_by_source(
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
                            ));
                            loop {
                                tokio::select! {
                                    res = &mut download_fut => break Some(res),
                                    _ = shutdown_rx.changed() => {
                                        disconnected = true;
                                        break None;
                                    }
                                    next = ws_source.next() => {
                                        match next {
                                            Some(Ok(Message::Text(ctrl))) => {
                                                if matches!(serde_json::from_str::<ClientMessage>(&ctrl), Ok(ClientMessage::CancelDownload)) {
                                                    cancelled = true;
                                                    break None;
                                                }
                                            }
                                            Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                                                disconnected = true;
                                                break None;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        };
                        if disconnected {
                            break 'client;
                        }
                        if cancelled {
                            continue;
                        }
                        match download_res.expect("download result missing") {
                            Ok(_) => {
                                send_json(&out_tx, &ServerMessage::Downloaded { filename });
                                send_models_list(&out_tx, &engine).await;
                            }
                            Err(error) => {
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
                    Ok(ClientMessage::Start {
                        filename,
                        repo,
                        url,
                        language,
                        japanese_only,
                        translate,
                        diarization,
                    }) => {
                        if let Some(handle) = infer_handle.take() {
                            handle.abort();
                        }
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        if let Ok(mut guard) = infer_slot.lock() {
                            guard.running = false;
                            guard.pending = None;
                        }
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(0);
                        }
                        segmenter.reset_stream();
                        vad_ring.clear();
                        pending_vad_samples.clear();
                        speech_active = false;

                        let force_ja = japanese_only
                            .unwrap_or_else(|| filename.to_ascii_lowercase().contains("kotoba"));
                        let active_language = if force_ja {
                            Some("ja".to_string())
                        } else if language == "auto" {
                            None
                        } else {
                            Some(language)
                        };

                        let tx_vad = out_tx.clone();
                        vad_ctx = engine
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
                            .await;

                        if diarization {
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
                                    let (tx, task) = spawn_diarizer_worker(
                                        diarizer,
                                        Arc::clone(&speaker_timeline),
                                        0,
                                    );
                                    diar_tx = Some(tx);
                                    diar_task = Some(task);
                                }
                                Err(err) => {
                                    eprintln!("[server] Speaker diarizer unavailable: {err}");
                                }
                            }
                        }

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
                                active_ctx = Some(Arc::clone(&model));
                                ui_state.set_status(
                                    "Listening",
                                    Some(loaded_filename.clone()),
                                    "Waiting for speech...",
                                );
                                let model_supports_s2tt = detect_local_model_direct_s2tt(
                                    &models::cache_dir().join(&loaded_filename),
                                    &loaded_filename,
                                )
                                .await
                                .unwrap_or(true);
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
                                        let asr_res = tokio::task::spawn_blocking(move || {
                                            transcribe(
                                                &model_clone,
                                                &samples,
                                                lang_clone.as_deref(),
                                                worker_translate,
                                            )
                                            .map(|(text, raw_text, ms)| {
                                                let tokens = if text.is_empty() || worker_translate {
                                                    Vec::new()
                                                } else {
                                                    annotate_furigana(&text)
                                                };
                                                (text, tokens, raw_text, ms)
                                            })
                                        })
                                        .await;

                                        if let Ok(mut guard) = worker_slot.lock() {
                                            guard.running = false;
                                        }

                                        match asr_res {
                                            Ok(Ok((text, tokens, raw_text, ms))) => {
                                                if !text.is_empty() {
                                                    let speakers = worker_timeline
                                                        .lock()
                                                        .ok()
                                                        .map(|tl| {
                                                            tl.active_speakers(
                                                                pending.chunk.start_sample,
                                                                pending.chunk.end_sample,
                                                            )
                                                        })
                                                        .unwrap_or_default();

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
                    Ok(ClientMessage::SetDiarization { enabled }) => {
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        let base_offset =
                            segmenter.stream_pos() + pending_vad_samples.len() as u64;
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(base_offset);
                        }
                        if enabled {
                            let state_diar = ui_state.clone();
                            let tx_act = out_tx.clone();
                            match engine
                                .get_or_load_diarizer(move |_progress, label, _bytes_per_sec| {
                                    state_diar.set_activity(label.clone());
                                    send_json(&tx_act, &ServerMessage::Activity { activity: label });
                                })
                                .await
                            {
                                Ok(diarizer) => {
                                    let refreshed_offset =
                                        segmenter.stream_pos() + pending_vad_samples.len() as u64;
                                    let (tx, task) = spawn_diarizer_worker(
                                        diarizer,
                                        Arc::clone(&speaker_timeline),
                                        refreshed_offset,
                                    );
                                    diar_tx = Some(tx);
                                    diar_task = Some(task);
                                }
                                Err(err) => {
                                    eprintln!("[server] Speaker diarizer unavailable: {err}");
                                }
                            }
                        }
                    }
                    Ok(ClientMessage::Stop) => {
                        if let Some(handle) = infer_handle.take() {
                            handle.abort();
                        }
                        stop_diarizer_worker(&mut diar_tx, &mut diar_task);
                        if let Ok(mut guard) = infer_slot.lock() {
                            guard.running = false;
                            guard.pending = None;
                        }
                        if let Ok(mut guard) = speaker_timeline.lock() {
                            guard.reset(0);
                        }
                        segmenter.reset_stream();
                        vad_ring.clear();
                        pending_vad_samples.clear();
                        speech_active = false;
                        active_ctx = None;
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
                if active_ctx.is_none() {
                    continue;
                }
                let samples = pcm_i16_le_to_f32(&bytes);
                if let Some(tx) = diar_tx.as_ref() {
                    let _ = tx.send(samples.clone());
                }
                pending_vad_samples.extend_from_slice(&samples);

                while pending_vad_samples.len() >= VAD_HOP_SAMPLES {
                    let hop: Vec<f32> = pending_vad_samples.drain(..VAD_HOP_SAMPLES).collect();
                    vad_ring.extend(hop.iter().copied());
                    while vad_ring.len() > VAD_WINDOW_SAMPLES {
                        vad_ring.pop_front();
                    }

                    let prob = if let Some(vad) = vad_ctx.as_ref() {
                        let window: Vec<f32> = vad_ring.iter().copied().collect();
                        let vad_arc = Arc::clone(vad);
                        tokio::task::spawn_blocking(move || detect_speech_prob(&vad_arc, &window))
                            .await
                            .ok()
                            .flatten()
                    } else {
                        None
                    };

                    let out = segmenter.push(&hop, prob);
                    let speaking = segmenter.is_speaking();
                    if speaking != speech_active {
                        speech_active = speaking;
                        let busy = infer_slot.lock().map(|g| g.running).unwrap_or(false);
                        if !busy {
                            let label = if speaking {
                                "Hearing speech..."
                            } else {
                                "Waiting for speech..."
                            };
                            ui_state.set_activity(label);
                            send_json(
                                &out_tx,
                                &ServerMessage::Activity {
                                    activity: label.to_string(),
                                },
                            );
                        }
                    }

                    if let Some(final_chunk) = out.final_chunk {
                        enqueue_chunk(&infer_slot, &infer_notify, final_chunk, true);
                    } else if let Some(interim_chunk) = out.interim_chunk {
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
    stop_diarizer_worker(&mut diar_tx, &mut diar_task);
    if active_ctx.is_some() {
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

fn main() {
    let addr = std::env::var("LIVE_SUBTITLES_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());
    let headless = std::env::args().any(|a| a == "--headless");
    let ui_state = SharedState::new(addr.clone());

    if headless {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to start Tokio runtime");
        rt.block_on(run_server(addr, ui_state));
        return;
    }

    let bg_state = ui_state.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("Failed to start Tokio runtime");
        rt.block_on(run_server(addr, bg_state));
    });

    loop {
        if ui_state.should_show_window() {
            if let Err(err) = ui::run_window(ui_state.clone()) {
                eprintln!("[ui] Failed to open desktop window: {err}");
                break;
            }
            ui_state.mark_window_closed();
            #[cfg(not(target_os = "linux"))]
            break;
        }
        if !ui_state.wait_until_show_or_quit() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
