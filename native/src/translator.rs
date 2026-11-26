use crate::gemini::language_name;
use crate::models::{HY_MT3_FILENAME, HY_MT3_REPO, cache_dir, download_url_to_path};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USER_AGENT: &str = "live-subtitles-native/0.1";
const LLAMA_RELEASES_URL: &str =
    "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=10";

fn is_shard_filename(lower: &str) -> bool {
    // Multi-part GGUFs typically end with `-00001-of-00002.gguf`.
    let Some(stem) = lower.strip_suffix(".gguf") else {
        return false;
    };
    let Some((_prefix, total)) = stem.rsplit_once("-of-") else {
        return false;
    };
    total.len() == 5 && total.chars().all(|c| c.is_ascii_digit())
}

pub fn select_hy_mt3_gguf_filename(rfilenames: &[&str]) -> Option<String> {
    let candidates: Vec<&str> = rfilenames
        .iter()
        .copied()
        .filter(|name| {
            let lower = name.to_ascii_lowercase();
            lower.ends_with(".gguf") && !lower.contains("mmproj")
        })
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let single_files: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|name| !is_shard_filename(&name.to_ascii_lowercase()))
        .collect();
    let pool = if single_files.is_empty() {
        &candidates
    } else {
        &single_files
    };

    for quant in ["q4_k_m", "q5_k_m", "q8_0"] {
        if let Some(found) = pool
            .iter()
            .find(|name| name.to_ascii_lowercase().contains(quant))
        {
            return Some((*found).to_string());
        }
    }

    pool.first().map(|s| (*s).to_string())
}

fn is_supported_archive_name(lower: &str) -> bool {
    lower.ends_with(".tar.gz")
        || lower.ends_with(".tgz")
        || lower.ends_with(".tar.xz")
        || lower.ends_with(".zip")
}

pub fn select_llama_release_asset<'a>(
    assets: &'a [(String, String)],
    os: &str,
    arch: &str,
) -> Option<&'a (String, String)> {
    let find_matching = |needles: &[&str], excludes: &[&str]| -> Option<&'a (String, String)> {
        assets.iter().find(|(name, _url)| {
            let lower = name.to_ascii_lowercase();
            is_supported_archive_name(&lower)
                && needles.iter().all(|needle| lower.contains(needle))
                && excludes.iter().all(|ex| !lower.contains(ex))
        })
    };

    match (os, arch) {
        ("linux", "x86_64") => find_matching(&["ubuntu", "vulkan", "x64"], &["cuda", "rocm", "sycl", "opencl"])
            .or_else(|| find_matching(&["linux", "vulkan", "x64"], &["cuda", "rocm", "sycl", "opencl"]))
            .or_else(|| find_matching(&["ubuntu", "x64"], &["vulkan", "cuda", "rocm", "sycl", "opencl", "kompute", "rpc"])),
        ("linux", "aarch64") => find_matching(&["ubuntu", "vulkan", "arm64"], &["cuda", "rocm", "sycl", "opencl"])
            .or_else(|| find_matching(&["ubuntu", "arm64"], &["vulkan", "cuda", "rocm", "sycl", "opencl", "kompute", "rpc"])),
        ("macos", "aarch64") => find_matching(&["macos", "arm64"], &[]),
        ("macos", "x86_64") => find_matching(&["macos", "x64"], &[]),
        ("windows", "x86_64") => find_matching(&["win", "vulkan", "x64"], &["cuda", "rocm", "sycl", "opencl"])
            .or_else(|| find_matching(&["win", "cpu", "x64"], &["cuda", "rocm", "sycl", "opencl"])),
        _ => None,
    }
}

fn extract_release_assets(release: &Value) -> Vec<(String, String)> {
    release
        .get("assets")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let name = item.get("name")?.as_str()?.to_string();
                    let url = item.get("browser_download_url")?.as_str()?.to_string();
                    Some((name, url))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn select_llama_asset_from_releases(
    releases_json: &Value,
    os: &str,
    arch: &str,
) -> Option<(String, String)> {
    let releases: Vec<&Value> = match releases_json.as_array() {
        Some(arr) => arr.iter().collect(),
        None => vec![releases_json],
    };

    for release in releases {
        if release.get("draft").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let assets = extract_release_assets(release);
        if let Some(found) = select_llama_release_asset(&assets, os, arch) {
            return Some(found.clone());
        }
    }
    None
}

pub fn build_hy_mt3_prompt(text: &str, target_lang: &str, context_lines: &[String]) -> String {
    let target_name = language_name(target_lang);
    let recent: Vec<&str> = context_lines
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();

    if recent.is_empty() {
        format!(
            "Translate the following text into {target_name}. Note that you should only output the translated result without any additional explanation:\n\n{text}"
        )
    } else {
        let tail_start = recent.len().saturating_sub(3);
        let ctx = recent[tail_start..].join("\n");
        format!(
            "{ctx}\nUsing the information above, translate the following text into {target_name}. Do not translate the preceding context, and do not include any additional explanations:\n{text}"
        )
    }
}

pub fn clean_hy_mt3_output(raw: &str) -> String {
    let collapsed = raw
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut s = collapsed.trim();
    loop {
        let stripped = if (s.starts_with('"') && s.ends_with('"'))
            || (s.starts_with('\'') && s.ends_with('\''))
            || (s.starts_with('「') && s.ends_with('」'))
            || (s.starts_with('“') && s.ends_with('”'))
        {
            let mut chars = s.chars();
            chars.next();
            chars.next_back();
            Some(chars.as_str().trim())
        } else {
            None
        };
        match stripped {
            Some(inner) if !inner.is_empty() && inner != s => s = inner,
            _ => break,
        }
    }
    s.to_string()
}

fn llama_binary_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

fn find_llama_binary_recursive(dir: &Path) -> Option<PathBuf> {
    let target = llama_binary_name();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some(target) {
                return Some(path);
            }
        }
    }
    None
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn ensure_executable_permissions_recursive(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_dir() {
                stack.push(path);
            } else if let Ok(meta) = entry.metadata() {
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if fname.starts_with("llama-") || fname.contains(".so") || fname.ends_with(".dylib") {
                    let mut perms = meta.permissions();
                    perms.set_mode(perms.mode() | 0o755);
                    let _ = std::fs::set_permissions(&path, perms);
                }
            }
        }
    }
}

#[cfg(not(unix))]
fn ensure_executable_permissions_recursive(_dir: &Path) {}

pub fn has_llama_server_binary() -> bool {
    if let Ok(env_path) = std::env::var("LIVE_SUBTITLES_LLAMA_SERVER") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() && Path::new(trimmed).is_file() {
            return true;
        }
    }

    let install_dir = cache_dir().join("llama-server");
    if install_dir.is_dir() && find_llama_binary_recursive(&install_dir).is_some() {
        return true;
    }

    find_in_path(llama_binary_name()).is_some()
}

pub async fn ensure_llama_server_binary<F>(on_progress: &mut F) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    if let Ok(env_path) = std::env::var("LIVE_SUBTITLES_LLAMA_SERVER") {
        let trimmed = env_path.trim();
        if !trimmed.is_empty() {
            let p = PathBuf::from(trimmed);
            if p.is_file() {
                return Ok(p);
            }
            return Err(format!(
                "LIVE_SUBTITLES_LLAMA_SERVER path does not exist: {}",
                p.display()
            ));
        }
    }

    let install_dir = cache_dir().join("llama-server");
    if install_dir.is_dir()
        && let Some(existing) = find_llama_binary_recursive(&install_dir)
    {
        ensure_executable_permissions_recursive(&install_dir);
        return Ok(existing);
    }

    if let Some(in_path) = find_in_path(llama_binary_name()) {
        return Ok(in_path);
    }

    on_progress(None, "Resolving llama-server release...".to_string(), None);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let resp = client
        .get(LLAMA_RELEASES_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("Failed to query llama.cpp releases: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read llama.cpp release response: {e}"))?;
    if !status.is_success() {
        return Err(format!("GitHub release lookup failed: HTTP {status}"));
    }

    let payload: Value = serde_json::from_str(&body)
        .map_err(|e| format!("Invalid llama.cpp release JSON: {e}"))?;

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let (asset_name, asset_url) = match select_llama_asset_from_releases(&payload, os, arch) {
        Some(found) => found,
        None => {
            let sample_assets: Vec<String> = payload
                .as_array()
                .and_then(|arr| {
                    arr.iter()
                        .find(|r| {
                            r.get("assets")
                                .and_then(Value::as_array)
                                .is_some_and(|a| !a.is_empty())
                        })
                        .map(extract_release_assets)
                })
                .unwrap_or_default()
                .into_iter()
                .take(6)
                .map(|(n, _)| n)
                .collect();
            return Err(format!(
                "No prebuilt llama-server release asset found for {os}/{arch} (sample assets: {})",
                if sample_assets.is_empty() {
                    "none".to_string()
                } else {
                    sample_assets.join(", ")
                }
            ));
        }
    };

    let base_dir = cache_dir();
    tokio::fs::create_dir_all(&base_dir)
        .await
        .map_err(|e| format!("Failed to create cache dir {}: {e}", base_dir.display()))?;

    let archive_tmp = base_dir.join(format!("llama-archive-{}.tmp", std::process::id()));
    let _ = tokio::fs::remove_file(&archive_tmp).await;
    download_url_to_path(&archive_tmp, &asset_url, &asset_name, on_progress).await?;

    on_progress(None, format!("Extracting {asset_name}..."), None);

    let staging_dir = base_dir.join(format!("llama-server-staging-{}", std::process::id()));
    let _ = tokio::fs::remove_dir_all(&staging_dir).await;
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .map_err(|e| format!("Failed to create staging dir: {e}"))?;

    let archive_clone = archive_tmp.clone();
    let staging_clone = staging_dir.clone();
    let asset_label = asset_name.clone();
    let is_zip = asset_name.to_ascii_lowercase().ends_with(".zip");
    let extract_res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let output = if is_zip && cfg!(windows) {
            Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "Expand-Archive -Path '{}' -DestinationPath '{}' -Force",
                        archive_clone.display(),
                        staging_clone.display()
                    ),
                ])
                .output()
        } else if is_zip {
            match Command::new("unzip")
                .arg("-o")
                .arg(&archive_clone)
                .arg("-d")
                .arg(&staging_clone)
                .output()
            {
                Ok(out) => Ok(out),
                Err(_) => Command::new("tar")
                    .arg("-xf")
                    .arg(&archive_clone)
                    .arg("-C")
                    .arg(&staging_clone)
                    .output(),
            }
        } else {
            Command::new("tar")
                .arg("-xf")
                .arg(&archive_clone)
                .arg("-C")
                .arg(&staging_clone)
                .output()
        };
        let output = output.map_err(|e| format!("Failed to extract {asset_label}: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("Extraction failed for {asset_label}: {}", stderr.trim()));
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Extraction task panicked: {e}"))?;

    let _ = tokio::fs::remove_file(&archive_tmp).await;
    if let Err(err) = extract_res {
        let _ = tokio::fs::remove_dir_all(&staging_dir).await;
        return Err(err);
    }

    if install_dir.exists() {
        let _ = tokio::fs::remove_dir_all(&install_dir).await;
    }
    tokio::fs::rename(&staging_dir, &install_dir)
        .await
        .map_err(|e| format!("Failed to install llama-server into {}: {e}", install_dir.display()))?;

    ensure_executable_permissions_recursive(&install_dir);
    find_llama_binary_recursive(&install_dir).ok_or_else(|| {
        format!(
            "Extracted archive {asset_name} did not contain {}",
            llama_binary_name()
        )
    })
}

pub async fn ensure_hy_mt3_model<F>(on_progress: &mut F) -> Result<PathBuf, String>
where
    F: FnMut(Option<u8>, String, Option<u64>) + Send,
{
    let dir = cache_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cache dir {}: {e}", dir.display()))?;

    let target = dir.join(HY_MT3_FILENAME);
    if let Ok(meta) = tokio::fs::metadata(&target).await
        && meta.is_file()
        && meta.len() > 0
    {
        return Ok(target);
    }

    on_progress(None, "Resolving Hy-MT3-1.8B GGUF...".to_string(), None);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_default();
    let api_url = format!("https://huggingface.co/api/models/{HY_MT3_REPO}");
    let resp = client
        .get(&api_url)
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(|e| format!("Failed to query Hugging Face for {HY_MT3_REPO}: {e}"))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read Hugging Face response: {e}"))?;
    if !status.is_success() {
        return Err(format!("Hugging Face lookup failed: HTTP {status}"));
    }

    let payload: Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid Hugging Face JSON: {e}"))?;
    let sibling_names: Vec<String> = payload
        .get("siblings")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("rfilename").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<&str> = sibling_names.iter().map(String::as_str).collect();
    let rfilename = select_hy_mt3_gguf_filename(&refs)
        .ok_or_else(|| format!("No suitable .gguf file found in {HY_MT3_REPO}"))?;

    let download_url = format!("https://huggingface.co/{HY_MT3_REPO}/resolve/main/{rfilename}");
    download_url_to_path(&target, &download_url, "Hy-MT3-1.8B", on_progress).await
}

pub struct HyMt3Server {
    port: u16,
    child: Mutex<Child>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    client: reqwest::Client,
}

impl HyMt3Server {
    fn pick_free_port() -> Result<u16, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Failed to allocate local port for Hy-MT3: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("Failed to inspect local port: {e}"))?
            .port();
        Ok(port)
    }

    fn spawn_child(exe_path: &Path, model_path: &Path, port: u16, ngl: &str) -> Result<(Child, Arc<Mutex<VecDeque<String>>>), String> {
        let mut cmd = Command::new(exe_path);
        cmd.arg("-m")
            .arg(model_path)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("-ngl")
            .arg(ngl)
            .arg("-c")
            .arg("2048")
            .arg("-np")
            .arg("1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        if let Some(lib_dir) = exe_path.parent() {
            #[cfg(target_os = "linux")]
            {
                let mut paths = vec![lib_dir.to_path_buf()];
                if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("LD_LIBRARY_PATH", joined);
                }
            }
            #[cfg(target_os = "macos")]
            {
                let mut paths = vec![lib_dir.to_path_buf()];
                if let Some(existing) = std::env::var_os("DYLD_LIBRARY_PATH") {
                    paths.extend(std::env::split_paths(&existing));
                }
                if let Ok(joined) = std::env::join_paths(paths) {
                    cmd.env("DYLD_LIBRARY_PATH", joined);
                }
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to launch llama-server ({}): {e}", exe_path.display()))?;

        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(20)));
        if let Some(stderr) = child.stderr.take() {
            let tail_clone = Arc::clone(&stderr_tail);
            let debug_log = std::env::var_os("LIVE_SUBTITLES_DEBUG_TRANSLATOR").is_some();
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if debug_log {
                        eprintln!("[llama-server] {line}");
                    }
                    if let Ok(mut q) = tail_clone.lock() {
                        if q.len() >= 20 {
                            q.pop_front();
                        }
                        q.push_back(line);
                    }
                }
            });
        }

        Ok((child, stderr_tail))
    }

    async fn wait_healthy(
        client: &reqwest::Client,
        port: u16,
        child: &Mutex<Child>,
        stderr_tail: &Mutex<VecDeque<String>>,
    ) -> Result<(), String> {
        let health_url = format!("http://127.0.0.1:{port}/health");
        let deadline = Instant::now() + Duration::from_secs(60);

        while Instant::now() < deadline {
            if let Ok(mut guard) = child.lock()
                && let Ok(Some(status)) = guard.try_wait()
            {
                let tail = stderr_tail
                    .lock()
                    .map(|q| q.iter().cloned().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let detail = if tail.is_empty() {
                    format!("exited with {status}")
                } else {
                    format!("exited with {status}: {tail}")
                };
                return Err(format!("llama-server {detail}"));
            }

            if let Ok(resp) = client.get(&health_url).send().await
                && resp.status().is_success()
            {
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        Err("Timed out waiting for Hy-MT3 llama-server /health".to_string())
    }

    pub async fn start(exe_path: &Path, model_path: &Path) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_default();

        for (attempt, ngl) in ["99", "0"].into_iter().enumerate() {
            let port = Self::pick_free_port()?;
            let (child, stderr_tail) = Self::spawn_child(exe_path, model_path, port, ngl)?;
            let server = Self {
                port,
                child: Mutex::new(child),
                stderr_tail,
                client: client.clone(),
            };

            match Self::wait_healthy(&server.client, server.port, &server.child, &server.stderr_tail).await {
                Ok(()) => {
                    eprintln!(
                        "[hy-mt3] llama-server ready on http://127.0.0.1:{port} (ngl={ngl})"
                    );
                    return Ok(server);
                }
                Err(err) => {
                    if attempt == 0 {
                        eprintln!("[hy-mt3] GPU launch failed ({err}); retrying with CPU (-ngl 0)");
                    } else {
                        return Err(format!("Failed to start Hy-MT3 server: {err}"));
                    }
                }
            }
        }

        Err("Failed to start Hy-MT3 server".to_string())
    }

    pub fn is_alive(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok())
            .is_some_and(|status| status.is_none())
    }

    pub async fn translate(
        &self,
        text: &str,
        target_lang: &str,
        context_lines: &[String],
    ) -> Result<String, String> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }

        let prompt = build_hy_mt3_prompt(trimmed, target_lang, context_lines);
        let body = json!({
            "messages": [
                { "role": "user", "content": prompt }
            ],
            "temperature": 0.2,
            "top_p": 0.6,
            "top_k": 20,
            "repeat_penalty": 1.05,
            "max_tokens": 256,
            "cache_prompt": true,
            "stream": false
        });
        let body_str = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        let url = format!("http://127.0.0.1:{}/v1/chat/completions", self.port);

        let resp = self
            .client
            .post(&url)
            .header("content-type", "application/json")
            .body(body_str)
            .send()
            .await
            .map_err(|e| format!("Hy-MT3 request failed: {e}"))?;

        let status = resp.status();
        let resp_text = resp
            .text()
            .await
            .map_err(|e| format!("Failed to read Hy-MT3 response: {e}"))?;

        if !status.is_success() {
            return Err(format!("Hy-MT3 HTTP {status}: {resp_text}"));
        }

        let payload: Value = serde_json::from_str(&resp_text)
            .map_err(|e| format!("Invalid Hy-MT3 JSON: {e}"))?;
        let raw_content = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");

        Ok(clean_hy_mt3_output(raw_content))
    }
}

impl Drop for HyMt3Server {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_preferred_hy_mt3_gguf_quant() {
        assert_eq!(
            select_hy_mt3_gguf_filename(&[
                "README.md",
                "Hy-MT3-1.8B-Q8_0.gguf",
                "Hy-MT3-1.8B-Q5_K_M.gguf",
                "Hy-MT3-1.8B-Q4_K_M.gguf",
                "mmproj-model.gguf",
            ]),
            Some("Hy-MT3-1.8B-Q4_K_M.gguf".to_string())
        );

        assert_eq!(
            select_hy_mt3_gguf_filename(&["Hy-MT3-1.8B-Q8_0.gguf", "Hy-MT3-1.8B-F16.gguf"]),
            Some("Hy-MT3-1.8B-Q8_0.gguf".to_string())
        );

        assert_eq!(
            select_hy_mt3_gguf_filename(&[
                "Hy-MT3-1.8B-F16-00001-of-00002.gguf",
                "Hy-MT3-1.8B-IQ4_XS.gguf",
            ]),
            Some("Hy-MT3-1.8B-IQ4_XS.gguf".to_string())
        );

        assert_eq!(select_hy_mt3_gguf_filename(&["mmproj-f16.gguf", "config.json"]), None);
    }

    #[test]
    fn selects_llama_release_asset_for_platform() {
        let assets = vec![
            (
                "llama-b9999-bin-ubuntu-vulkan-x64.tar.gz.sha256".to_string(),
                "https://example.com/vulkan.tar.gz.sha256".to_string(),
            ),
            (
                "llama-b9999-bin-ubuntu-x64.tar.gz".to_string(),
                "https://example.com/cpu.tar.gz".to_string(),
            ),
            (
                "llama-b9999-bin-ubuntu-vulkan-x64.tar.gz".to_string(),
                "https://example.com/vulkan.tar.gz".to_string(),
            ),
            (
                "llama-b9999-bin-macos-arm64.tar.gz".to_string(),
                "https://example.com/mac.tar.gz".to_string(),
            ),
            (
                "llama-b9999-bin-win-vulkan-x64.zip".to_string(),
                "https://example.com/win.zip".to_string(),
            ),
        ];

        assert_eq!(
            select_llama_release_asset(&assets, "linux", "x86_64").map(|a| a.0.as_str()),
            Some("llama-b9999-bin-ubuntu-vulkan-x64.tar.gz")
        );
        assert_eq!(
            select_llama_release_asset(&assets, "macos", "aarch64").map(|a| a.0.as_str()),
            Some("llama-b9999-bin-macos-arm64.tar.gz")
        );
        assert_eq!(
            select_llama_release_asset(&assets, "windows", "x86_64").map(|a| a.0.as_str()),
            Some("llama-b9999-bin-win-vulkan-x64.zip")
        );
        assert!(select_llama_release_asset(&assets, "freebsd", "x86_64").is_none());

        let zip_only = vec![(
            "llama-b9999-bin-ubuntu-vulkan-x64.zip".to_string(),
            "https://example.com/vulkan.zip".to_string(),
        )];
        assert_eq!(
            select_llama_release_asset(&zip_only, "linux", "x86_64").map(|a| a.0.as_str()),
            Some("llama-b9999-bin-ubuntu-vulkan-x64.zip")
        );
    }

    #[test]
    fn scans_recent_releases_skipping_drafts_or_empty_latest() {
        let releases = json!([
            {
                "draft": true,
                "assets": [
                    { "name": "llama-b10002-bin-ubuntu-vulkan-x64.tar.gz", "browser_download_url": "https://example.com/draft.tar.gz" }
                ]
            },
            {
                "draft": false,
                "assets": [
                    { "name": "llama-b10001-bin-macos-arm64.tar.gz", "browser_download_url": "https://example.com/mac.tar.gz" }
                ]
            },
            {
                "draft": false,
                "assets": [
                    { "name": "llama-b10000-bin-ubuntu-vulkan-x64.tar.gz", "browser_download_url": "https://example.com/ubuntu.tar.gz" }
                ]
            }
        ]);

        assert_eq!(
            select_llama_asset_from_releases(&releases, "linux", "x86_64"),
            Some((
                "llama-b10000-bin-ubuntu-vulkan-x64.tar.gz".to_string(),
                "https://example.com/ubuntu.tar.gz".to_string(),
            ))
        );
    }

    #[test]
    fn builds_hy_mt3_prompt_with_and_without_context() {
        let no_ctx = build_hy_mt3_prompt("おはよう", "en", &[]);
        assert!(no_ctx.starts_with("Translate the following text into English."));
        assert!(no_ctx.ends_with("\n\nおはよう"));

        let ctx = vec![
            "こんにちは".to_string(),
            " ".to_string(),
            "元気ですか？".to_string(),
            "はい、元気です。".to_string(),
            "それはよかった！".to_string(),
        ];
        let with_ctx = build_hy_mt3_prompt("今日も頑張ろう", "en", &ctx);
        assert!(!with_ctx.contains("こんにちは"));
        assert!(with_ctx.starts_with("元気ですか？\nはい、元気です。\nそれはよかった！\n"));
        assert!(with_ctx.contains("translate the following text into English"));
        assert!(with_ctx.ends_with("\n今日も頑張ろう"));
    }

    #[test]
    fn cleans_hy_mt3_output_whitespace_and_quotes() {
        assert_eq!(clean_hy_mt3_output("  \"Hello\n  world!\"  "), "Hello world!");
        assert_eq!(clean_hy_mt3_output("「Good morning」"), "Good morning");
        assert_eq!(clean_hy_mt3_output("Plain text"), "Plain text");
    }
}
