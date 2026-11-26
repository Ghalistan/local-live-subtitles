use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

pub const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
pub const GEMINI_LIVE_HOST: &str = "generativelanguage.googleapis.com";
pub const GEMINI_LIVE_PATH: &str =
    "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";

pub type GoogleLiveWs = WebSocketStream<TlsStream<TcpStream>>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GoogleModelInfo {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub supported_generation_methods: Vec<String>,
    pub transport: String,
    pub kind: String,
}

pub fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push_str(&format!("{byte:02X}"));
        }
    }
    out
}

pub fn to_bcp47(code: &str) -> Option<&'static str> {
    match code {
        "ja" => Some("ja-JP"),
        "en" => Some("en-US"),
        "id" => Some("id-ID"),
        "zh" => Some("zh-CN"),
        "ko" => Some("ko-KR"),
        "es" => Some("es-ES"),
        "fr" => Some("fr-FR"),
        "de" => Some("de-DE"),
        _ => None,
    }
}

pub fn language_name(code: &str) -> &str {
    match code {
        "ja" => "Japanese",
        "en" => "English",
        "id" => "Indonesian",
        "zh" => "Chinese",
        "ko" => "Korean",
        "es" => "Spanish",
        "fr" => "French",
        "de" => "German",
        _ => code,
    }
}

fn is_excluded_model_id(lower: &str) -> bool {
    if lower.is_empty() || lower.starts_with("gemma-") {
        return true;
    }
    const EXCLUDED: &[&str] = &[
        "embedding",
        "imagen",
        "veo",
        "lyria",
        "tts",
        "aqa",
        "robotics",
        "computer-use",
        "deep-research",
        "nano-banana",
        "bisociate",
    ];
    EXCLUDED.iter().any(|kw| lower.contains(kw))
}

pub fn classify_google_model(id: &str, methods: &[String]) -> Option<(&'static str, &'static str)> {
    let lower = id.trim().to_ascii_lowercase();
    if is_excluded_model_id(&lower) {
        return None;
    }

    let supports_bidi = methods.iter().any(|m| m == "bidiGenerateContent");
    let supports_generate = methods.iter().any(|m| m == "generateContent");
    if !supports_bidi && !supports_generate {
        return None;
    }

    if lower.contains("transcribe") && (supports_bidi || lower.contains("live")) {
        return Some(("live", "transcribe-live"));
    }
    if lower.contains("translate") && (supports_bidi || lower.contains("live")) {
        return Some(("live", "translate-live"));
    }
    if supports_bidi {
        return Some(("live", "live"));
    }
    if lower.contains("transcribe") {
        return Some(("generate", "transcribe"));
    }
    Some(("generate", "multimodal"))
}

fn kind_priority(kind: &str) -> u8 {
    match kind {
        "transcribe-live" => 0,
        "translate-live" => 1,
        "live" => 2,
        "transcribe" => 3,
        _ => 4,
    }
}

fn compare_natural_desc(a: &str, b: &str) -> Ordering {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let mut i = 0;
    let mut j = 0;

    while i < ab.len() && j < bb.len() {
        if ab[i].is_ascii_digit() && bb[j].is_ascii_digit() {
            let mut ai = i;
            while ai < ab.len() && ab[ai].is_ascii_digit() {
                ai += 1;
            }
            let mut bj = j;
            while bj < bb.len() && bb[bj].is_ascii_digit() {
                bj += 1;
            }
            let an = std::str::from_utf8(&ab[i..ai])
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            let bn = std::str::from_utf8(&bb[j..bj])
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            match bn.cmp(&an) {
                Ordering::Equal => {
                    i = ai;
                    j = bj;
                }
                non_eq => return non_eq,
            }
        } else {
            let ca = ab[i].to_ascii_lowercase();
            let cb = bb[j].to_ascii_lowercase();
            match cb.cmp(&ca) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                non_eq => return non_eq,
            }
        }
    }
    bb.len().cmp(&ab.len())
}

pub fn parse_google_models_response(raw_models: &[Value]) -> Vec<GoogleModelInfo> {
    let mut models = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for entry in raw_models {
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let raw_name = obj
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let id = raw_name.strip_prefix("models/").unwrap_or(raw_name);
        if id.is_empty() || !seen_ids.insert(id.to_string()) {
            continue;
        }

        let methods: Vec<String> = obj
            .get("supportedGenerationMethods")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();

        let Some((transport, kind)) = classify_google_model(id, &methods) else {
            continue;
        };

        let display_name = obj
            .get("displayName")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(id)
            .to_string();
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("")
            .to_string();

        models.push(GoogleModelInfo {
            id: id.to_string(),
            name: format!("models/{id}"),
            display_name,
            description,
            supported_generation_methods: methods,
            transport: transport.to_string(),
            kind: kind.to_string(),
        });
    }

    models.sort_by(|a, b| {
        kind_priority(&a.kind)
            .cmp(&kind_priority(&b.kind))
            .then_with(|| compare_natural_desc(&a.id, &b.id))
    });
    models
}

fn extract_api_error_message(status: reqwest::StatusCode, body_text: &str) -> String {
    if let Ok(val) = serde_json::from_str::<Value>(body_text)
        && let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return msg.to_string();
    }
    format!(
        "Google API request failed ({} {}).",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Error")
    )
}

pub async fn fetch_google_models(api_key: &str) -> Result<Vec<GoogleModelInfo>, String> {
    let trimmed_key = api_key.trim();
    if trimmed_key.is_empty() {
        return Err("Enter a Google Gemini API key first.".to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| format!("Failed to initialize HTTP client: {e}"))?;

    let mut all_raw_models: Vec<Value> = Vec::new();
    let mut page_token = String::new();

    for _ in 0..5 {
        let url = if page_token.is_empty() {
            format!("{GEMINI_API_BASE}/models?pageSize=200")
        } else {
            format!(
                "{GEMINI_API_BASE}/models?pageSize=200&pageToken={}",
                percent_encode(&page_token)
            )
        };

        let response = client
            .get(&url)
            .header("x-goog-api-key", trimmed_key)
            .send()
            .await
            .map_err(|e| format!("Failed to reach Google Gemini API: {e}"))?;

        let status = response.status();
        let body_text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read Google API response: {e}"))?;

        if !status.is_success() {
            return Err(extract_api_error_message(status, &body_text));
        }

        let payload: Value = serde_json::from_str(&body_text)
            .map_err(|e| format!("Invalid Google models response JSON: {e}"))?;
        if let Some(arr) = payload.get("models").and_then(Value::as_array) {
            all_raw_models.extend(arr.iter().cloned());
        }
        match payload
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(token) => page_token = token.to_string(),
            None => break,
        }
    }

    Ok(parse_google_models_response(&all_raw_models))
}

pub fn build_live_setup_message(
    model: &GoogleModelInfo,
    language: &str,
    target_language: &str,
) -> Value {
    let model_name = if model.name.starts_with("models/") {
        model.name.clone()
    } else {
        format!("models/{}", model.id)
    };

    if model.kind == "transcribe-live" {
        let input_audio_transcription = match to_bcp47(language) {
            Some(bcp47) => json!({ "languageCodes": [bcp47] }),
            None => json!({}),
        };
        return json!({
            "setup": {
                "model": model_name,
                "generationConfig": {
                    "responseModalities": ["TEXT"]
                },
                "inputAudioTranscription": input_audio_transcription
            }
        });
    }

    if model.kind == "translate-live" {
        let raw_target = if target_language == "same" {
            "en"
        } else {
            target_language
        };
        let target_code = to_bcp47(raw_target).unwrap_or(raw_target);
        let target_name = language_name(raw_target);
        return json!({
            "setup": {
                "model": model_name,
                "systemInstruction": {
                    "parts": [{
                        "text": format!(
                            "Translate all spoken audio accurately and naturally into {target_name}."
                        )
                    }]
                },
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "translationConfig": {
                        "targetLanguageCode": target_code,
                        "echoTargetLanguage": false
                    }
                },
                "outputAudioTranscription": {}
            }
        });
    }

    if target_language != "same" {
        let target_name = language_name(target_language);
        json!({
            "setup": {
                "model": model_name,
                "generationConfig": {
                    "responseModalities": ["AUDIO"]
                },
                "inputAudioTranscription": {},
                "outputAudioTranscription": {},
                "systemInstruction": {
                    "parts": [{
                        "text": format!(
                            "You are a live subtitle translator. Translate every spoken utterance concisely into {target_name}. Output only the translation with no commentary."
                        )
                    }]
                }
            }
        })
    } else {
        json!({
            "setup": {
                "model": model_name,
                "generationConfig": {
                    "responseModalities": ["AUDIO"]
                },
                "inputAudioTranscription": {},
                "systemInstruction": {
                    "parts": [{
                        "text": "Listen silently and do not reply unless spoken to."
                    }]
                }
            }
        })
    }
}

fn merge_transcript_text(current: &str, incoming: &str) -> String {
    let trimmed_in = incoming.trim();
    if trimmed_in.is_empty() {
        return current.to_string();
    }
    if current.is_empty() {
        return trimmed_in.to_string();
    }
    if trimmed_in.starts_with(current) {
        return trimmed_in.to_string();
    }
    if current.ends_with(trimmed_in) {
        return current.to_string();
    }
    let last_ascii_alnum = current
        .as_bytes()
        .last()
        .is_some_and(u8::is_ascii_alphanumeric);
    let first_ascii_alnum = trimmed_in
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric);
    if last_ascii_alnum && first_ascii_alnum {
        format!("{current} {trimmed_in}")
    } else {
        format!("{current}{trimmed_in}")
    }
}

fn should_flush_line(text: &str, max_line_chars: usize) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    let char_count = trimmed.chars().count();
    if char_count >= max_line_chars {
        return true;
    }
    let stripped = trimmed.trim_end_matches(['"', '\'', '”', '’', '）', ')']);
    if char_count >= 8 && stripped.ends_with(['。', '？', '！']) {
        return true;
    }
    if char_count >= 20 && stripped.ends_with(['.', '?', '!']) {
        return true;
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveOutputMode {
    Input,
    OutputOnly,
    OutputOrModelTurn,
}

pub struct GeminiLiveAccumulator {
    mode: LiveOutputMode,
    max_line_chars: usize,
    committed: String,
    interim: String,
}

impl GeminiLiveAccumulator {
    pub fn new(mode: LiveOutputMode, max_line_chars: usize) -> Self {
        Self {
            mode,
            max_line_chars,
            committed: String::new(),
            interim: String::new(),
        }
    }

    pub fn push_server_content(&mut self, server_content: &Value) -> Vec<(String, bool)> {
        let mut events = Vec::new();

        let interim_input = server_content
            .get("interimInputTranscription")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let input = server_content
            .get("inputTranscription")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let output = server_content
            .get("outputTranscription")
            .and_then(|v| v.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let model_turn_text: String = server_content
            .get("modelTurn")
            .and_then(|v| v.get("parts"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter(|p| !p.get("thought").and_then(Value::as_bool).unwrap_or(false))
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<String>()
            })
            .unwrap_or_default();

        let turn_complete = server_content
            .get("turnComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || server_content
                .get("generationComplete")
                .and_then(Value::as_bool)
                .unwrap_or(false);

        match self.mode {
            LiveOutputMode::OutputOnly | LiveOutputMode::OutputOrModelTurn => {
                let primary = if !output.is_empty() {
                    output
                } else if self.mode == LiveOutputMode::OutputOrModelTurn {
                    model_turn_text.as_str()
                } else {
                    ""
                };
                if !primary.is_empty() {
                    self.committed = merge_transcript_text(&self.committed, primary);
                    if should_flush_line(&self.committed, self.max_line_chars) {
                        let flushed = self.committed.trim().to_string();
                        self.committed.clear();
                        events.push((flushed, true));
                    } else {
                        events.push((self.committed.trim().to_string(), false));
                    }
                }
            }
            LiveOutputMode::Input => {
                let mut emitted_from_update = false;
                if !input.is_empty() {
                    self.committed = merge_transcript_text(&self.committed, input);
                    self.interim.clear();
                    if should_flush_line(&self.committed, self.max_line_chars) {
                        let flushed = self.committed.trim().to_string();
                        self.committed.clear();
                        events.push((flushed, true));
                        emitted_from_update = true;
                    }
                }
                if !interim_input.is_empty() {
                    self.interim = interim_input.trim().to_string();
                }
                if !turn_complete && (!interim_input.is_empty() || !input.is_empty()) {
                    if !self.interim.is_empty() {
                        let combined = merge_transcript_text(&self.committed, &self.interim);
                        let trimmed = combined.trim().to_string();
                        if !trimmed.is_empty() {
                            events.push((trimmed, false));
                        }
                    } else if !input.is_empty() && !emitted_from_update {
                        let trimmed = self.committed.trim().to_string();
                        if !trimmed.is_empty() {
                            events.push((trimmed, false));
                        }
                    }
                }
            }
        }

        if turn_complete {
            let final_text = match self.mode {
                LiveOutputMode::OutputOnly | LiveOutputMode::OutputOrModelTurn => {
                    self.committed.trim().to_string()
                }
                LiveOutputMode::Input => merge_transcript_text(&self.committed, &self.interim)
                    .trim()
                    .to_string(),
            };
            self.committed.clear();
            self.interim.clear();
            if !final_text.is_empty() {
                events.push((final_text, true));
            }
        }

        events
    }
}

pub fn encode_realtime_pcm_chunk(pcm_le_bytes: &[u8]) -> String {
    let encoded = STANDARD.encode(pcm_le_bytes);
    json!({
        "realtimeInput": {
            "mediaChunks": [{
                "mimeType": "audio/pcm;rate=16000",
                "data": encoded
            }]
        }
    })
    .to_string()
}

pub async fn connect_google_live(
    api_key: &str,
    model: &GoogleModelInfo,
    language: &str,
    target_language: &str,
) -> Result<GoogleLiveWs, String> {
    let trimmed_key = api_key.trim();
    if trimmed_key.is_empty() {
        return Err("Set your Gemini API key in Settings before starting.".to_string());
    }

    let tcp = TcpStream::connect((GEMINI_LIVE_HOST, 443))
        .await
        .map_err(|e| format!("Failed to connect to Google Gemini Live: {e}"))?;
    let _ = tcp.set_nodelay(true);

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|e| format!("TLS setup failed: {e}"))?
    .with_root_certificates(root_store)
    .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(tls_config));
    let server_name = rustls_pki_types::ServerName::try_from(GEMINI_LIVE_HOST)
        .map_err(|_| "Invalid Gemini Live hostname.".to_string())?;
    let tls_stream = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS handshake with Google Gemini Live failed: {e}"))?;

    let ws_url = format!(
        "wss://{GEMINI_LIVE_HOST}{GEMINI_LIVE_PATH}?key={}",
        percent_encode(trimmed_key)
    );
    let (mut ws_stream, _) = tokio_tungstenite::client_async(&ws_url, tls_stream)
        .await
        .map_err(|_| {
            "Google Gemini Live WebSocket handshake failed. Check your API key and model."
                .to_string()
        })?;

    let setup_json = build_live_setup_message(model, language, target_language).to_string();
    eprintln!("[gemini-live] setup: {setup_json}");
    ws_stream
        .send(Message::Text(setup_json.into()))
        .await
        .map_err(|e| format!("Failed to send Gemini Live setup: {e}"))?;

    let wait_setup = tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(frame) = ws_stream.next().await {
            match frame {
                Ok(Message::Text(txt)) => {
                    if let Ok(payload) = serde_json::from_str::<Value>(txt.as_str()) {
                        if let Some(err_msg) = payload
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                        {
                            return Err(err_msg.to_string());
                        }
                        if payload.get("setupComplete").is_some() {
                            return Ok(());
                        }
                    }
                }
                Ok(Message::Binary(bytes)) => {
                    if let Ok(payload) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(err_msg) = payload
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(Value::as_str)
                        {
                            return Err(err_msg.to_string());
                        }
                        if payload.get("setupComplete").is_some() {
                            return Ok(());
                        }
                    }
                }
                Ok(Message::Close(frame)) => {
                    let msg = match frame {
                        Some(f) if !f.reason.is_empty() => {
                            format!("Google Gemini Live closed [{}] ({})", f.code, f.reason)
                        }
                        Some(f) => format!("Google Gemini Live closed [{}].", f.code),
                        None => "Google Gemini Live connection closed.".to_string(),
                    };
                    return Err(msg);
                }
                Ok(_) => {}
                Err(e) => return Err(format!("Google Gemini Live handshake error: {e}")),
            }
        }
        Err("Google Gemini Live closed before setup completed.".to_string())
    })
    .await;

    match wait_setup {
        Ok(Ok(())) => Ok(ws_stream),
        Ok(Err(err)) => Err(err),
        Err(_) => Err("Timed out waiting for Google Gemini Live setup confirmation.".to_string()),
    }
}

pub fn encode_f32_to_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let data_bytes = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_bytes as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_bytes.to_le_bytes());

    for &sample in samples {
        let clamped = sample.clamp(-1.0, 1.0);
        let pcm = if clamped < 0.0 {
            (clamped * 32768.0) as i16
        } else {
            (clamped * 32767.0) as i16
        };
        wav.extend_from_slice(&pcm.to_le_bytes());
    }
    wav
}

pub fn build_generate_content_config(model: &GoogleModelInfo) -> Option<Value> {
    if model.kind == "transcribe" {
        return None;
    }
    let id = model.id.to_ascii_lowercase();
    let is_gen3_or_4 = id.starts_with("gemini-3.")
        || id.starts_with("gemini-3-")
        || id.starts_with("gemini-4.")
        || id.starts_with("gemini-4-");

    if is_gen3_or_4 {
        let level = if id.contains("flash") {
            "MINIMAL"
        } else {
            "LOW"
        };
        Some(json!({
            "temperature": 0.1,
            "maxOutputTokens": 256,
            "thinkingConfig": {
                "thinkingLevel": level
            }
        }))
    } else if id.starts_with("gemini-2.5-flash") {
        Some(json!({
            "temperature": 0.1,
            "maxOutputTokens": 256,
            "thinkingConfig": {
                "thinkingBudget": 0
            }
        }))
    } else {
        Some(json!({
            "temperature": 0.1,
            "maxOutputTokens": 256
        }))
    }
}

pub fn extract_candidate_text(payload: &Value) -> String {
    let Some(parts) = payload
        .get("candidates")
        .and_then(Value::as_array)
        .and_then(|c| c.first())
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(Value::as_array)
    else {
        return String::new();
    };

    parts
        .iter()
        .filter(|p| !p.get("thought").and_then(Value::as_bool).unwrap_or(false))
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect::<String>()
}

#[allow(clippy::too_many_arguments)]
pub async fn transcribe_chunk_with_gemini<F>(
    client: &reqwest::Client,
    api_key: &str,
    model: &GoogleModelInfo,
    samples: &[f32],
    language: &str,
    target_language: &str,
    last_context: &str,
    mut on_interim: F,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    let trimmed_key = api_key.trim();
    if trimmed_key.is_empty() {
        return Err("Set your Gemini API key in Settings before starting.".to_string());
    }

    let wav = encode_f32_to_wav(samples, 16_000);
    let wav_b64 = STANDARD.encode(&wav);

    let audio_part = json!({
        "inlineData": {
            "mimeType": "audio/wav",
            "data": wav_b64
        }
    });

    let parts = if model.kind == "transcribe" {
        if language != "auto" {
            let lang_name = language_name(language);
            vec![
                audio_part,
                json!({ "text": format!("Transcribe the {lang_name} speech accurately. Output only the transcript.") }),
            ]
        } else {
            vec![audio_part]
        }
    } else {
        let spoken_hint = if language != "auto" {
            format!(" The spoken language is {}.", language_name(language))
        } else {
            String::new()
        };
        let context_hint = if last_context.is_empty() {
            String::new()
        } else {
            format!(" Previous subtitle for context (do not repeat): \"{last_context}\".")
        };

        if target_language != "same" {
            let target_name = language_name(target_language);
            vec![
                audio_part,
                json!({
                    "text": format!(
                        "Listen to this audio.{spoken_hint}{context_hint} Translate the spoken dialogue concisely into {target_name} as a live subtitle. Output only the translation, or an empty string if there is no speech."
                    )
                }),
            ]
        } else {
            vec![
                audio_part,
                json!({
                    "text": format!(
                        "Transcribe the spoken dialogue verbatim.{spoken_hint}{context_hint} Output only the transcript, or an empty string if there is no speech."
                    )
                }),
            ]
        }
    };

    let use_sse = model.kind == "multimodal"
        && (model.supported_generation_methods.is_empty()
            || model
                .supported_generation_methods
                .iter()
                .any(|m| m == "streamGenerateContent"));
    let endpoint = if use_sse {
        "streamGenerateContent?alt=sse"
    } else {
        "generateContent"
    };

    let url = format!(
        "{GEMINI_API_BASE}/models/{}:{endpoint}",
        percent_encode(&model.id)
    );
    let mut body = json!({
        "contents": [{ "parts": parts }]
    });
    if let Some(cfg) = build_generate_content_config(model)
        && let Some(obj) = body.as_object_mut()
    {
        obj.insert("generationConfig".to_string(), cfg);
    }

    let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let response = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-goog-api-key", trimmed_key)
        .body(body_str)
        .send()
        .await
        .map_err(|e| format!("Google Gemini request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let err_body = response.text().await.unwrap_or_default();
        return Err(extract_api_error_message(status, &err_body));
    }

    if !use_sse {
        let body_text = response
            .text()
            .await
            .map_err(|e| format!("Failed to read Gemini response: {e}"))?;
        let payload: Value = serde_json::from_str(&body_text)
            .map_err(|e| format!("Invalid Gemini response JSON: {e}"))?;
        return Ok(extract_candidate_text(&payload).trim().to_string());
    }

    let mut stream = response.bytes_stream();
    let mut raw_buf: Vec<u8> = Vec::new();
    let mut accumulated = String::new();

    let mut process_line = |line_bytes: &[u8]| {
        let line = String::from_utf8_lossy(line_bytes);
        let trimmed = line.trim();
        let Some(data_part) = trimmed.strip_prefix("data:") else {
            return;
        };
        let json_str = data_part.trim();
        if json_str.is_empty() || json_str == "[DONE]" {
            return;
        }
        if let Ok(parsed) = serde_json::from_str::<Value>(json_str) {
            let chunk = extract_candidate_text(&parsed);
            if !chunk.is_empty() {
                accumulated.push_str(&chunk);
                let display = accumulated.trim();
                if !display.is_empty() {
                    on_interim(display);
                }
            }
        }
    };

    while let Some(item) = stream.next().await {
        let bytes = item.map_err(|e| format!("Failed reading Gemini stream: {e}"))?;
        raw_buf.extend_from_slice(&bytes);
        while let Some(pos) = raw_buf.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = raw_buf.drain(..=pos).collect();
            process_line(&line_bytes);
        }
    }
    if !raw_buf.is_empty() {
        process_line(&raw_buf);
    }

    Ok(accumulated.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str, transport: &str, kind: &str) -> GoogleModelInfo {
        GoogleModelInfo {
            id: id.to_string(),
            name: format!("models/{id}"),
            display_name: id.to_string(),
            description: String::new(),
            supported_generation_methods: vec![
                "generateContent".to_string(),
                "streamGenerateContent".to_string(),
            ],
            transport: transport.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn parses_filters_classifies_and_sorts_google_models() {
        let raw = vec![
            json!({
                "name": "models/text-embedding-004",
                "displayName": "Embedding",
                "supportedGenerationMethods": ["embedContent"]
            }),
            json!({
                "name": "models/imagen-3.0-generate-002",
                "displayName": "Imagen 3",
                "supportedGenerationMethods": ["generateContent"]
            }),
            json!({
                "name": "models/gemma-3-27b-it",
                "displayName": "Gemma 3",
                "supportedGenerationMethods": ["generateContent"]
            }),
            json!({
                "name": "models/gemini-2.5-flash",
                "displayName": "Gemini 2.5 Flash",
                "supportedGenerationMethods": ["generateContent"]
            }),
            json!({
                "name": "models/gemini-3.5-transcribe",
                "displayName": "Gemini 3.5 Transcribe",
                "supportedGenerationMethods": ["generateContent"]
            }),
            json!({
                "name": "models/gemini-2.5-flash-native-audio-preview",
                "displayName": "Gemini 2.5 Native Audio",
                "supportedGenerationMethods": ["bidiGenerateContent"]
            }),
            json!({
                "name": "models/gemini-3.5-live-translate-preview",
                "displayName": "Gemini 3.5 Live Translate",
                "supportedGenerationMethods": ["bidiGenerateContent"]
            }),
            json!({
                "name": "models/gemini-3.5-transcribe-live",
                "displayName": "Gemini 3.5 Transcribe Live",
                "supportedGenerationMethods": ["bidiGenerateContent"]
            }),
        ];

        let parsed = parse_google_models_response(&raw);
        let summary: Vec<(&str, &str, &str)> = parsed
            .iter()
            .map(|m| (m.id.as_str(), m.transport.as_str(), m.kind.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("gemini-3.5-transcribe-live", "live", "transcribe-live"),
                ("gemini-3.5-live-translate-preview", "live", "translate-live"),
                ("gemini-2.5-flash-native-audio-preview", "live", "live"),
                ("gemini-3.5-transcribe", "generate", "transcribe"),
                ("gemini-2.5-flash", "generate", "multimodal"),
            ]
        );
    }

    #[test]
    fn builds_live_setup_messages_correctly() {
        let transcribe_live = model("gemini-3.5-transcribe-live", "live", "transcribe-live");
        assert_eq!(
            build_live_setup_message(&transcribe_live, "ja", "same"),
            json!({
                "setup": {
                    "model": "models/gemini-3.5-transcribe-live",
                    "generationConfig": { "responseModalities": ["TEXT"] },
                    "inputAudioTranscription": { "languageCodes": ["ja-JP"] }
                }
            })
        );
        assert_eq!(
            build_live_setup_message(&transcribe_live, "auto", "same"),
            json!({
                "setup": {
                    "model": "models/gemini-3.5-transcribe-live",
                    "generationConfig": { "responseModalities": ["TEXT"] },
                    "inputAudioTranscription": {}
                }
            })
        );

        let translate_live = model("gemini-3.5-live-translate-preview", "live", "translate-live");
        assert_eq!(
            build_live_setup_message(&translate_live, "ja", "same"),
            json!({
                "setup": {
                    "model": "models/gemini-3.5-live-translate-preview",
                    "systemInstruction": {
                        "parts": [{
                            "text": "Translate all spoken audio accurately and naturally into English."
                        }]
                    },
                    "generationConfig": {
                        "responseModalities": ["AUDIO"],
                        "translationConfig": {
                            "targetLanguageCode": "en-US",
                            "echoTargetLanguage": false
                        }
                    },
                    "outputAudioTranscription": {}
                }
            })
        );

        let live = model("gemini-2.5-flash-native-audio-preview", "live", "live");
        let setup_tr = build_live_setup_message(&live, "ja", "en");
        assert!(setup_tr["setup"]["outputAudioTranscription"].is_object());
        assert!(
            setup_tr["setup"]["systemInstruction"]["parts"][0]["text"]
                .as_str()
                .unwrap()
                .contains("English")
        );
        let setup_stt = build_live_setup_message(&live, "ja", "same");
        assert!(setup_stt["setup"]["outputAudioTranscription"].is_null());
    }

    #[test]
    fn live_accumulator_handles_interim_turn_complete_and_output_mode() {
        let mut acc = GeminiLiveAccumulator::new(LiveOutputMode::Input, 60);
        assert_eq!(
            acc.push_server_content(&json!({ "interimInputTranscription": { "text": "Hel" } })),
            vec![("Hel".to_string(), false)]
        );
        assert_eq!(
            acc.push_server_content(&json!({ "inputTranscription": { "text": "Hello" } })),
            vec![("Hello".to_string(), false)]
        );
        assert_eq!(
            acc.push_server_content(&json!({
                "inputTranscription": { "text": "world" },
                "interimInputTranscription": { "text": "today" }
            })),
            vec![("Hello world today".to_string(), false)]
        );
        assert_eq!(
            acc.push_server_content(&json!({ "turnComplete": true })),
            vec![("Hello world today".to_string(), true)]
        );
        assert_eq!(
            acc.push_server_content(&json!({
                "inputTranscription": { "text": "This is a complete sentence that is long enough." }
            })),
            vec![(
                "This is a complete sentence that is long enough.".to_string(),
                true
            )]
        );

        let mut strict_out = GeminiLiveAccumulator::new(LiveOutputMode::OutputOnly, 140);
        assert_eq!(
            strict_out.push_server_content(&json!({
                "inputTranscription": { "text": "こんにちは" },
                "modelTurn": { "parts": [{ "text": "ignored modelTurn" }] }
            })),
            vec![]
        );
        assert_eq!(
            strict_out.push_server_content(&json!({
                "outputTranscription": { "text": "Hello" },
                "turnComplete": true
            })),
            vec![
                ("Hello".to_string(), false),
                ("Hello".to_string(), true)
            ]
        );

        let mut out_acc = GeminiLiveAccumulator::new(LiveOutputMode::OutputOrModelTurn, 140);
        assert_eq!(
            out_acc.push_server_content(&json!({
                "inputTranscription": { "text": "こんにちは" },
                "outputTranscription": { "text": "Hello" }
            })),
            vec![("Hello".to_string(), false)]
        );
        assert_eq!(
            out_acc.push_server_content(&json!({
                "modelTurn": {
                    "parts": [
                        { "text": "internal thought", "thought": true },
                        { "text": "there!" }
                    ]
                },
                "turnComplete": true
            })),
            vec![
                ("Hello there!".to_string(), false),
                ("Hello there!".to_string(), true)
            ]
        );
    }

    #[test]
    fn builds_thinking_config_extracts_text_and_encodes_wav() {
        assert_eq!(
            build_generate_content_config(&model(
                "gemini-3-flash-preview",
                "generate",
                "multimodal"
            )),
            Some(json!({
                "temperature": 0.1,
                "maxOutputTokens": 256,
                "thinkingConfig": { "thinkingLevel": "MINIMAL" }
            }))
        );
        assert_eq!(
            build_generate_content_config(&model(
                "gemini-3-pro-preview",
                "generate",
                "multimodal"
            )),
            Some(json!({
                "temperature": 0.1,
                "maxOutputTokens": 256,
                "thinkingConfig": { "thinkingLevel": "LOW" }
            }))
        );
        assert_eq!(
            build_generate_content_config(&model("gemini-2.5-flash", "generate", "multimodal")),
            Some(json!({
                "temperature": 0.1,
                "maxOutputTokens": 256,
                "thinkingConfig": { "thinkingBudget": 0 }
            }))
        );
        assert_eq!(
            build_generate_content_config(&model(
                "gemini-3.5-transcribe",
                "generate",
                "transcribe"
            )),
            None
        );

        assert_eq!(
            extract_candidate_text(&json!({
                "candidates": [{
                    "content": {
                        "parts": [
                            { "text": "secret", "thought": true },
                            { "text": "Hello " },
                            { "text": "world!" }
                        ]
                    }
                }]
            })),
            "Hello world!"
        );

        let wav = encode_f32_to_wav(&[0.0, 0.5, -0.5], 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + 6);
    }

    #[test]
    fn builds_generate_config_and_accumulates_input_transcripts() {
        let make_model = |id: &str, kind: &str| GoogleModelInfo {
            id: id.to_string(),
            name: format!("models/{id}"),
            display_name: id.to_string(),
            description: String::new(),
            supported_generation_methods: vec!["generateContent".to_string()],
            transport: "generate".to_string(),
            kind: kind.to_string(),
        };

        assert_eq!(
            build_generate_content_config(&make_model("gemini-3.5-transcribe", "transcribe")),
            None
        );
        assert_eq!(
            build_generate_content_config(&make_model("gemini-2.5-flash", "multimodal")),
            Some(json!({
                "temperature": 0.1,
                "maxOutputTokens": 256,
                "thinkingConfig": { "thinkingBudget": 0 }
            }))
        );
        assert_eq!(
            build_generate_content_config(&make_model("gemini-3-flash-preview", "multimodal")),
            Some(json!({
                "temperature": 0.1,
                "maxOutputTokens": 256,
                "thinkingConfig": { "thinkingLevel": "MINIMAL" }
            }))
        );

        let mut acc = GeminiLiveAccumulator::new(LiveOutputMode::Input, 140);
        assert_eq!(
            acc.push_server_content(&json!({
                "inputTranscription": { "text": "こんにちは、元気ですか？" }
            })),
            vec![("こんにちは、元気ですか？".to_string(), true)]
        );
    }
}
