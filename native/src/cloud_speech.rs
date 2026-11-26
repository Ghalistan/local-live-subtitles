use crate::gemini::to_bcp47;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::net::TcpStream;
use tokio_rustls::{TlsConnector, client::TlsStream};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error, Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue, Request, header::AUTHORIZATION},
    },
};

pub type CloudSpeechWs = WebSocketStream<TlsStream<TcpStream>>;

pub fn is_elevenlabs_model_id(id: &str) -> bool {
    let mut bytes = id.bytes();
    let is_alphanumeric = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    id.len() <= 128
        && bytes.next().is_some_and(is_alphanumeric)
        && bytes.all(|byte| is_alphanumeric(byte) || matches!(byte, b'_' | b'-'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudSpeechProvider {
    Soniox,
    ElevenLabs,
}

impl CloudSpeechProvider {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "soniox" => Some(Self::Soniox),
            "elevenlabs" => Some(Self::ElevenLabs),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Soniox => "soniox",
            Self::ElevenLabs => "elevenlabs",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Soniox => "Soniox",
            Self::ElevenLabs => "ElevenLabs",
        }
    }

    pub fn model_id(self, requested: Option<&str>) -> Result<&str, String> {
        match self {
            Self::Soniox => Ok("stt-rt-v6"),
            Self::ElevenLabs => match requested {
                None | Some("") => Ok("scribe_v2_realtime"),
                Some(id) if is_elevenlabs_model_id(id) => Ok(id),
                Some(_) => Err("Select a supported ElevenLabs real-time speech model.".into()),
            },
        }
    }

    pub async fn connect(
        self,
        api_key: &str,
        language: &str,
        model_id: &str,
    ) -> Result<CloudSpeechWs, String> {
        let label = self.label();
        let api_key = api_key.trim();
        if api_key.is_empty() {
            return Err(format!(
                "Set your {label} API key in Settings before starting."
            ));
        }
        let language = to_bcp47(language).map(|_| language);
        tokio::time::timeout(Duration::from_secs(15), self.open(api_key, language, model_id))
            .await
            .map_err(|_| format!("Timed out connecting to {label}."))?
    }

    fn request(
        self,
        api_key: &str,
        language: Option<&str>,
        model_id: &str,
    ) -> Result<Request<()>, String> {
        let (url, auth_header, auth_value) = match self {
            Self::Soniox => (
                "wss://stt-rt.soniox.com/transcribe-websocket".to_string(),
                AUTHORIZATION,
                format!("Bearer {api_key}"),
            ),
            Self::ElevenLabs => {
                let mut url = format!(
                    "wss://api.elevenlabs.io/v1/speech-to-text/realtime?model_id={model_id}&audio_format=pcm_16000&commit_strategy=vad",
                );
                if let Some(language) = language {
                    url.push_str("&language_code=");
                    url.push_str(language);
                }
                (
                    url,
                    HeaderName::from_static("xi-api-key"),
                    api_key.to_string(),
                )
            }
        };
        let mut request = url
            .into_client_request()
            .map_err(|_| format!("Could not build the {} connection request.", self.label()))?;
        let mut header = HeaderValue::from_str(&auth_value)
            .map_err(|_| format!("Invalid characters in the {} API key.", self.label()))?;
        header.set_sensitive(true);
        request.headers_mut().insert(auth_header, header);
        Ok(request)
    }

    fn soniox_setup(language: Option<&str>, model_id: &str) -> Value {
        let mut setup = json!({
            "model": model_id,
            "audio_format": "pcm_s16le",
            "sample_rate": 16000,
            "num_channels": 1,
            "enable_endpoint_detection": true
        });
        if let Some(language) = language {
            setup["language_hints"] = json!([language]);
        }
        setup
    }

    async fn open(
        self,
        api_key: &str,
        language: Option<&str>,
        model_id: &str,
    ) -> Result<CloudSpeechWs, String> {
        let label = self.label();
        let request = self.request(api_key, language, model_id)?;
        let host = request
            .uri()
            .host()
            .ok_or_else(|| format!("Invalid {label} hostname."))?
            .to_owned();
        let tcp = TcpStream::connect((host.as_str(), 443))
            .await
            .map_err(|err| format!("Could not connect to {label}: {err}"))?;
        let _ = tcp.set_nodelay(true);
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|err| format!("TLS setup failed: {err}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        let hostname = rustls_pki_types::ServerName::try_from(host)
            .map_err(|_| format!("Invalid {label} hostname."))?;
        let stream = TlsConnector::from(Arc::new(tls))
            .connect(hostname, tcp)
            .await
            .map_err(|err| format!("TLS handshake with {label} failed: {err}"))?;
        let (mut socket, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|err| match err {
                Error::Http(response) => self.http_error(response.status().as_u16()),
                _ => format!("{label} WebSocket handshake failed."),
            })?;

        if self == Self::Soniox {
            let setup = Self::soniox_setup(language, model_id);
            // The server answers this ping only once the transcription stream is ready.
            for payload in [setup, json!({"type": "ping", "id": 1})] {
                socket
                    .send(Message::Text(payload.to_string().into()))
                    .await
                    .map_err(|_| format!("Could not send {label} session setup."))?;
            }
        }

        while let Some(Ok(frame)) = socket.next().await {
            let payload = match frame {
                Message::Text(text) => serde_json::from_str::<Value>(text.as_str()),
                Message::Binary(bytes) => serde_json::from_slice::<Value>(&bytes),
                Message::Close(_) => break,
                _ => continue,
            }
            .map_err(|_| format!("{label} returned an invalid setup response."))?;
            if let Some(err) = self.response_error(&payload) {
                return Err(err);
            }
            let ready = match self {
                Self::Soniox => payload["type"] == "pong" && payload["id"] == 1,
                Self::ElevenLabs => payload["message_type"] == "session_started",
            };
            if ready {
                return Ok(socket);
            }
        }
        Err(format!(
            "{label} closed the connection before the session started."
        ))
    }

    pub fn audio_message(self, pcm16: &[u8]) -> Message {
        match self {
            Self::Soniox => Message::Binary(pcm16.to_vec().into()),
            Self::ElevenLabs => Message::Text(
                json!({
                    "message_type": "input_audio_chunk",
                    "audio_base_64": STANDARD.encode(pcm16),
                    "commit": false,
                    "sample_rate": 16000
                })
                .to_string()
                .into(),
            ),
        }
    }

    fn http_error(self, status: u16) -> String {
        let detail = match status {
            401 | 403 => " Check your API key and speech-to-text permissions.",
            402 => " Check your account credits and usage limits.",
            429 => " Check your usage limits or try again later.",
            _ => "",
        };
        format!(
            "{} rejected the speech session (HTTP {status}).{detail}",
            self.label()
        )
    }

    fn response_error(self, payload: &Value) -> Option<String> {
        // Provider response text can contain credentials or session details.
        match self {
            Self::Soniox => payload["error_code"]
                .as_u64()
                .and_then(|status| u16::try_from(status).ok())
                .map(|status| self.http_error(status)),
            Self::ElevenLabs => {
                let message_type = payload["message_type"].as_str()?;
                let detail = match message_type {
                    "auth_error" => "Check your API key and speech-to-text permissions.",
                    "quota_exceeded" => "Check your account credits and usage limits.",
                    "unaccepted_terms" => "Accept the Scribe terms in your ElevenLabs dashboard.",
                    "rate_limited" | "resource_exhausted" | "queue_overflow" => {
                        "Try again later or check your usage limits."
                    }
                    "session_time_limit_exceeded" => {
                        "The session limit was reached. Start a new session."
                    }
                    "insufficient_audio_activity" => "The session timed out waiting for audio.",
                    "input_error" | "invalid_request" | "chunk_size_exceeded" => {
                        "The audio stream or session settings were rejected."
                    }
                    "error" | "transcriber_error" | "commit_throttled" => {
                        "Try starting a new session."
                    }
                    _ => return None,
                };
                Some(format!("ElevenLabs speech session failed. {detail}"))
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CloudSpeechEvent {
    Interim(String),
    Final(String),
    Finished,
}

pub struct CloudSpeechTranscript {
    provider: CloudSpeechProvider,
    soniox_finalized: String,
    last_interim: String,
}

impl CloudSpeechTranscript {
    pub fn new(provider: CloudSpeechProvider) -> Self {
        Self {
            provider,
            soniox_finalized: String::new(),
            last_interim: String::new(),
        }
    }

    pub fn handle(&mut self, payload: &[u8]) -> Result<Vec<CloudSpeechEvent>, String> {
        let payload: Value = serde_json::from_slice(payload).map_err(|_| {
            format!(
                "{} returned an invalid speech response.",
                self.provider.label()
            )
        })?;
        if let Some(error) = self.provider.response_error(&payload) {
            return Err(error);
        }

        let mut events = Vec::new();
        match self.provider {
            CloudSpeechProvider::Soniox => {
                let mut partial = String::new();
                if let Some(tokens) = payload["tokens"].as_array() {
                    // Final tokens are deltas. The current non-final suffix is replaced per response.
                    for token in tokens {
                        let Some(text) = token["text"].as_str() else {
                            continue;
                        };
                        let is_final = token["is_final"] == true;
                        if matches!(text, "<end>" | "<fin>") {
                            if is_final {
                                let finalized = std::mem::take(&mut self.soniox_finalized);
                                self.commit(&finalized, &mut events);
                                partial.clear();
                            }
                        } else if is_final {
                            self.soniox_finalized.push_str(text);
                        } else {
                            partial.push_str(text);
                        }
                    }
                }
                if payload["finished"] == true {
                    let finalized = std::mem::take(&mut self.soniox_finalized);
                    self.commit(&finalized, &mut events);
                    events.push(CloudSpeechEvent::Finished);
                } else if payload["tokens"].is_array() {
                    let caption = format!("{}{partial}", self.soniox_finalized);
                    self.interim(&caption, &mut events);
                }
            }
            CloudSpeechProvider::ElevenLabs => {
                if let Some(text) = payload["text"].as_str() {
                    match payload["message_type"].as_str() {
                        Some("partial_transcript") => self.interim(text, &mut events),
                        Some("committed_transcript") => self.commit(text, &mut events),
                        _ => {}
                    }
                }
            }
        }
        Ok(events)
    }

    fn interim(&mut self, text: &str, events: &mut Vec<CloudSpeechEvent>) {
        let text = text.trim();
        if text != self.last_interim {
            self.last_interim = text.to_string();
            events.push(CloudSpeechEvent::Interim(text.to_string()));
        }
    }

    fn commit(&mut self, text: &str, events: &mut Vec<CloudSpeechEvent>) {
        let text = text.trim();
        if text.is_empty() {
            self.interim("", events);
        } else {
            self.last_interim.clear();
            events.push(CloudSpeechEvent::Final(text.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CloudSpeechEvent::{Final, Finished, Interim};
    use CloudSpeechProvider::{ElevenLabs, Soniox};

    #[test]
    fn prepares_provider_requests_and_audio() {
        let soniox_model_id = Soniox.model_id(None).unwrap();
        let request = Soniox.request("test-key", Some("ja"), soniox_model_id).unwrap();
        assert_eq!(request.uri().path(), "/transcribe-websocket");
        assert_eq!(request.headers()[AUTHORIZATION], "Bearer test-key");
        assert!(request.headers()[AUTHORIZATION].is_sensitive());
        assert!(request.uri().query().is_none());
        let setup = CloudSpeechProvider::soniox_setup(Some("ja"), soniox_model_id);
        assert_eq!(setup["model"], soniox_model_id);
        assert_eq!(setup["language_hints"], json!(["ja"]));
        assert_eq!(setup["sample_rate"], 16000);
        assert!(
            CloudSpeechProvider::soniox_setup(None, soniox_model_id)
                .get("language_hints")
                .is_none()
        );
        assert_eq!(
            Soniox.audio_message(&[1, 2]),
            Message::Binary(vec![1, 2].into())
        );

        let selected = ElevenLabs.model_id(Some("scribe_v3_live")).unwrap();
        let request = ElevenLabs.request("test-key", Some("id"), selected).unwrap();
        assert_eq!(request.headers()["xi-api-key"], "test-key");
        let query = request.uri().query().unwrap();
        assert!(query.contains("model_id=scribe_v3_live"));
        assert!(query.contains("commit_strategy=vad"));
        assert!(query.contains("language_code=id"));
        let frame = ElevenLabs.audio_message(&[1, 2]);
        let encoded: Value = serde_json::from_str(frame.to_text().unwrap()).unwrap();
        assert_eq!(encoded["message_type"], "input_audio_chunk");
        assert_eq!(encoded["audio_base_64"], "AQI=");
        assert_eq!(encoded["commit"], false);
    }

    #[test]
    fn preserves_the_default_and_validates_discovered_model_ids() {
        assert_eq!(ElevenLabs.model_id(None).unwrap(), "scribe_v2_realtime");
        assert_eq!(
            ElevenLabs.model_id(Some("next_model-2")).unwrap(),
            "next_model-2"
        );
        for invalid in [" scribe_v3_live", "scribe&token=123", "../scribe", "-model"] {
            assert!(ElevenLabs.model_id(Some(invalid)).is_err(), "{invalid}");
        }
        assert!(!is_elevenlabs_model_id(&"x".repeat(129)));
    }

    #[test]
    fn soniox_accumulates_stable_tokens_and_replaces_partial_tokens() {
        let mut transcript = CloudSpeechTranscript::new(Soniox);
        assert_eq!(
            transcript.handle(br#"{"tokens":[{"text":"Hello","is_final":true},{"text":" wor","is_final":false}]}"#).unwrap(),
            vec![Interim("Hello wor".into())],
        );
        assert_eq!(
            transcript.handle(br#"{"tokens":[{"text":" world.","is_final":true},{"text":"<end>","is_final":true},{"text":"Next","is_final":false}]}"#).unwrap(),
            vec![Final("Hello world.".into()), Interim("Next".into())],
        );
        assert_eq!(
            transcript
                .handle(br#"{"tokens":[{"text":"Next one.","is_final":true}],"finished":true}"#)
                .unwrap(),
            vec![Final("Next one.".into()), Finished],
        );
    }

    #[test]
    fn elevenlabs_replaces_interims_and_reports_safe_errors() {
        let mut transcript = CloudSpeechTranscript::new(ElevenLabs);
        let partial = br#"{"message_type":"partial_transcript","text":"Good m"}"#;
        assert_eq!(
            transcript.handle(partial).unwrap(),
            vec![Interim("Good m".into())]
        );
        assert!(transcript.handle(partial).unwrap().is_empty());
        assert_eq!(
            transcript
                .handle(br#"{"message_type":"partial_transcript","text":""}"#)
                .unwrap(),
            vec![Interim(String::new())],
        );
        assert_eq!(
            transcript
                .handle(br#"{"message_type":"committed_transcript","text":"Good morning."}"#)
                .unwrap(),
            vec![Final("Good morning.".into())],
        );
        let err = transcript
            .handle(br#"{"message_type":"auth_error","error":"secret-key"}"#)
            .unwrap_err();
        assert!(err.contains("API key") && !err.contains("secret-key"));
    }
}
