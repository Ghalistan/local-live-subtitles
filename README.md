# Local Live Subtitles

Real-time local speech transcription and translation subtitles captured directly from browser tab audio. All audio processing and AI inference run locally on your machine using a native Rust GPU server (`whisper.cpp` on **Vulkan** for Linux/Windows, **Metal** for macOS) paired with a lightweight Chrome extension overlay.

> **Browser Compatibility:** Currently **mainly tested on Google Chrome**. Other Chromium-based browsers that support Manifest V3 `tabCapture` and `offscreen` documents may work, though they are not actively tested.

## Features

- **100% Local GPU Inference:** Captures active tab audio at 16 kHz and transcribes locally on Vulkan (Linux/Windows) or Metal (macOS).
- **Flexible Model Catalog & Quantization Slider:** Pick a Whisper or Kotoba-Whisper model family and slide between **Fastest** (`Q4_K` / `Q5_0` / `Q5_1`) and **Best** (`FP16`), or customize the list and add local/Hugging Face `.bin` models in **Settings**.
- **Direct Speech-to-English Translation (S2TT):** Models with full decoders (e.g., Whisper Large v3, Medium, Small, Base) unlock a **Target Language → English (Direct S2TT)** option, detected ahead of time via preset metadata and GGML headers.
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
2. Confirm the server status pill says **Online**, select a model and quantization level, and click **Download**.
3. Choose the **Spoken Language** (and **Target Language** if supported), then click **Start Subtitles on Active Tab**.

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
