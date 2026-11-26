use crate::audio::{SegmenterConfig, compute_rms, pcm_i16_le_to_f32};
use crate::diarizer::SpeakerTimeline;
use crate::engine::detect_speech_prob;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use whisper_rs::WhisperVadContext;

pub const VAD_WINDOW_SAMPLES: usize = 8192;
pub const VAD_HOP_SAMPLES: usize = 2048;

const DEFAULT_RMS_THRESHOLD: f32 = 0.003;
const SILERO_SILENCE_THRESHOLD: f32 = 0.35;
const LIVE_PRE_ROLL_HOPS: usize = 2; // ~256 ms pre-roll (2 * 2048 samples)
const LIVE_HANGOVER_SAMPLES: usize = 14_400; // 900 ms trailing silence for Gemini Live turnComplete
const BYPASS_SILENCE_MS: usize = 86_400_000; // 24h without overflowing sample multiplication

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VadModel {
    SileroV5,
    Rms,
}

impl VadModel {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("") {
            "rms" => Self::Rms,
            _ => Self::SileroV5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMechanism {
    SilenceBalanced,
    SilenceFast,
    FixedWindow,
}

impl EndpointMechanism {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).unwrap_or("") {
            "silence-fast" => Self::SilenceFast,
            "fixed-window" => Self::FixedWindow,
            _ => Self::SilenceBalanced,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiarizationModel {
    Disabled,
    NemotronSortformer,
}

impl DiarizationModel {
    pub fn parse(raw: Option<&str>, legacy_enabled: bool) -> Self {
        match raw.map(str::trim) {
            Some("nemotron-sortformer") => Self::NemotronSortformer,
            Some(_) => Self::Disabled,
            None => {
                if legacy_enabled {
                    Self::NemotronSortformer
                } else {
                    Self::Disabled
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PipelineConfig {
    pub enable_vad_endpointing: bool,
    pub vad_model: VadModel,
    pub endpoint_mechanism: EndpointMechanism,
    pub diarization_model: DiarizationModel,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            enable_vad_endpointing: true,
            vad_model: VadModel::SileroV5,
            endpoint_mechanism: EndpointMechanism::SilenceBalanced,
            diarization_model: DiarizationModel::Disabled,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VadHop {
    pub f32_samples: Vec<f32>,
    pub pcm16_bytes: Vec<u8>,
    pub speech_prob: Option<f32>,
    pub is_voiced: bool,
}

pub struct VadStage {
    enabled: bool,
    model: VadModel,
    vad_ctx: Option<Arc<Mutex<WhisperVadContext>>>,
    vad_ring: VecDeque<f32>,
    pending_f32: Vec<f32>,
    pending_pcm16: Vec<u8>,
}

impl VadStage {
    pub fn new(
        enabled: bool,
        model: VadModel,
        vad_ctx: Option<Arc<Mutex<WhisperVadContext>>>,
    ) -> Self {
        Self {
            enabled,
            model,
            vad_ctx,
            vad_ring: VecDeque::with_capacity(VAD_WINDOW_SAMPLES + VAD_HOP_SAMPLES),
            pending_f32: Vec::with_capacity(VAD_HOP_SAMPLES * 2),
            pending_pcm16: Vec::with_capacity(VAD_HOP_SAMPLES * 4),
        }
    }

    pub fn configure(
        &mut self,
        enabled: bool,
        model: VadModel,
        vad_ctx: Option<Arc<Mutex<WhisperVadContext>>>,
    ) {
        self.enabled = enabled;
        self.model = model;
        self.vad_ctx = vad_ctx;
        self.reset();
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn buffered_samples(&self) -> usize {
        self.pending_f32.len()
    }

    pub fn reset(&mut self) {
        self.vad_ring.clear();
        self.pending_f32.clear();
        self.pending_pcm16.clear();
    }

    pub async fn push_pcm16(&mut self, bytes: &[u8]) -> Vec<VadHop> {
        let usable_bytes = bytes.len() - (bytes.len() % 2);
        if usable_bytes == 0 {
            return Vec::new();
        }
        let slice = &bytes[..usable_bytes];
        let samples = pcm_i16_le_to_f32(slice);
        self.pending_f32.extend_from_slice(&samples);
        self.pending_pcm16.extend_from_slice(slice);

        let mut hops = Vec::new();
        let hop_byte_len = VAD_HOP_SAMPLES * 2;
        while self.pending_f32.len() >= VAD_HOP_SAMPLES {
            let hop_f32: Vec<f32> = self.pending_f32.drain(..VAD_HOP_SAMPLES).collect();
            let hop_pcm16: Vec<u8> = self.pending_pcm16.drain(..hop_byte_len).collect();

            if !self.enabled {
                hops.push(VadHop {
                    f32_samples: hop_f32,
                    pcm16_bytes: hop_pcm16,
                    speech_prob: Some(1.0),
                    is_voiced: true,
                });
                continue;
            }

            self.vad_ring.extend(hop_f32.iter().copied());
            while self.vad_ring.len() > VAD_WINDOW_SAMPLES {
                self.vad_ring.pop_front();
            }

            let speech_prob = if self.model == VadModel::SileroV5
                && let Some(vad) = self.vad_ctx.as_ref()
            {
                let window: Vec<f32> = self.vad_ring.iter().copied().collect();
                let vad_arc = Arc::clone(vad);
                tokio::task::spawn_blocking(move || detect_speech_prob(&vad_arc, &window))
                    .await
                    .ok()
                    .flatten()
            } else {
                None
            };

            let is_voiced = match speech_prob {
                Some(prob) => prob >= SILERO_SILENCE_THRESHOLD,
                None => compute_rms(&hop_f32) >= DEFAULT_RMS_THRESHOLD,
            };

            hops.push(VadHop {
                f32_samples: hop_f32,
                pcm16_bytes: hop_pcm16,
                speech_prob,
                is_voiced,
            });
        }

        hops
    }
}

pub fn build_segmenter_config(cfg: &PipelineConfig, is_google_chunked: bool) -> SegmenterConfig {
    let overlap_ms = if is_google_chunked { 0 } else { 400 };
    let interim_interval_ms = if is_google_chunked { usize::MAX } else { 750 };

    if !cfg.enable_vad_endpointing {
        return SegmenterConfig {
            rms_threshold: 0.0,
            speech_threshold: 0.0,
            silence_threshold: 0.0,
            pre_roll_ms: 0,
            min_speech_ms: 0,
            silence_ms: BYPASS_SILENCE_MS,
            max_chunk_ms: 3000,
            overlap_ms,
            interim_interval_ms,
            ..SegmenterConfig::default()
        };
    }

    match cfg.endpoint_mechanism {
        EndpointMechanism::SilenceBalanced => {
            if is_google_chunked {
                SegmenterConfig {
                    silence_ms: 400,
                    max_chunk_ms: 2800,
                    min_speech_ms: 300,
                    overlap_ms: 0,
                    interim_interval_ms: usize::MAX,
                    ..SegmenterConfig::default()
                }
            } else {
                SegmenterConfig::default()
            }
        }
        EndpointMechanism::SilenceFast => {
            if is_google_chunked {
                SegmenterConfig {
                    silence_ms: 250,
                    max_chunk_ms: 2400,
                    min_speech_ms: 250,
                    overlap_ms: 0,
                    interim_interval_ms: usize::MAX,
                    ..SegmenterConfig::default()
                }
            } else {
                SegmenterConfig {
                    silence_ms: 250,
                    max_chunk_ms: 2800,
                    min_speech_ms: 250,
                    overlap_ms: 250,
                    interim_interval_ms: 550,
                    ..SegmenterConfig::default()
                }
            }
        }
        EndpointMechanism::FixedWindow => SegmenterConfig {
            silence_ms: 1200,
            max_chunk_ms: 3000,
            overlap_ms,
            interim_interval_ms,
            ..SegmenterConfig::default()
        },
    }
}

pub struct LiveGateOutput {
    pub pcm16: Vec<u8>,
    pub diar_f32: Vec<f32>,
    pub is_speaking: bool,
}

pub struct LiveSpeechGate {
    pre_roll: VecDeque<Vec<u8>>,
    active: bool,
    silence_samples: usize,
}

impl Default for LiveSpeechGate {
    fn default() -> Self {
        Self {
            pre_roll: VecDeque::with_capacity(LIVE_PRE_ROLL_HOPS + 1),
            active: false,
            silence_samples: 0,
        }
    }
}

impl LiveSpeechGate {
    pub fn reset(&mut self) {
        self.pre_roll.clear();
        self.active = false;
        self.silence_samples = 0;
    }

    pub fn push_hop(&mut self, hop: &VadHop) -> LiveGateOutput {
        if !self.active {
            if !hop.is_voiced {
                if self.pre_roll.len() >= LIVE_PRE_ROLL_HOPS {
                    self.pre_roll.pop_front();
                }
                self.pre_roll.push_back(hop.pcm16_bytes.clone());
                return LiveGateOutput {
                    pcm16: Vec::new(),
                    diar_f32: vec![0.0; hop.f32_samples.len()],
                    is_speaking: false,
                };
            }

            self.active = true;
            self.silence_samples = 0;
            let pre_bytes: usize = self.pre_roll.iter().map(Vec::len).sum();
            let mut pcm16 = Vec::with_capacity(pre_bytes + hop.pcm16_bytes.len());
            while let Some(chunk) = self.pre_roll.pop_front() {
                pcm16.extend_from_slice(&chunk);
            }
            pcm16.extend_from_slice(&hop.pcm16_bytes);
            return LiveGateOutput {
                pcm16,
                diar_f32: hop.f32_samples.clone(),
                is_speaking: true,
            };
        }

        if hop.is_voiced {
            self.silence_samples = 0;
            return LiveGateOutput {
                pcm16: hop.pcm16_bytes.clone(),
                diar_f32: hop.f32_samples.clone(),
                is_speaking: true,
            };
        }

        self.silence_samples += hop.f32_samples.len();
        if self.silence_samples <= LIVE_HANGOVER_SAMPLES {
            LiveGateOutput {
                pcm16: hop.pcm16_bytes.clone(),
                diar_f32: hop.f32_samples.clone(),
                is_speaking: true,
            }
        } else {
            self.active = false;
            self.silence_samples = 0;
            LiveGateOutput {
                pcm16: Vec::new(),
                diar_f32: vec![0.0; hop.f32_samples.len()],
                is_speaking: false,
            }
        }
    }
}

pub fn feed_diarizer_hop(
    diar_tx: Option<&mpsc::UnboundedSender<Vec<f32>>>,
    samples: &[f32],
    is_voiced: bool,
    vad_enabled: bool,
) {
    let Some(tx) = diar_tx else {
        return;
    };
    if !vad_enabled || is_voiced {
        let _ = tx.send(samples.to_vec());
    } else {
        let _ = tx.send(vec![0.0; samples.len()]);
    }
}

pub async fn resolve_chunk_speakers(
    timeline: Arc<Mutex<SpeakerTimeline>>,
    start_sample: u64,
    end_sample: u64,
) -> Vec<u8> {
    timeline
        .lock()
        .ok()
        .map(|tl| tl.active_speakers(start_sample, end_sample))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::SpeechSegmenter;

    #[test]
    fn parses_pipeline_enums_and_legacy_diarization_fallback() {
        assert_eq!(VadModel::parse(None), VadModel::SileroV5);
        assert_eq!(VadModel::parse(Some("rms")), VadModel::Rms);
        assert_eq!(VadModel::parse(Some("silero-v5")), VadModel::SileroV5);

        assert_eq!(
            EndpointMechanism::parse(None),
            EndpointMechanism::SilenceBalanced
        );
        assert_eq!(
            EndpointMechanism::parse(Some("silence-fast")),
            EndpointMechanism::SilenceFast
        );
        assert_eq!(
            EndpointMechanism::parse(Some("fixed-window")),
            EndpointMechanism::FixedWindow
        );

        assert_eq!(
            DiarizationModel::parse(None, true),
            DiarizationModel::NemotronSortformer
        );
        assert_eq!(
            DiarizationModel::parse(None, false),
            DiarizationModel::Disabled
        );
        assert_eq!(
            DiarizationModel::parse(Some("disabled"), true),
            DiarizationModel::Disabled
        );
        assert_eq!(
            DiarizationModel::parse(Some("nemotron-sortformer"), false),
            DiarizationModel::NemotronSortformer
        );
    }

    #[test]
    fn bypass_endpointing_emits_continuous_3s_windows_even_on_zeros() {
        let cfg = PipelineConfig {
            enable_vad_endpointing: false,
            vad_model: VadModel::SileroV5,
            endpoint_mechanism: EndpointMechanism::SilenceBalanced,
            diarization_model: DiarizationModel::Disabled,
        };
        let seg_cfg = build_segmenter_config(&cfg, false);
        let mut segmenter = SpeechSegmenter::new(seg_cfg);

        // 3 seconds = 48_000 samples = 24 hops of 2048 samples (~49_152 samples)
        let silence_hop = vec![0.0_f32; VAD_HOP_SAMPLES];
        let mut emitted = 0usize;
        for _ in 0..24 {
            let out = segmenter.push(&silence_hop, Some(0.0));
            if out.final_chunk.is_some() {
                emitted += 1;
            }
        }
        assert_eq!(emitted, 1);
    }

    #[test]
    fn live_speech_gate_emits_preroll_hangover_and_suppresses_silence() {
        let mut gate = LiveSpeechGate::default();
        let make_hop = |val: i16, is_voiced: bool| -> VadHop {
            let mut pcm16_bytes = Vec::with_capacity(VAD_HOP_SAMPLES * 2);
            for _ in 0..VAD_HOP_SAMPLES {
                pcm16_bytes.extend_from_slice(&val.to_le_bytes());
            }
            VadHop {
                f32_samples: vec![0.1; VAD_HOP_SAMPLES],
                pcm16_bytes,
                speech_prob: Some(if is_voiced { 0.9 } else { 0.0 }),
                is_voiced,
            }
        };

        // Initial silence is buffered in pre-roll and suppressed for cloud upload.
        let silent1 = gate.push_hop(&make_hop(1, false));
        assert!(silent1.pcm16.is_empty());
        assert!(!silent1.is_speaking);
        assert_eq!(silent1.diar_f32.len(), VAD_HOP_SAMPLES);
        assert!(silent1.diar_f32.iter().all(|&s| s == 0.0));

        // Voiced onset flushes pre-roll + current hop.
        let voiced = gate.push_hop(&make_hop(2, true));
        assert!(voiced.is_speaking);
        assert_eq!(voiced.pcm16.len(), VAD_HOP_SAMPLES * 2 * 2);
        assert!(voiced.diar_f32.iter().all(|&s| (s - 0.1).abs() < 1e-6));

        // 7 silent hops = 14,336 samples <= 14,400 hangover -> keeps streaming.
        for _ in 0..7 {
            let hang = gate.push_hop(&make_hop(0, false));
            assert!(hang.is_speaking);
            assert_eq!(hang.pcm16.len(), VAD_HOP_SAMPLES * 2);
        }

        // 8th silent hop = 16,384 samples > 14,400 hangover -> closes gate & zeroes diar audio.
        let closed = gate.push_hop(&make_hop(0, false));
        assert!(!closed.is_speaking);
        assert!(closed.pcm16.is_empty());
        assert!(closed.diar_f32.iter().all(|&s| s == 0.0));
    }
}
