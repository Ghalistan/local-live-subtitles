# Local Live Subtitles

Real-time speech transcription and translation subtitles captured directly from browser tab audio. The Chrome extension works with a native Rust server for local GPU inference or optional cloud transcription.

> **Browser Compatibility:** Currently **mainly tested on Google Chrome**. Other Chromium-based browsers that support Manifest V3 `tabCapture` and `offscreen` documents may work, though they are not actively tested.

## Features

- **100% Local GPU Inference:** Captures active tab audio at 16 kHz and transcribes locally on Vulkan (Linux/Windows) or Metal (macOS).
- **Local model selection.** Choose Whisper Large v3 (default) or the Japanese-only Kotoba Whisper v2.2. The extension fetches each model’s available GGML quantizations and file sizes from Hugging Face and caches the list locally. Installed variants stay selectable offline. Add other local or Hugging Face `.bin` models in **Settings**.
- **Cloud transcription.** Use your own API key for Google Gemini, Soniox, or ElevenLabs. The native server streams captured tab audio to the selected provider.
- **Direct speech-to-English translation (S2TT).** Whisper Large v3 and compatible custom models offer **Target Language → English (Direct S2TT)**.
- **Japanese Furigana:** Automatic kanji reading annotations powered by embedded `lindera` (IPADIC).
- **Streaming Speaker Diarization:** Optional ONNX speaker labeling (`Nemotron-3`) with stacked multi-speaker subtitle lines (up to 3 lines) when multiple people speak.
- **Customizable Overlay & Persistence:** Adjust font size, colors, and background opacity on the **Display** tab; model downloads run in the background extension service worker with `MB/s` speed, live progress, and cancel support.

---

## Quick Setup (Prebuilt Releases)

Download the Chrome extension zip and the desktop server archive for your OS from the [**Releases**](https://github.com/Ghalistan/local-live-subtitles/releases) page:

### 1. Start the Native Desktop Server

- **Linux (`local-live-subtitles-server-v*-linux-x86_64.tar.gz`):**
  ```bash
  tar -xzf local-live-subtitles-server-v0.1.0-linux-x86_64.tar.gz
  ./live-subtitles-native
  ```
  Starts in the background with a system tray icon (`Show / Hide`, `Clear History`, `Quit`) listening on `ws://127.0.0.1:8765`.
- **macOS (`local-live-subtitles-server-v*-macos-arm64.tar.gz`):**
  ```bash
  tar -xzf local-live-subtitles-server-v0.1.0-macos-arm64.tar.gz
  ./live-subtitles-native
  ```
- **Windows (`local-live-subtitles-server-v*-windows-x86_64.zip`):**
  Unzip the archive and run `live-subtitles-native.exe`.

> **Options:** Pass `--headless` to run without opening a desktop UI window. Override the default WebSocket bind address (`127.0.0.1:8765`) via `LIVE_SUBTITLES_ADDR` or in the desktop **Settings** panel (and match the Listen Port in the extension's **Settings** tab). Override model storage (`~/.cache/live-subtitles`) via `LIVE_SUBTITLES_CACHE_DIR`.

### 2. Install the Chrome Extension

1. Download and unzip `local-live-subtitles-extension-v*-chrome.zip` into a permanent folder.
2. Open Chrome and navigate to `chrome://extensions`.
3. Enable **Developer mode** (top right).
4. Click **Load unpacked** and select the unzipped extension folder.

### 3. Capture Live Subtitles

1. Open a tab with audio (e.g., YouTube or Twitch) and click the **Local Live Subtitles** extension icon.
2. Confirm the native server is online and choose a **Transcription Provider**. For **Local Whisper**, select a model and quantization level, then click **Download**. For a cloud provider, open **Settings → Cloud Provider API Keys**, select the provider, and save its API key. Return to **Model** to choose the provider for capture.
3. Choose the **Spoken Language** (and **Target Language** if supported), then click **Start Subtitles on Active Tab**.

Soniox and ElevenLabs use realtime speech-to-text. For ElevenLabs, choose a real-time model in **Model → ElevenLabs Model**. Use **Refresh** to update the list from ElevenLabs. To translate either provider’s subtitles, select **Hy-MT3** under **Translation** and download the local translator.

---

## Build from Source

### Prerequisites

- **Rust** (stable toolchain) & **CMake**
- **Bun** (`>= 1.2`)
- **Linux:** `pkg-config`, `libclang-dev`, `libvulkan-dev`, `glslc`, X11/Wayland/GTK3/DBus dev headers (see [release.yml](.github/workflows/release.yml) for the exact `apt` list)
- **Windows:** LunarG Vulkan SDK (`VULKAN_SDK`), LLVM (`LIBCLANG_PATH`), and MSVC Build Tools
- **macOS:** Xcode Command Line Tools (Metal enabled automatically)

### Commands

```bash
# 1. Start the native GPU server
cargo run --release --manifest-path native/Cargo.toml

# 2. Install, test, and build the Chrome extension
bun --cwd extension install
bun --cwd extension test
bun --cwd extension build   # outputs to extension/.output/chrome-mv3/
```

Load `extension/.output/chrome-mv3/` via `chrome://extensions` → **Load unpacked**.

---

## License

This project's source code is licensed under the [MIT License](LICENSE).

AI models downloaded on demand at runtime (including OpenAI Whisper GGML weights, Kotoba-Whisper, Silero VAD, and NVIDIA Nemotron-3 Diarization ONNX) are subject to their respective upstream model licenses.
