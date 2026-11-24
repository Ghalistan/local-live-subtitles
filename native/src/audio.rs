use std::collections::VecDeque;

pub const TARGET_SAMPLE_RATE: usize = 16_000;

pub fn pcm_i16_le_to_f32(bytes: &[u8]) -> Vec<f32> {
    let count = bytes.len() / 2;
    let mut out = Vec::with_capacity(count);
    for chunk in bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        out.push(sample as f32 / 32768.0);
    }
    out
}

pub fn compute_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

#[derive(Debug, Clone, Copy)]
pub struct SegmenterConfig {
    pub sample_rate: usize,
    pub rms_threshold: f32,
    pub speech_threshold: f32,
    pub silence_threshold: f32,
    pub pre_roll_ms: usize,
    pub min_speech_ms: usize,
    pub silence_ms: usize,
    pub max_chunk_ms: usize,
    pub overlap_ms: usize,
    pub interim_interval_ms: usize,
}

impl Default for SegmenterConfig {
    fn default() -> Self {
        Self {
            sample_rate: TARGET_SAMPLE_RATE,
            rms_threshold: 0.003,
            speech_threshold: 0.5,
            silence_threshold: 0.35,
            pre_roll_ms: 200,
            min_speech_ms: 350,
            silence_ms: 380,
            max_chunk_ms: 4500,
            overlap_ms: 400,
            interim_interval_ms: 750,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SpeechChunk {
    pub samples: Vec<f32>,
    pub start_sample: u64,
    pub end_sample: u64,
}

#[derive(Debug, Default)]
pub struct SegmenterOutput {
    pub final_chunk: Option<SpeechChunk>,
    pub interim_chunk: Option<SpeechChunk>,
}

pub struct SpeechSegmenter {
    rms_threshold: f32,
    speech_threshold: f32,
    silence_threshold: f32,
    pre_roll_max_samples: usize,
    min_speech_samples: usize,
    silence_limit_samples: usize,
    max_chunk_samples: usize,
    overlap_samples: usize,
    interim_interval_samples: usize,

    pre_roll: VecDeque<f32>,
    speech_samples: Vec<f32>,
    stream_pos: u64,
    voiced_start_sample: u64,
    voiced_end_sample: u64,
    voiced_samples: usize,
    silence_samples: usize,
    samples_since_interim: usize,
    in_speech: bool,
}

impl SpeechSegmenter {
    pub fn new(config: SegmenterConfig) -> Self {
        let ms_to_samples = |ms: usize| -> usize { (ms * config.sample_rate) / 1000 };
        Self {
            rms_threshold: config.rms_threshold,
            speech_threshold: config.speech_threshold,
            silence_threshold: config.silence_threshold,
            pre_roll_max_samples: ms_to_samples(config.pre_roll_ms),
            min_speech_samples: ms_to_samples(config.min_speech_ms),
            silence_limit_samples: ms_to_samples(config.silence_ms),
            max_chunk_samples: ms_to_samples(config.max_chunk_ms),
            overlap_samples: ms_to_samples(config.overlap_ms),
            interim_interval_samples: ms_to_samples(config.interim_interval_ms),
            pre_roll: VecDeque::with_capacity(ms_to_samples(config.pre_roll_ms) + 512),
            speech_samples: Vec::new(),
            stream_pos: 0,
            voiced_start_sample: 0,
            voiced_end_sample: 0,
            voiced_samples: 0,
            silence_samples: 0,
            samples_since_interim: 0,
            in_speech: false,
        }
    }

    pub fn is_speaking(&self) -> bool {
        self.in_speech
    }

    pub fn stream_pos(&self) -> u64 {
        self.stream_pos
    }

    pub fn reset_stream(&mut self) {
        self.stream_pos = 0;
        self.reset();
    }

    pub fn push(&mut self, samples: &[f32], speech_prob: Option<f32>) -> SegmenterOutput {
        if samples.is_empty() {
            return SegmenterOutput::default();
        }

        let hop_start = self.stream_pos;
        let hop_end = hop_start + samples.len() as u64;
        self.stream_pos = hop_end;

        let is_voiced = match speech_prob {
            Some(prob) => {
                if self.in_speech {
                    prob >= self.silence_threshold
                } else {
                    prob >= self.speech_threshold
                }
            }
            None => compute_rms(samples) >= self.rms_threshold,
        };

        if !self.in_speech {
            if !is_voiced {
                self.push_pre_roll(samples);
                return SegmenterOutput::default();
            }

            self.in_speech = true;
            self.speech_samples.clear();
            self.speech_samples.extend(self.pre_roll.drain(..));
            self.speech_samples.extend_from_slice(samples);
            self.voiced_start_sample = hop_start;
            self.voiced_end_sample = hop_end;
            self.voiced_samples = samples.len();
            self.silence_samples = 0;
            self.samples_since_interim = samples.len();
            return SegmenterOutput::default();
        }

        self.speech_samples.extend_from_slice(samples);
        self.samples_since_interim += samples.len();

        if is_voiced {
            self.voiced_end_sample = hop_end;
            self.voiced_samples += samples.len();
            self.silence_samples = 0;
        } else {
            self.silence_samples += samples.len();
        }

        if self.silence_samples >= self.silence_limit_samples {
            let final_chunk = if self.voiced_samples >= self.min_speech_samples {
                Some(SpeechChunk {
                    samples: std::mem::take(&mut self.speech_samples),
                    start_sample: self.voiced_start_sample,
                    end_sample: self.voiced_end_sample,
                })
            } else {
                None
            };
            self.reset();
            return SegmenterOutput {
                final_chunk,
                interim_chunk: None,
            };
        }

        if self.speech_samples.len() >= self.max_chunk_samples {
            let final_samples = std::mem::take(&mut self.speech_samples);
            let start_sample = self.voiced_start_sample;
            let end_sample = self.voiced_end_sample;
            let keep = self.overlap_samples.min(final_samples.len());
            if keep > 0 {
                let tail = &final_samples[final_samples.len() - keep..];
                self.speech_samples.extend_from_slice(tail);
                self.voiced_samples = keep;
            } else {
                self.voiced_samples = 0;
            }
            self.voiced_start_sample = hop_end.saturating_sub(keep as u64);
            self.voiced_end_sample = hop_end;
            self.silence_samples = 0;
            self.samples_since_interim = 0;
            return SegmenterOutput {
                final_chunk: Some(SpeechChunk {
                    samples: final_samples,
                    start_sample,
                    end_sample,
                }),
                interim_chunk: None,
            };
        }

        if self.interim_interval_samples > 0
            && self.samples_since_interim >= self.interim_interval_samples
            && self.voiced_samples >= self.min_speech_samples
        {
            self.samples_since_interim = 0;
            return SegmenterOutput {
                final_chunk: None,
                interim_chunk: Some(SpeechChunk {
                    samples: self.speech_samples.clone(),
                    start_sample: self.voiced_start_sample,
                    end_sample: self.voiced_end_sample,
                }),
            };
        }

        SegmenterOutput::default()
    }

    #[allow(dead_code)]
    pub fn flush(&mut self) -> Option<SpeechChunk> {
        let out = if self.in_speech && self.voiced_samples >= self.min_speech_samples {
            Some(SpeechChunk {
                samples: std::mem::take(&mut self.speech_samples),
                start_sample: self.voiced_start_sample,
                end_sample: self.voiced_end_sample,
            })
        } else {
            None
        };
        self.reset();
        out
    }

    pub fn reset(&mut self) {
        self.pre_roll.clear();
        self.speech_samples.clear();
        self.voiced_start_sample = 0;
        self.voiced_end_sample = 0;
        self.voiced_samples = 0;
        self.silence_samples = 0;
        self.samples_since_interim = 0;
        self.in_speech = false;
    }

    fn push_pre_roll(&mut self, samples: &[f32]) {
        if self.pre_roll_max_samples == 0 {
            return;
        }
        self.pre_roll.extend(samples.iter().copied());
        while self.pre_roll.len() > self.pre_roll_max_samples {
            self.pre_roll.pop_front();
        }
    }
}

pub fn clean_transcript(raw: &str) -> String {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return String::new();
    }

    let lower = collapsed.to_lowercase();
    if matches!(
        lower.as_str(),
        "♪" | "[音楽]" | "(音楽)" | "[music]" | "(music)" | "[拍手]" | "(拍手)"
    ) {
        return String::new();
    }

    let is_punct_only = collapsed.chars().all(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '♪' | '♫' | '。' | '．' | '.' | '!' | '！' | '?' | '？' | '・' | '、' | ',' | '[' | ']' | '(' | ')' | '（' | '）'
            )
    });
    if is_punct_only {
        return String::new();
    }

    let normalized = lower.trim_end_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '。' | '．' | '.' | '!' | '！' | '?' | '？' | '♪' | '〜' | '~' | '・' | '、' | ','
            )
    });
    if matches!(
        normalized,
        "ご視聴ありがとうございました"
            | "ご視聴ありがとうございます"
            | "チャンネル登録お願いします"
            | "チャンネル登録よろしくお願いします"
            | "高評価お願いします"
            | "字幕作成"
            | "おやすみなさい"
            | "thank you for watching"
            | "thanks for watching"
            | "subtitles by"
    ) {
        return String::new();
    }

    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(samples: usize, amplitude: f32) -> Vec<f32> {
        vec![amplitude; samples]
    }

    #[test]
    fn converts_pcm_i16_le_correctly() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0_i16.to_le_bytes());
        bytes.extend_from_slice(&16384_i16.to_le_bytes());
        bytes.extend_from_slice(&(-32768_i16).to_le_bytes());
        let out = pcm_i16_le_to_f32(&bytes);
        assert_eq!(out.len(), 3);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 0.5).abs() < 1e-5);
        assert!((out[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn ignores_pure_silence() {
        let mut seg = SpeechSegmenter::new(SegmenterConfig::default());
        for _ in 0..20 {
            let out = seg.push(&make_frame(1600, 0.001), None);
            assert!(out.final_chunk.is_none());
            assert!(out.interim_chunk.is_none());
        }
        assert!(seg.flush().is_none());
    }

    #[test]
    fn emits_final_after_speech_and_pause() {
        let mut seg = SpeechSegmenter::new(SegmenterConfig {
            min_speech_ms: 400,
            silence_ms: 300,
            interim_interval_ms: 5000,
            ..Default::default()
        });
        // Feed initial silence for pre-roll
        assert!(seg.push(&make_frame(3200, 0.0), None).final_chunk.is_none());
        for _ in 0..5 {
            assert!(seg.push(&make_frame(1600, 0.05), None).final_chunk.is_none());
        }
        assert!(seg.push(&make_frame(3200, 0.0), None).final_chunk.is_none());
        let out = seg.push(&make_frame(1600, 0.0), None);
        let chunk = out.final_chunk.unwrap();
        assert!(chunk.samples.len() >= 8000);
        assert_eq!(chunk.start_sample, 3200);
        assert_eq!(chunk.end_sample, 11200);
        assert!(chunk.start_sample < chunk.end_sample);
        assert_eq!(seg.stream_pos(), 16000);

        // Next utterance advances stream_pos monotonically
        assert!(seg.push(&make_frame(1600, 0.05), None).final_chunk.is_none());
        assert_eq!(seg.stream_pos(), 17600);
    }

    #[test]
    fn uses_silero_hysteresis_and_emits_interim_then_overlap() {
        let mut seg = SpeechSegmenter::new(SegmenterConfig {
            min_speech_ms: 300,
            interim_interval_ms: 500,
            max_chunk_ms: 1000,
            overlap_ms: 200,
            silence_ms: 300,
            ..Default::default()
        });

        // Loud BGM with low probability is ignored
        for _ in 0..5 {
            assert!(seg.push(&make_frame(1600, 0.2), Some(0.15)).final_chunk.is_none());
        }
        assert!(!seg.is_speaking());

        let mut saw_interim = false;
        let mut saw_max_final = false;
        for _ in 0..12 {
            let out = seg.push(&make_frame(1600, 0.05), Some(0.7));
            if let Some(interim) = out.interim_chunk {
                saw_interim = true;
                assert!(interim.start_sample < interim.end_sample);
            }
            if let Some(chunk) = out.final_chunk {
                saw_max_final = true;
                assert_eq!(chunk.samples.len(), 16000);
                assert!(chunk.start_sample < chunk.end_sample);
            }
        }
        assert!(saw_interim);
        assert!(saw_max_final);
        // 200ms pre-roll + 8x100ms frames hit 1000ms max_chunk; 4 remaining frames + 200ms overlap = 9600
        let flushed = seg.flush().unwrap();
        assert_eq!(flushed.samples.len(), 9600);
        assert!(flushed.start_sample < flushed.end_sample);
    }

    #[test]
    fn resets_is_speaking_when_short_burst_ends_in_silence() {
        let mut seg = SpeechSegmenter::new(SegmenterConfig {
            min_speech_ms: 400,
            silence_ms: 200,
            ..Default::default()
        });
        assert!(!seg.is_speaking());
        seg.push(&make_frame(1600, 0.05), Some(0.8));
        assert!(seg.is_speaking());
        assert!(seg.push(&make_frame(1600, 0.001), Some(0.1)).final_chunk.is_none());
        assert!(seg.is_speaking());
        assert!(seg.push(&make_frame(1600, 0.001), Some(0.1)).final_chunk.is_none());
        assert!(!seg.is_speaking());
    }

    #[test]
    fn filters_hallucinations_and_keeps_dialogue() {
        assert_eq!(clean_transcript("   "), "");
        assert_eq!(clean_transcript("[音楽]"), "");
        assert_eq!(clean_transcript("ご視聴ありがとうございました。"), "");
        assert_eq!(clean_transcript("Thank you for watching."), "");
        assert_eq!(
            clean_transcript("  今日   めっちゃ楽しいんだけど！  "),
            "今日 めっちゃ楽しいんだけど！"
        );
    }
}
