import {
  DEFAULT_SUBTITLE_STYLE,
  LINE_EXPIRE_MS,
  parsePortFromServerUrl,
  portToServerUrl,
  pushSubtitleLine,
  sanitizeSubtitleStyle,
  type ExtensionState,
  type OffscreenToBackgroundMessage,
  type PopupToBackgroundMessage,
  type SubtitleLine,
  type SubtitleStyle,
} from '../lib/messages';
import {
  DEFAULT_LANGUAGE,
  DEFAULT_MODEL_ID,
  DEFAULT_QUANT_ID,
  DEFAULT_SERVER_URL,
  DEFAULT_TARGET_LANGUAGE,
  buildVisibleModels,
  migrateLegacyModelSelection,
  pickVariantForModel,
  resolveModelSource,
  sanitizeCustomModels,
  sanitizeHiddenModelIds,
  sanitizeTargetLanguage,
} from '../lib/models';

export default defineBackground(() => {
  const state: ExtensionState = {
    status: 'idle',
    tabId: null,
    modelId: DEFAULT_MODEL_ID,
    quantId: DEFAULT_QUANT_ID,
    hiddenModelIds: [],
    customModels: [],
    language: DEFAULT_LANGUAGE,
    targetLanguage: DEFAULT_TARGET_LANGUAGE,
    showFurigana: true,
    enableDiarization: false,
    subtitleStyle: { ...DEFAULT_SUBTITLE_STYLE },
    serverUrl: DEFAULT_SERVER_URL,
    device: null,
    activity: '',
    progress: null,
    progressLabel: '',
    progressSpeedBps: null,
    downloadingFilename: null,
    downloadProgress: null,
    downloadLabel: '',
    downloadSpeedBps: null,
    error: null,
    lastText: '',
  };

  let subtitleLines: SubtitleLine[] = [];
  let nextLineId = 1;
  let downloadWs: WebSocket | null = null;
  let cancelActiveDownload: (() => void) | null = null;

  const settingsReady = chrome.storage.local
    .get({
      modelId: DEFAULT_MODEL_ID,
      quantId: DEFAULT_QUANT_ID,
      hiddenModelIds: [] as string[],
      customModels: [] as unknown[],
      customRepo: '',
      customFilename: '',
      language: DEFAULT_LANGUAGE,
      targetLanguage: DEFAULT_TARGET_LANGUAGE,
      showFurigana: true,
      enableDiarization: false,
      subtitleStyle: DEFAULT_SUBTITLE_STYLE,
      serverUrl: DEFAULT_SERVER_URL,
    })
    .then((saved) => {
      const sanitizedCustom = sanitizeCustomModels(saved.customModels);
      const migrated = migrateLegacyModelSelection(
        String(saved.modelId || DEFAULT_MODEL_ID),
        typeof saved.quantId === 'string' ? saved.quantId : undefined,
        String(saved.customRepo || ''),
        String(saved.customFilename || ''),
        sanitizedCustom,
      );
      const hidden = sanitizeHiddenModelIds(saved.hiddenModelIds, migrated.customModels);
      const visible = buildVisibleModels(hidden, migrated.customModels);
      const active = visible.find((m) => m.id === migrated.modelId) ?? visible[0]!;

      state.customModels = migrated.customModels;
      state.hiddenModelIds = hidden;
      state.modelId = active.id;
      state.quantId = pickVariantForModel(active, migrated.quantId).id;
      state.language = String(saved.language || DEFAULT_LANGUAGE);
      state.targetLanguage = sanitizeTargetLanguage(saved.targetLanguage);
      state.showFurigana = saved.showFurigana !== false;
      state.enableDiarization = Boolean(saved.enableDiarization);
      state.subtitleStyle = sanitizeSubtitleStyle(
        saved.subtitleStyle as Partial<SubtitleStyle> | undefined,
      );
      state.serverUrl = portToServerUrl(
        parsePortFromServerUrl(String(saved.serverUrl || DEFAULT_SERVER_URL)),
      );
    })
    .catch(() => {});

  function broadcastState(): void {
    chrome.runtime.sendMessage({ type: 'STATE_CHANGED', state }).catch(() => {});
  }

  async function sendSubtitleToTab(
    tabId: number,
    lines: SubtitleLine[],
    visible: boolean,
  ): Promise<void> {
    const payload = {
      type: 'SUBTITLE_UPDATE',
      lines,
      showFurigana: state.showFurigana,
      style: state.subtitleStyle,
      visible,
    };
    try {
      await chrome.tabs.sendMessage(tabId, payload);
    } catch {
      try {
        await chrome.scripting.executeScript({
          target: { tabId },
          files: ['content-scripts/content.js'],
        });
        await chrome.tabs.sendMessage(tabId, payload);
      } catch {
        // Tab may be closed or non-scriptable
      }
    }
  }

  async function ensureOffscreen(): Promise<void> {
    const contexts = await chrome.runtime.getContexts({
      contextTypes: [chrome.runtime.ContextType.OFFSCREEN_DOCUMENT],
    });
    if (contexts.length > 0) return;

    await chrome.offscreen.createDocument({
      url: chrome.runtime.getURL('/offscreen.html'),
      reasons: [chrome.offscreen.Reason.USER_MEDIA],
      justification: 'Capture tab audio and stream 16kHz PCM to the local Vulkan ASR server',
    });
  }

  function getTabStreamId(tabId: number): Promise<string> {
    return new Promise((resolve, reject) => {
      chrome.tabCapture.getMediaStreamId({ targetTabId: tabId }, (streamId) => {
        if (chrome.runtime.lastError || !streamId) {
          reject(new Error(chrome.runtime.lastError?.message ?? 'Failed to capture tab audio'));
          return;
        }
        resolve(streamId);
      });
    });
  }

  async function stopCapture(): Promise<void> {
    const previousTab = state.tabId;
    await chrome.runtime.sendMessage({ type: 'OFFSCREEN_STOP' }).catch(() => {});
    await chrome.offscreen.closeDocument().catch(() => {});
    if (previousTab !== null) {
      await sendSubtitleToTab(previousTab, [], false);
    }
    state.status = 'idle';
    state.tabId = null;
    state.activity = '';
    state.progress = null;
    state.progressLabel = '';
    state.progressSpeedBps = null;
    state.error = null;
    subtitleLines = [];
    broadcastState();
  }

  async function startCapture(
    msg: Extract<PopupToBackgroundMessage, { type: 'START_CAPTURE' }>,
  ): Promise<void> {
    await settingsReady;
    const serverUrl = portToServerUrl(
      parsePortFromServerUrl(msg.serverUrl || state.serverUrl || DEFAULT_SERVER_URL),
    );
    const enableDiarization = msg.enableDiarization ?? state.enableDiarization;
    const targetLanguage = sanitizeTargetLanguage(msg.targetLanguage ?? state.targetLanguage);

    state.modelId = msg.modelId;
    state.quantId = msg.quantId;
    state.language = msg.language;
    state.targetLanguage = targetLanguage;
    state.enableDiarization = enableDiarization;
    state.serverUrl = serverUrl;
    await chrome.storage.local
      .set({
        modelId: msg.modelId,
        quantId: msg.quantId,
        language: msg.language,
        targetLanguage,
        enableDiarization,
        serverUrl,
      })
      .catch(() => {});

    if (state.tabId !== null && state.tabId !== msg.tabId) {
      await sendSubtitleToTab(state.tabId, [], false);
    }

    state.status = 'loading';
    state.tabId = msg.tabId;
    state.activity = '';
    state.error = null;
    state.lastText = '';
    subtitleLines = [];
    state.progress = 0;
    state.progressLabel = 'Starting audio capture...';
    state.progressSpeedBps = null;
    broadcastState();

    try {
      await sendSubtitleToTab(msg.tabId, [], false);
      await ensureOffscreen();
      const streamId = await getTabStreamId(msg.tabId);
      const source =
        msg.source ?? resolveModelSource(msg.modelId, msg.quantId, state.customModels);
      const translate =
        Boolean(source.supportsDirectS2tt) &&
        targetLanguage === 'en' &&
        msg.language !== 'en';

      await chrome.runtime.sendMessage({
        type: 'OFFSCREEN_START',
        streamId,
        source,
        language: msg.language,
        translate,
        enableDiarization,
        serverUrl,
      });
    } catch (err) {
      state.status = 'error';
      state.error = err instanceof Error ? err.message : String(err);
      state.progress = null;
      state.progressLabel = '';
      state.progressSpeedBps = null;
      broadcastState();
    }
  }

  function startModelDownload(
    msg: Extract<PopupToBackgroundMessage, { type: 'DOWNLOAD_MODEL' }>,
  ): void {
    if (state.downloadingFilename !== null || downloadWs !== null) return;

    const filename = msg.source.filename?.trim();
    if (!filename) {
      state.error = 'Select a model before downloading.';
      broadcastState();
      return;
    }

    const serverUrl = portToServerUrl(
      parsePortFromServerUrl(msg.serverUrl || state.serverUrl || DEFAULT_SERVER_URL),
    );
    state.downloadingFilename = filename;
    state.downloadProgress = 0;
    state.downloadLabel = `Connecting to download ${filename}...`;
    state.downloadSpeedBps = null;
    state.error = null;
    broadcastState();

    let settled = false;
    let ws: WebSocket | null = null;
    const finish = (error: string | null) => {
      if (settled) return;
      settled = true;
      cancelActiveDownload = null;
      if (ws && downloadWs === ws) {
        downloadWs = null;
      }
      if (ws) {
        ws.onopen = null;
        ws.onmessage = null;
        ws.onerror = null;
        ws.onclose = null;
      }
      state.downloadingFilename = null;
      state.downloadProgress = null;
      state.downloadLabel = '';
      state.downloadSpeedBps = null;
      state.error = error;
      broadcastState();
      if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) {
        ws.close();
      }
    };

    cancelActiveDownload = () => {
      if (ws && ws.readyState === WebSocket.OPEN) {
        try {
          ws.send(JSON.stringify({ type: 'cancelDownload' }));
        } catch {
          // Ignore send failure during cancellation
        }
      }
      finish(null);
    };

    try {
      ws = new WebSocket(serverUrl);
      downloadWs = ws;
      ws.onopen = () => {
        ws?.send(
          JSON.stringify({
            type: 'downloadModel',
            filename,
            repo: msg.source.repo,
            url: msg.source.url,
          }),
        );
      };
      ws.onmessage = (ev) => {
        try {
          const data = JSON.parse(String(ev.data)) as {
            type?: string;
            progress?: number | null;
            label?: string;
            bytesPerSec?: number | null;
            error?: string;
          };
          if (data.type === 'progress') {
            state.downloadProgress = data.progress ?? null;
            state.downloadLabel = data.label ?? `Downloading ${filename}...`;
            state.downloadSpeedBps =
              typeof data.bytesPerSec === 'number' ? data.bytesPerSec : null;
            broadcastState();
          } else if (data.type === 'models') {
            finish(null);
          } else if (data.type === 'error') {
            finish(data.error ?? 'Download failed');
          }
        } catch {
          // Ignore malformed messages
        }
      };
      ws.onerror = () => {
        finish('Server offline — run: cd native && cargo run --release');
      };
      ws.onclose = () => {
        if (!settled) {
          finish('Download connection closed unexpectedly.');
        }
      };
    } catch (err) {
      finish(err instanceof Error ? err.message : 'Failed to start download.');
    }
  }

  chrome.runtime.onMessage.addListener(
    (
      message: PopupToBackgroundMessage | OffscreenToBackgroundMessage,
      _sender,
      sendResponse,
    ) => {
      if (message.type === 'GET_STATE') {
        (async () => {
          await settingsReady;
          if (state.status === 'loading' || state.status === 'listening') {
            const contexts = await chrome.runtime.getContexts({
              contextTypes: [chrome.runtime.ContextType.OFFSCREEN_DOCUMENT],
            });
            if (contexts.length === 0) {
              state.status = 'error';
              state.tabId = null;
              state.progress = null;
              state.progressLabel = '';
              state.progressSpeedBps = null;
              state.error = 'Audio capture worker closed unexpectedly.';
              broadcastState();
            }
          }
          sendResponse({ ...state });
        })();
        return true;
      }

      if (message.type === 'START_CAPTURE') {
        startCapture(message).then(() => {
          sendResponse({ ...state });
        });
        return true;
      }

      if (message.type === 'DOWNLOAD_MODEL') {
        startModelDownload(message);
        sendResponse({ ...state });
        return false;
      }

      if (message.type === 'CANCEL_DOWNLOAD') {
        cancelActiveDownload?.();
        sendResponse({ ...state });
        return false;
      }

      if (message.type === 'STOP_CAPTURE') {
        stopCapture().then(() => sendResponse({ ...state }));
        return true;
      }

      if (message.type === 'SET_FURIGANA') {
        state.showFurigana = message.enabled;
        void chrome.storage.local.set({ showFurigana: message.enabled }).catch(() => {});
        const now = Date.now();
        subtitleLines = subtitleLines.filter(
          (l) => !l.isFinal || now - l.updatedAt <= LINE_EXPIRE_MS,
        );
        if (state.status === 'listening' && state.tabId !== null && subtitleLines.length > 0) {
          void sendSubtitleToTab(state.tabId, subtitleLines, true);
        }
        broadcastState();
        return false;
      }

      if (message.type === 'SET_DIARIZATION') {
        state.enableDiarization = message.enabled;
        void chrome.storage.local
          .set({ enableDiarization: state.enableDiarization })
          .catch(() => {});
        if (state.status === 'listening' || state.status === 'loading') {
          void chrome.runtime
            .sendMessage({
              type: 'OFFSCREEN_SET_DIARIZATION',
              enabled: state.enableDiarization,
            })
            .catch(() => {});
        }
        broadcastState();
        return false;
      }

      if (message.type === 'SET_STYLE') {
        const clean = sanitizeSubtitleStyle(message.style);
        state.subtitleStyle = clean;
        void chrome.storage.local.set({ subtitleStyle: clean }).catch(() => {});
        const now = Date.now();
        subtitleLines = subtitleLines.filter(
          (l) => !l.isFinal || now - l.updatedAt <= LINE_EXPIRE_MS,
        );
        if (state.status === 'listening' && state.tabId !== null && subtitleLines.length > 0) {
          void sendSubtitleToTab(state.tabId, subtitleLines, true);
        }
        broadcastState();
        return false;
      }

      if (message.type === 'SET_SERVER_URL') {
        const normalizedUrl = portToServerUrl(parsePortFromServerUrl(message.serverUrl));
        state.serverUrl = normalizedUrl;
        void chrome.storage.local.set({ serverUrl: normalizedUrl }).catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'SET_MODEL_CATALOG') {
        const customModels = sanitizeCustomModels(message.customModels);
        const hiddenModelIds = sanitizeHiddenModelIds(message.hiddenModelIds, customModels);
        const visible = buildVisibleModels(hiddenModelIds, customModels);
        const active = visible.find((m) => m.id === state.modelId) ?? visible[0]!;

        state.customModels = customModels;
        state.hiddenModelIds = hiddenModelIds;
        state.modelId = active.id;
        state.quantId = pickVariantForModel(active, state.quantId).id;

        void chrome.storage.local
          .set({
            customModels,
            hiddenModelIds,
            modelId: state.modelId,
            quantId: state.quantId,
          })
          .catch(() => {});
        broadcastState();
        return false;
      }

      if (message.type === 'OFFSCREEN_STATUS') {
        const uiChanged =
          state.status !== message.status ||
          (message.device !== undefined && state.device !== message.device) ||
          (message.progress !== undefined && state.progress !== message.progress) ||
          (message.progressLabel !== undefined && state.progressLabel !== message.progressLabel) ||
          (message.speedBps !== undefined && state.progressSpeedBps !== message.speedBps) ||
          (message.error !== undefined && state.error !== message.error);
        state.status = message.status;
        if (message.device !== undefined) state.device = message.device;
        if (message.activity !== undefined) state.activity = message.activity;
        if (message.progress !== undefined) state.progress = message.progress;
        if (message.progressLabel !== undefined) state.progressLabel = message.progressLabel;
        if (message.speedBps !== undefined) state.progressSpeedBps = message.speedBps;
        if (message.error !== undefined) state.error = message.error;
        if (uiChanged) broadcastState();
        return false;
      }

      if (message.type === 'OFFSCREEN_TRANSCRIPT') {
        if (state.status !== 'listening' || state.tabId === null) return false;
        state.lastText = message.text;
        const pushed = pushSubtitleLine(
          subtitleLines,
          {
            text: message.text,
            tokens: message.tokens ?? [],
            speakers: message.speakers,
            isFinal: message.isFinal,
            updatedAt: Date.now(),
          },
          nextLineId,
        );
        subtitleLines = pushed.lines;
        nextLineId = pushed.nextId;
        sendSubtitleToTab(state.tabId, subtitleLines, true);
        return false;
      }

      return false;
    },
  );

  chrome.tabs.onRemoved.addListener((removedTabId) => {
    if (removedTabId === state.tabId) {
      stopCapture();
    }
  });
});
