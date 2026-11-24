use futures_util::StreamExt;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type DownloadLockMap = tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>;
static DOWNLOAD_LOCKS: OnceLock<DownloadLockMap> = OnceLock::new();
static PART_COUNTER: AtomicU64 = AtomicU64::new(1);

pub const DEFAULT_REPO: &str = "ggerganov/whisper.cpp";
pub const DEFAULT_FILENAME: &str = "ggml-large-v3-q5_0.bin";

const GGML_MAGIC: u32 = 0x67676d6c;
const WHISPER_MULTILINGUAL_MIN_VOCAB: i32 = 51865;

pub const VAD_REPO: &str = "ggml-org/whisper-vad";
pub const VAD_FILENAME: &str = "ggml-silero-v5.1.2.bin";

pub const DIARIZER_FILENAME: &str = "nemotron3-diar-v3-int8.onnx";
pub const DIARIZER_URL: &str =
    "https://huggingface.co/jparedesDS/nemotron-3-diarization-onnx/resolve/main/nemotron3_diar_v3_int8.onnx";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalModelFile {
    pub filename: String,
    pub bytes: u64,
    #[serde(rename = "supportsDirectS2tt", skip_serializing_if = "Option::is_none")]
    pub supports_direct_s2tt: Option<bool>,
}

pub fn inspect_ggml_direct_s2tt(header: &[u8], filename: &str) -> Option<bool> {
    if header.len() < 48 {
        return None;
    }
    let magic = u32::from_le_bytes(header[0..4].try_into().ok()?);
    if magic != GGML_MAGIC {
        return None;
    }
    let n_vocab = i32::from_le_bytes(header[4..8].try_into().ok()?);
    let n_audio_layer = i32::from_le_bytes(header[20..24].try_into().ok()?);
    let n_text_layer = i32::from_le_bytes(header[36..40].try_into().ok()?);

    if filename.to_ascii_lowercase().contains("kotoba") {
        return Some(false);
    }

    Some(
        n_vocab >= WHISPER_MULTILINGUAL_MIN_VOCAB
            && n_audio_layer > 0
            && n_text_layer == n_audio_layer,
    )
}

pub async fn detect_local_model_direct_s2tt(
    path: &std::path::Path,
    filename: &str,
) -> Option<bool> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let mut buf = [0u8; 48];
    file.read_exact(&mut buf).await.ok()?;
    inspect_ggml_direct_s2tt(&buf, filename)
}

pub fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name != ".bin"
        && name.ends_with(".bin")
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && !name.contains('\0')
}

pub fn is_safe_repo(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let Some(owner) = parts.next() else {
        return false;
    };
    let Some(name) = parts.next() else {
        return false;
    };
    if parts.next().is_some() || owner.is_empty() || name.is_empty() {
        return false;
    }
    let valid_part = |p: &str| {
        p != "."
            && p != ".."
            && !p.contains("..")
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    valid_part(owner) && valid_part(name)
}

pub fn resolve_download_url(
    filename: &str,
    repo: Option<&str>,
    url: Option<&str>,
) -> Result<String, String> {
    if !is_safe_filename(filename) {
        return Err(format!("Invalid GGML model filename: '{filename}'"));
    }

    if let Some(raw_url) = url.map(str::trim).filter(|s| !s.is_empty()) {
        if !raw_url.starts_with("https://") {
            return Err("Custom model URL must start with https://".to_string());
        }
        return Ok(raw_url.to_string());
    }

    let clean_repo = repo.map(str::trim).filter(|s| !s.is_empty()).ok_or_else(|| {
        format!("'{filename}' is not in cache and no Hugging Face repo or URL was provided.")
    })?;

    if !is_safe_repo(clean_repo) {
        return Err(format!("Invalid Hugging Face repo '{clean_repo}' (expected owner/repo)."));
    }

    Ok(format!(
        "https://huggingface.co/{clean_repo}/resolve/main/{filename}"
    ))
}

pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("LIVE_SUBTITLES_CACHE_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join("live-subtitles");
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache").join("live-subtitles")
}

pub async fn list_local_models() -> Result<(PathBuf, Vec<LocalModelFile>), String> {
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cache directory {}: {e}", dir.display()))?;

    let mut entries = tokio::fs::read_dir(&dir)
        .await
        .map_err(|e| format!("Failed to read cache directory {}: {e}", dir.display()))?;

    let mut files = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| format!("Directory read error: {e}"))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".bin") || name.starts_with("ggml-silero-") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await
            && meta.is_file()
            && meta.len() > 0
        {
            let supports_direct_s2tt =
                detect_local_model_direct_s2tt(&entry.path(), &name).await;
            files.push(LocalModelFile {
                filename: name,
                bytes: meta.len(),
                supports_direct_s2tt,
            });
        }
    }

    files.sort_by(|a, b| a.filename.cmp(&b.filename));
    Ok((dir, files))
}

pub async fn delete_local_model(filename: &str) -> Result<(), String> {
    if !is_safe_filename(filename) || filename.starts_with("ggml-silero-") {
        return Err(format!("Refusing to delete invalid or protected file '{filename}'"));
    }
    let path = cache_dir().join(filename);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Failed to delete {}: {e}", path.display())),
    }
}

pub async fn ensure_vad_file<F>(on_progress: F) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    ensure_file_by_source(VAD_FILENAME, Some(VAD_REPO), None, on_progress).await
}

async fn lock_for_target(target: &std::path::Path) -> Arc<tokio::sync::Mutex<()>> {
    let map = DOWNLOAD_LOCKS.get_or_init(|| tokio::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().await;
    Arc::clone(
        guard
            .entry(target.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
    )
}

struct TempFileGuard(Option<PathBuf>);

impl TempFileGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn unique_part_path(target: &std::path::Path) -> PathBuf {
    let seq = PART_COUNTER.fetch_add(1, Ordering::Relaxed);
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    target.with_file_name(format!("{name}.part.{}-{seq}", std::process::id()))
}

async fn download_url_to_path<F>(
    target: &std::path::Path,
    download_url: &str,
    display_name: &str,
    on_progress: &mut F,
) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    let file_lock = lock_for_target(target).await;
    let _guard = match file_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            on_progress(
                None,
                format!("Download already in progress for {display_name}..."),
                None,
            );
            file_lock.lock().await
        }
    };

    if let Ok(meta) = tokio::fs::metadata(target).await
        && meta.len() > 0
    {
        return Ok(target.to_path_buf());
    }

    on_progress(Some(0), format!("Downloading {display_name}..."), None);

    let client = reqwest::Client::new();
    let response = client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("Failed to request {download_url}: {e}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "Failed to download {display_name}: HTTP {}",
            response.status()
        ));
    }

    let total = response.content_length();
    let part = unique_part_path(target);
    let mut file = tokio::fs::File::create(&part)
        .await
        .map_err(|e| format!("Failed to create {}: {e}", part.display()))?;
    let mut part_guard = TempFileGuard(Some(part.clone()));

    let mut downloaded: u64 = 0;
    let mut last_emit = Instant::now();
    let mut last_bytes: u64 = 0;
    let mut ema_bps: f64 = 0.0;
    let mut stream = response.bytes_stream();

    while let Some(item) = stream.next().await {
        let chunk = item.map_err(|e| format!("Download error for {display_name}: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write error for {display_name}: {e}"))?;
        downloaded += chunk.len() as u64;

        let dt = last_emit.elapsed().as_secs_f64();
        if dt >= 0.25 {
            let delta = downloaded.saturating_sub(last_bytes);
            let inst_bps = delta as f64 / dt;
            ema_bps = if ema_bps == 0.0 {
                inst_bps
            } else {
                0.35 * inst_bps + 0.65 * ema_bps
            };
            last_bytes = downloaded;
            last_emit = Instant::now();

            let pct = total.map(|t| ((downloaded * 100) / t.max(1)).min(99) as u8);
            let speed_bps = (ema_bps >= 1.0).then_some(ema_bps.round() as u64);
            on_progress(pct, format!("Downloading {display_name}"), speed_bps);
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("Flush error for {display_name}: {e}"))?;
    drop(file);

    tokio::fs::rename(&part, target)
        .await
        .map_err(|e| format!("Failed to finalize {}: {e}", target.display()))?;
    part_guard.disarm();

    Ok(target.to_path_buf())
}

pub async fn ensure_diarizer_file<F>(mut on_progress: F) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cache directory {}: {e}", dir.display()))?;

    let target = dir.join(DIARIZER_FILENAME);
    if let Ok(meta) = tokio::fs::metadata(&target).await
        && meta.len() > 0
    {
        return Ok(target);
    }

    on_progress(None, "Downloading speaker diarizer...".to_string(), None);
    download_url_to_path(&target, DIARIZER_URL, DIARIZER_FILENAME, &mut on_progress).await
}

pub async fn ensure_file_by_source<F>(
    filename: &str,
    repo: Option<&str>,
    url: Option<&str>,
    mut on_progress: F,
) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    if !is_safe_filename(filename) {
        return Err(format!("Invalid GGML model filename: '{filename}'"));
    }

    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cache directory {}: {e}", dir.display()))?;

    let target = dir.join(filename);
    if let Ok(meta) = tokio::fs::metadata(&target).await
        && meta.len() > 0
    {
        return Ok(target);
    }

    let download_url = resolve_download_url(filename, repo, url)?;
    download_url_to_path(&target, &download_url, filename, &mut on_progress).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_safe_filenames() {
        assert!(is_safe_filename("ggml-kotoba-v2.2-q8_0.bin"));
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename(".bin"));
        assert!(!is_safe_filename("../evil.bin"));
        assert!(!is_safe_filename("sub/model.bin"));
        assert!(!is_safe_filename("model.onnx"));
    }

    #[test]
    fn validates_safe_repos() {
        assert!(is_safe_repo("ggerganov/whisper.cpp"));
        assert!(is_safe_repo("Pomni/kotoba-whisper-v2.2-ggml-allquants"));
        assert!(!is_safe_repo("whisper.cpp"));
        assert!(!is_safe_repo("owner/repo/extra"));
        assert!(!is_safe_repo("https://huggingface.co/a/b"));
        assert!(!is_safe_repo("../evil"));
    }

    #[test]
    fn resolves_repo_and_custom_urls() {
        assert_eq!(
            resolve_download_url("ggml-base.bin", Some("ggerganov/whisper.cpp"), None).unwrap(),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin"
        );
        assert_eq!(
            resolve_download_url("custom.bin", None, Some("https://example.com/custom.bin"))
                .unwrap(),
            "https://example.com/custom.bin"
        );
        assert!(resolve_download_url("custom.bin", None, Some("http://insecure.com/a.bin")).is_err());
        assert!(resolve_download_url("missing.bin", None, None).is_err());
    }

    fn make_ggml_header(magic: u32, n_vocab: i32, n_audio_layer: i32, n_text_layer: i32) -> [u8; 48] {
        let mut buf = [0u8; 48];
        buf[0..4].copy_from_slice(&magic.to_le_bytes());
        buf[4..8].copy_from_slice(&n_vocab.to_le_bytes());
        buf[20..24].copy_from_slice(&n_audio_layer.to_le_bytes());
        buf[36..40].copy_from_slice(&n_text_layer.to_le_bytes());
        buf
    }

    #[test]
    fn detects_direct_s2tt_from_ggml_header() {
        let full_v3 = make_ggml_header(GGML_MAGIC, 51866, 32, 32);
        assert_eq!(inspect_ggml_direct_s2tt(&full_v3, "ggml-large-v3-q5_0.bin"), Some(true));
        assert_eq!(inspect_ggml_direct_s2tt(&full_v3, "ggml-kotoba-custom.bin"), Some(false));

        let turbo = make_ggml_header(GGML_MAGIC, 51866, 32, 4);
        assert_eq!(inspect_ggml_direct_s2tt(&turbo, "ggml-large-v3-turbo-q8_0.bin"), Some(false));

        let kotoba = make_ggml_header(GGML_MAGIC, 51866, 32, 2);
        assert_eq!(inspect_ggml_direct_s2tt(&kotoba, "ggml-kotoba-v2.2-q8_0.bin"), Some(false));

        let en_only = make_ggml_header(GGML_MAGIC, 51864, 12, 12);
        assert_eq!(inspect_ggml_direct_s2tt(&en_only, "ggml-small.en.bin"), Some(false));

        let bad_magic = make_ggml_header(0x12345678, 51866, 32, 32);
        assert_eq!(inspect_ggml_direct_s2tt(&bad_magic, "ggml-large-v3.bin"), None);
        assert_eq!(inspect_ggml_direct_s2tt(&full_v3[..16], "ggml-large-v3.bin"), None);
    }

    #[test]
    fn generates_distinct_non_bin_part_paths() {
        let target = PathBuf::from("/tmp/ggml-large-v3.bin");
        let p1 = unique_part_path(&target);
        let p2 = unique_part_path(&target);
        assert_ne!(p1, p2);
        assert_ne!(p1, target);
        assert!(!p1.to_string_lossy().ends_with(".bin"));
    }

    #[tokio::test]
    async fn download_skips_network_when_target_already_exists() {
        let path = std::env::temp_dir().join(format!(
            "live-subtitles-existing-test-{}.bin",
            std::process::id()
        ));
        tokio::fs::write(&path, b"ggml-existing").await.unwrap();

        let mut progress_calls = 0usize;
        let res = download_url_to_path(
            &path,
            "https://127.0.0.1:0/should-not-request.bin",
            "existing.bin",
            &mut |_pct, _label, _bps| {
                progress_calls += 1;
            },
        )
        .await;

        let _ = tokio::fs::remove_file(&path).await;
        assert_eq!(res.unwrap(), path);
        assert_eq!(progress_calls, 0);
    }

    #[test]
    fn temp_file_guard_removes_on_drop_unless_disarmed() {
        let pid = std::process::id();
        let drop_path = std::env::temp_dir().join(format!("live-subtitles-guard-drop-{pid}.part"));
        let keep_path = std::env::temp_dir().join(format!("live-subtitles-guard-keep-{pid}.part"));

        std::fs::write(&drop_path, b"tmp").unwrap();
        {
            let _guard = TempFileGuard(Some(drop_path.clone()));
        }
        assert!(!drop_path.exists());

        std::fs::write(&keep_path, b"keep").unwrap();
        {
            let mut guard = TempFileGuard(Some(keep_path.clone()));
            guard.disarm();
        }
        assert!(keep_path.exists());
        let _ = std::fs::remove_file(&keep_path);
    }
}
