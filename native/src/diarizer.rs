use parakeet_rs::ExecutionConfig;
use parakeet_rs::sortformer::{DiarizationConfig, Sortformer, SpeakerSegment, StreamingProfile};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

const MAX_SPEAKERS: usize = 8;
const HISTORY_PRUNE_SAMPLES: u64 = 60 * 16_000;
const FALLBACK_LOOKBACK_SAMPLES: u64 = 16_000;
const MIN_SECONDARY_OVERLAP_SAMPLES: u64 = 4_800; // 0.30s minimum overlap for secondary tag
const MAX_SPEAKER_TAGS: usize = 3;

static ORT_ENV: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

fn ort_environment() -> Result<&'static ort::environment::Environment, String> {
    let res = ORT_ENV.get_or_init(|| {
        ort::init()
            .with_name("live-subtitles")
            .with_telemetry(false)
            .build()
            .map_err(|e| format!("Failed to initialize ONNX Runtime: {e}"))
    });
    match res {
        Ok(env) => Ok(env),
        Err(err) => Err(err.clone()),
    }
}

pub struct NemotronDiarizer {
    inner: Mutex<Sortformer>,
}

impl NemotronDiarizer {
    pub fn load(path: &Path) -> Result<Self, String> {
        let env = ort_environment()?;
        let config = ExecutionConfig::new()
            .with_intra_threads(4)
            .with_environment(env.clone());

        let mut diarizer = Sortformer::with_config(path, Some(config), DiarizationConfig::default())
            .map_err(|e| format!("Failed to load Nemotron diarizer {}: {e}", path.display()))?;

        // 0.64s buffer (480ms stride + 160ms right context) aligns closely with VAD finalization + Whisper latency.
        diarizer
            .set_profile(StreamingProfile::very_low_latency())
            .map_err(|e| format!("Failed to configure Nemotron streaming profile: {e}"))?;

        Ok(Self {
            inner: Mutex::new(diarizer),
        })
    }

    pub fn reset(&self) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.reset_state();
        }
    }

    pub fn feed(&self, samples: &[f32]) -> Result<Vec<SpeakerSegment>, String> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| "Nemotron diarizer lock poisoned".to_string())?;
        guard
            .feed(samples)
            .map_err(|e| format!("Nemotron diarization step failed: {e}"))
    }
}

#[derive(Debug, Clone, Copy)]
struct TimelineSegment {
    start: u64,
    end: u64,
    speaker_id: usize,
}

/// Rolling sample-indexed timeline of Nemotron speaker segments for aligning Whisper chunks.
///
/// shortcut: overlapping voices within one Whisper VAD chunk are tagged with all active speakers
/// (`[S1][S2]`) on that chunk's single transcript rather than splitting into dual ASR streams.
pub struct SpeakerTimeline {
    segments: VecDeque<TimelineSegment>,
    base_offset: u64,
    debug: bool,
}

impl Default for SpeakerTimeline {
    fn default() -> Self {
        Self {
            segments: VecDeque::new(),
            base_offset: 0,
            debug: std::env::var_os("LIVE_SUBTITLES_DEBUG_DIAR").is_some(),
        }
    }
}

impl SpeakerTimeline {
    pub fn reset(&mut self, base_offset: u64) {
        self.segments.clear();
        self.base_offset = base_offset;
    }

    pub fn push_segments(&mut self, segs: &[SpeakerSegment]) {
        if segs.is_empty() {
            return;
        }
        let mut latest_end = 0_u64;
        for seg in segs {
            if seg.speaker_id >= MAX_SPEAKERS || seg.end <= seg.start {
                continue;
            }
            let start = self.base_offset.saturating_add(seg.start);
            let end = self.base_offset.saturating_add(seg.end);
            latest_end = latest_end.max(end);
            if self.debug {
                eprintln!(
                    "[nemotron] S{} {:.2}s - {:.2}s",
                    seg.speaker_id + 1,
                    start as f64 / 16_000.0,
                    end as f64 / 16_000.0
                );
            }
            self.segments.push_back(TimelineSegment {
                start,
                end,
                speaker_id: seg.speaker_id,
            });
        }

        let cutoff = latest_end.saturating_sub(HISTORY_PRUNE_SAMPLES);
        while let Some(front) = self.segments.front() {
            if front.end < cutoff {
                self.segments.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn active_speakers(&self, start_sample: u64, end_sample: u64) -> Vec<u8> {
        if self.segments.is_empty() || end_sample <= start_sample {
            return Vec::new();
        }

        let mut overlaps = [0_u64; MAX_SPEAKERS];
        for seg in &self.segments {
            let ov_start = seg.start.max(start_sample);
            let ov_end = seg.end.min(end_sample);
            if ov_end > ov_start {
                overlaps[seg.speaker_id] = overlaps[seg.speaker_id].saturating_add(ov_end - ov_start);
            }
        }

        let max_ov = overlaps.iter().copied().max().unwrap_or(0);
        if max_ov > 0 {
            let mut active: Vec<(usize, u64)> = overlaps
                .iter()
                .enumerate()
                .filter_map(|(idx, &ov)| {
                    if ov == 0 {
                        return None;
                    }
                    let keep = ov == max_ov
                        || (ov >= MIN_SECONDARY_OVERLAP_SAMPLES && ov * 100 >= max_ov * 35);
                    keep.then_some((idx, ov))
                })
                .collect();
            active.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            active.truncate(MAX_SPEAKER_TAGS);
            return active.into_iter().map(|(idx, _)| (idx + 1) as u8).collect();
        }

        // Near the streaming frontier, a short interim chunk may still sit in lookahead latency;
        // fall back to the latest segment ending within 1.0s of `end_sample`.
        let min_end = end_sample.saturating_sub(FALLBACK_LOOKBACK_SAMPLES);
        let mut recent: Option<&TimelineSegment> = None;

        for seg in self.segments.iter().rev() {
            if seg.start > end_sample || seg.end < min_end {
                continue;
            }
            if recent.is_none_or(|cur| seg.end > cur.end) {
                recent = Some(seg);
            }
        }

        recent
            .map(|seg| vec![(seg.speaker_id + 1) as u8])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, end: u64, speaker_id: usize) -> SpeakerSegment {
        SpeakerSegment {
            start,
            end,
            speaker_id,
        }
    }

    #[test]
    fn detects_active_and_overlapping_speakers_with_fallback_and_pruning() {
        let mut tl = SpeakerTimeline::default();
        assert!(tl.active_speakers(0, 16_000).is_empty());

        // Mid-stream reset at 10.0s
        tl.reset(160_000);
        tl.push_segments(&[
            seg(0, 8_000, 0),       // 10.0s - 10.5s S1
            seg(8_000, 24_000, 1),  // 10.5s - 11.5s S2
            seg(24_000, 38_000, 0), // 11.5s - 12.375s S1
            seg(30_000, 46_000, 1), // 11.875s - 12.875s S2 (0.5s overlap with S1)
            seg(46_000, 48_000, 2), // 12.875s - 13.0s S3 (0.125s brief edge bleed)
        ]);

        // Brief 0.25s vs 0.75s -> S1 is < 0.30s secondary threshold so only dominant S2 returned
        assert_eq!(tl.active_speakers(164_000, 180_000), vec![2]);

        // Simultaneous overlap: S1 = 0.75s, S2 = 0.375s (50% >= 35%) -> [1, 2]
        assert_eq!(tl.active_speakers(184_000, 196_000), vec![1, 2]);

        // Reverse dominance ordering: S2 = 1.0s, S1 = 0.5s (50% >= 35%), S3 = 0.125s (< 0.30s excluded) -> [2, 1]
        assert_eq!(tl.active_speakers(190_000, 208_000), vec![2, 1]);

        // Short interim slightly ahead (within 1.0s of last segment) falls back cleanly
        assert_eq!(tl.active_speakers(210_000, 214_000), vec![3]);
        assert!(tl.active_speakers(230_000, 240_000).is_empty());

        // Segments older than 60s from latest end are pruned
        tl.push_segments(&[seg(1_120_000, 1_136_000, 0)]);
        assert!(tl.active_speakers(164_000, 180_000).is_empty());
    }
}
